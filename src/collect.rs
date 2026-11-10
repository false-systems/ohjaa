//! `ohjaa collect`: what the human view cannot work out while a terminal
//! opens, gathered into one snapshot that `ohjaa line` and `ohjaa digest`
//! read. Two parts, each asked once, at the same time:
//!
//! - `github`: the pull requests updated in the owners of the fleet's clones
//!   under the roots (those with `.toimija/repo.yaml`), one search through
//!   `gh api graphql`, at most [`MAX_GH_CALLS`] calls;
//! - `secrets`: where vessa's secrets scan found secret-shaped tokens in
//!   agent sessions, and how many, never a value: one `vessa secrets --json`
//!   from where the last scan that read everything ended.
//!
//! The proposals are not in it: the view reads them itself, as it speaks, and `ohjaa digest` asks for the fleet itself. Each
//! part says when it was read, what it could not read, and, when it could
//! not answer, why; a part that failed keeps what it held before, as of
//! then.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::Dirs;
use crate::{gitconfig, home, intake, process};

pub const SCHEMA: &str = "ohjaa-collected.v1";

/// The most `gh` calls one collect makes: a second page, never a third.
pub const MAX_GH_CALLS: usize = 2;

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// vessa reads only the transcripts it is given, a fraction of a second
/// each; a vessa that hangs costs a collect this, never more.
const VESSA_TIMEOUT: Duration = Duration::from_secs(60);

const SEARCH: &str = "query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number title url createdAt updatedAt closedAt mergedAt baseRefName isDraft
        author { login }
        mergedBy { login }
        repository { nameWithOwner defaultBranchRef { name } }
      }
    }
  }
}";

/// The human view's folder in the state directory.
pub fn dir(dirs: &Dirs) -> PathBuf {
    dirs.state.join("human")
}

fn path(dirs: &Dirs) -> PathBuf {
    dir(dirs).join("collected.json")
}

/// Held by the one collect that runs.
pub fn lock_path(dirs: &Dirs) -> PathBuf {
    dir(dirs).join("collect.lock")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collected {
    pub schema: String,
    pub at_ms: i64,
    #[serde(default)]
    pub github: Part<Vec<Pr>>,
    /// Where vessa found secret-shaped tokens in agent sessions, and how
    /// many: never a value.
    #[serde(default)]
    pub secrets: Part<Secrets>,
}

impl Collected {
    /// What the snapshot keeps of the secrets found, only for the window the digest shows, from `from_ms`: never
    /// longer, and that holds what a read that failed keeps too.
    pub fn keep_window(&mut self, from_ms: i64) {
        self.secrets
            .value
            .findings
            .retain(|finding| finding.at_ms.unwrap_or(finding.found_ms) >= from_ms);
    }
}

/// What vessa's secrets scan found, and up to when it read everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secrets {
    /// When a scan last read everything it looked at: the next one begins
    /// there. `None`: none has yet.
    pub read_ms: Option<i64>,
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub undated_records: Option<u64>,
    #[serde(default)]
    pub other_issues: Option<u64>,
}

/// One place vessa found secret-shaped tokens: where and how many, never
/// the value (`vessa-secrets.v1`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub agent: String,
    /// The agent's own session id.
    pub session: String,
    pub cwd: Option<String>,
    pub at_ms: Option<i64>,
    /// `person`, `agent`, `tool_input`, `tool_output` or `other`.
    #[serde(rename = "where")]
    pub place: String,
    pub rule: String,
    pub count: u64,
    /// Measured subset in signature/encrypted fields; absent in older reports.
    #[serde(default)]
    pub opaque_count: Option<u64>,
    /// Explicit native-position verification; absent legacy context is unverified.
    #[serde(default)]
    pub opaque_native: Option<bool>,
    /// The first 8 hex characters of the token's SHA-256, when vessa gave
    /// one.
    pub fingerprint: Option<String>,
    /// When a collect first found it, by the snapshot's clock.
    pub found_ms: i64,
    /// Which of this machine's credentials it is, by where it is kept
    /// (`environment variable …`, `~/.aws/credentials`): a vessa that
    /// compares with them says so. None from an earlier vessa, whose
    /// findings were every secret-shaped string.
    #[serde(default)]
    pub held: Option<String>,
}

impl Finding {
    /// The same finding, read again, has the same key.
    pub fn key(&self) -> String {
        format!(
            "secret:{}:{}:{}:{}:{}:{}",
            self.agent,
            self.session,
            self.at_ms.map_or("-".to_owned(), |at| at.to_string()),
            self.place,
            self.rule,
            self.fingerprint.as_deref().unwrap_or("-")
        )
    }
}

/// A secrets scan begins this long before the last one that read
/// everything: a record written as that one read is read again, and is the
/// same finding.
pub const SECRETS_OVERLAP_MS: i64 = 5 * 60_000;

/// One source's answer, when it was read, what it could not read, and why
/// it did not answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Part<T> {
    /// When it last answered; `None`: never.
    pub as_of_ms: Option<i64>,
    /// What it covers from: GitHub's and vessa's window start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ms: Option<i64>,
    #[serde(default)]
    pub value: T,
    /// Why it did not answer this time: what it holds is as of `as_of_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What it could not read in an answer that otherwise stands: an entry
    /// that could not be marked, a page not asked for. Never an all-clear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<String>,
}

impl<T: Clone> Part<T> {
    fn read(answer: Answer<T>, before: &Part<T>, now_ms: i64, from_ms: Option<i64>) -> Part<T> {
        match answer {
            Ok((value, gaps)) => Part {
                as_of_ms: Some(now_ms),
                from_ms,
                value,
                error: None,
                gaps,
            },
            Err(why) => Part {
                as_of_ms: before.as_of_ms,
                from_ms: before.from_ms,
                value: before.value.clone(),
                error: Some(why),
                gaps: Vec::new(),
            },
        }
    }

    /// It did not answer, or could not read all it asked.
    pub fn failed(&self) -> bool {
        self.error.is_some() || !self.gaps.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pr {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<String>,
    pub created_ms: i64,
    pub merged_ms: Option<i64>,
    pub merged_by: Option<String>,
    pub closed_ms: Option<i64>,
    pub base: Option<String>,
    pub default_branch: Option<String>,
    /// Opened as a draft: work in progress. A snapshot from before ohjaa
    /// asked reads as not a draft.
    #[serde(default)]
    pub draft: bool,
    /// When GitHub last saw it change (a push, a comment, an edit); unknown
    /// in a snapshot from before ohjaa asked.
    #[serde(default)]
    pub updated_ms: Option<i64>,
}

impl Pr {
    /// Neither merged nor closed when GitHub was read.
    pub fn open(&self) -> bool {
        self.merged_ms.is_none() && self.closed_ms.is_none()
    }

    /// Merged into its repository's default branch (or one whose base is not
    /// known): a landing. A merge into another branch is not one.
    pub fn landed(&self) -> bool {
        self.merged_ms.is_some()
            && match (&self.base, &self.default_branch) {
                (Some(base), Some(default)) => base == default,
                _ => true,
            }
    }
}

/// The last snapshot, unless there is none or it cannot be read as one.
pub fn read(dirs: &Dirs) -> Option<Collected> {
    let text = std::fs::read_to_string(path(dirs)).ok()?;
    serde_json::from_str::<Collected>(&text)
        .ok()
        .filter(|collected| collected.schema == SCHEMA)
}

/// Replace the snapshot whole: a reader sees the old one or the new one.
pub fn write(dirs: &Dirs, collected: &Collected) -> Result<()> {
    let target = path(dirs);
    std::fs::create_dir_all(dir(dirs))
        .with_context(|| format!("cannot create {}", dir(dirs).display()))?;
    let partial = target.with_extension("json.partial");
    let mut text = serde_json::to_string_pretty(collected)?;
    text.push('\n');
    std::fs::write(&partial, text)
        .and_then(|()| std::fs::rename(&partial, &target))
        .with_context(|| format!("cannot write {}", target.display()))
}

/// Ask every source, each on its own thread, and keep what a failed one
/// held before. `repos` are the git repositories under the roots: GitHub is
/// asked about the owners of the fleet's among them. GitHub and vessa are
/// asked about what happened since `from_ms`.
pub fn collect(
    dirs: &Dirs,
    repos: Result<Vec<PathBuf>, String>,
    from_ms: i64,
    now_ms: i64,
) -> Collected {
    let before = read(dirs).unwrap_or_default();
    let mut collected = std::thread::scope(|scope| {
        let github = scope.spawn(|| {
            let (owners, mut gaps) = owners(&repos?);
            // No owner found, but not every clone could be read: whose pull
            // requests to ask for is unknown, never "none".
            if let (true, Some(first)) = (owners.is_empty(), gaps.first()) {
                return Err(format!(
                    "{} under the roots could not be read, so whose pull requests to ask \
                     GitHub for is unknown: {first}",
                    if gaps.len() == 1 {
                        "1 clone".to_owned()
                    } else {
                        format!("{} clones", gaps.len())
                    }
                ));
            }
            let (prs, more) = github(&owners, from_ms)?;
            gaps.extend(more);
            Ok((prs, gaps))
        });
        let secrets = scope.spawn(|| secrets(&before.secrets.value, from_ms, now_ms));
        Collected {
            schema: SCHEMA.to_owned(),
            at_ms: now_ms,
            github: Part::read(joined(github), &before.github, now_ms, Some(from_ms)),
            secrets: Part::read(joined(secrets), &before.secrets, now_ms, Some(from_ms)),
        }
    });
    collected.keep_window(from_ms);
    collected
}

pub type Answer<T> = Result<(T, Vec<String>), String>;

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Answer<T>>) -> Answer<T> {
    handle
        .join()
        .unwrap_or_else(|_| Err("its reader panicked".to_owned()))
}

/// The owners on GitHub of the fleet's clones under the roots, those that
/// declare themselves in `.toimija/repo.yaml` (as `ohjaa system` counts the
/// fleet), from each one's origin: whose pull requests to show. A clone of
/// someone else's project under a root is no part of the fleet, so its
/// owner never fills the search. Each clone's config is read once (one `git
/// config`), all at the same time; one that cannot be read is said beside
/// them.
fn owners(repos: &[PathBuf]) -> (Vec<String>, Vec<String>) {
    let fleet: Vec<&PathBuf> = repos
        .iter()
        .filter(|repo| repo.join(".toimija/repo.yaml").is_file())
        .collect();
    let read: Vec<Result<Option<String>, String>> = std::thread::scope(|scope| {
        fleet
            .iter()
            .map(|repo| {
                scope.spawn(move || {
                    let listing =
                        gitconfig::clone_listing(repo).map_err(|error| format!("{error:#}"))?;
                    Ok(gitconfig::fetch_url(&listing, "origin")
                        .as_deref()
                        .and_then(home::RepoId::parse)
                        .filter(|id| id.host.as_deref() == Some("github.com"))
                        .map(|id| id.owner.to_ascii_lowercase()))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("its reader panicked".to_owned()))
            })
            .collect()
    });
    let mut owners = BTreeSet::new();
    let mut gaps = Vec::new();
    for answer in read {
        match answer {
            Ok(owner) => owners.extend(owner),
            Err(why) => gaps.push(why),
        }
    }
    (owners.into_iter().collect(), gaps)
}

/// The search: every pull request in `owners` updated since `from_ms` (to
/// the minute), the last updated first.
pub fn search(owners: &[String], from_ms: i64) -> String {
    let from = intake::rfc3339((from_ms - from_ms.rem_euclid(60_000)).div_euclid(1000));
    let orgs: Vec<String> = owners.iter().map(|owner| format!("org:{owner}")).collect();
    format!(
        "{} is:pr updated:>={from} sort:updated-desc",
        orgs.join(" ")
    )
}

fn github(owners: &[String], from_ms: i64) -> Result<(Vec<Pr>, Vec<String>), String> {
    if owners.is_empty() {
        return Ok((
            Vec::new(),
            vec![
                "no clone of the fleet under the roots (one with .toimija/repo.yaml) has its origin on \
                 GitHub, so no owner to ask it about"
                    .to_owned(),
            ],
        ));
    }
    let search = search(owners, from_ms);
    let mut prs = Vec::new();
    let mut gaps = Vec::new();
    let mut after: Option<String> = None;
    for call in 1..=MAX_GH_CALLS {
        let found = gh_page(&search, after.as_deref())?;
        let field = |name: &str| found.get(name).cloned().unwrap_or(Value::Null);
        if let Some(nodes) = field("nodes").as_array() {
            prs.extend(nodes.iter().filter_map(pr));
        }
        let page = field("pageInfo");
        if page["hasNextPage"] != Value::Bool(true) {
            break;
        }
        if call == MAX_GH_CALLS {
            gaps.push(format!(
                "{} pull requests were updated since {}; only the {} updated last were read",
                field("issueCount").as_u64().unwrap_or(0),
                intake::clock(from_ms),
                prs.len()
            ));
            break;
        }
        after = page["endCursor"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    Ok((prs, gaps))
}

fn gh_page(search: &str, after: Option<&str>) -> Result<Value, String> {
    let mut command = Command::new("gh");
    command
        .args(["api", "graphql", "-f"])
        .arg(format!("query={SEARCH}"))
        .arg("-f")
        .arg(format!("q={search}"));
    if let Some(after) = after {
        command.arg("-f").arg(format!("after={after}"));
    }
    // gh writes state of its own when it checks for updates; asked not to.
    command
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("GH_PROMPT_DISABLED", "1");
    let output =
        process::run(&mut command, None, GH_TIMEOUT).map_err(|error| format!("{error:#}"))?;
    if !output.status.success() {
        // gh leads with its reason ("To get started with GitHub CLI, …").
        let stderr = String::from_utf8_lossy(&output.stderr);
        let why = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("exited {}", output.status));
        return Err(format!("gh api graphql: {why}"));
    }
    let answer: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh api graphql printed no answer: {error}"))?;
    if let Some(message) = answer["errors"][0]["message"].as_str() {
        return Err(format!("GitHub refused the search: {message}"));
    }
    match &answer["data"]["search"] {
        Value::Object(_) => Ok(answer["data"]["search"].clone()),
        _ => Err("GitHub answered without a search".to_owned()),
    }
}

/// A search hit that is a pull request; anything else is skipped.
fn pr(node: &Value) -> Option<Pr> {
    let text = |value: &Value| value.as_str().map(str::to_owned);
    let time = |name: &str| node[name].as_str().and_then(intake::parse_time);
    Some(Pr {
        repo: text(&node["repository"]["nameWithOwner"])?,
        number: node["number"].as_u64()?,
        title: text(&node["title"]).unwrap_or_default(),
        url: text(&node["url"]).unwrap_or_default(),
        author: text(&node["author"]["login"]),
        created_ms: time("createdAt")?,
        merged_ms: time("mergedAt"),
        merged_by: text(&node["mergedBy"]["login"]),
        closed_ms: time("closedAt"),
        base: text(&node["baseRefName"]),
        default_branch: text(&node["repository"]["defaultBranchRef"]["name"]),
        draft: node["isDraft"].as_bool().unwrap_or(false),
        updated_ms: time("updatedAt"),
    })
}

/// Why vessa could not be run for `what`.
pub fn vessa_failed(what: &str, error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    if text.contains("No such file or directory") {
        "vessa is not installed (not on PATH)".to_owned()
    } else {
        format!("vessa {what}: {text}")
    }
}

/// What vessa said went wrong: clap's `error:` line when it refused the
/// command (it ends a usage error with a hint), else its last line.
fn said_on_stderr(output: &process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("error:"))
        .map_or_else(|| output.failure_detail(), str::to_owned)
}

fn exit_of(code: Option<i32>) -> String {
    code.map_or("a signal".to_owned(), |code| code.to_string())
}

// ---- the secrets seen ---------------------------------------------------------

/// The form of `vessa secrets --json` a collect reads.
const VESSA_SECRETS: &str = "vessa-secrets.v1";

/// What vessa's secrets scan finds in agent sessions since the last scan
/// that read everything (`vessa secrets --json --since`), joined to what was
/// found before: where, and how many, never a value.
fn secrets(before: &Secrets, from_ms: i64, now_ms: i64) -> Answer<Secrets> {
    let since = intake::rfc3339(secrets_since(before, from_ms).div_euclid(1000));
    let mut command = Command::new("vessa");
    command.args(["secrets", "--json", "--since"]).arg(&since);
    let output = process::run(&mut command, None, VESSA_TIMEOUT)
        .map_err(|error| vessa_failed("secrets", &error))?;
    read_secrets(
        output.status.code(),
        &output.stdout,
        &said_on_stderr(&output),
        before,
        now_ms,
    )
}

/// Where the next secrets scan begins: where the last one that read
/// everything ended, a little before, and never before `from_ms`.
pub fn secrets_since(before: &Secrets, from_ms: i64) -> i64 {
    before
        .read_ms
        .map_or(from_ms, |read| read - SECRETS_OVERLAP_MS)
        .max(from_ms)
}

/// vessa's secrets scan (`vessa-secrets.v1`; its exit `code`, and `detail`,
/// what it said on stderr), joined to what was found `before`. A scan that
/// could not read everything (exit 4, `complete: false`, a path in
/// `unreadable`) stands for what it found, says what it could not read, and
/// leaves where the next begins where it was. An older vessa, an exit of
/// another kind, or an answer that is not the report is no answer.
pub fn read_secrets(
    code: Option<i32>,
    stdout: &[u8],
    detail: &str,
    before: &Secrets,
    now_ms: i64,
) -> Answer<Secrets> {
    let exit = exit_of(code);
    if !matches!(code, Some(0 | 4)) {
        return Err(if detail.contains("unrecognized subcommand") {
            format!("this vessa has no `secrets`: update vessa (exit {exit}: {detail})")
        } else {
            format!("vessa secrets --json: exit {exit}: {detail}")
        });
    }
    let report: Value = serde_json::from_slice(stdout).map_err(|error| {
        format!("vessa secrets --json printed no report (exit {exit}): {error}")
    })?;
    if report["schema"] != VESSA_SECRETS {
        return Err(format!(
            "vessa answered {}, not {VESSA_SECRETS}",
            report["schema"].as_str().unwrap_or("without a schema")
        ));
    }
    let Some(complete) = report["complete"].as_bool() else {
        return Err("vessa's secrets report does not say whether it read everything".to_owned());
    };
    let Some(listed) = report["findings"].as_array() else {
        return Err("vessa's secrets report lists no findings".to_owned());
    };
    let unreadable = report["unreadable"]
        .as_array()
        .ok_or_else(|| "vessa's secrets report has no valid unreadable-path list".to_owned())?;
    let mut gaps: Vec<String> = unreadable
        .iter()
        .map(|unread| {
            format!(
                "could not read {}: {}",
                unread["path"].as_str().unwrap_or("?"),
                unread["why"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let whole = code == Some(0) && complete && gaps.is_empty();
    let undated_records = report
        .get("undated_records")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .filter(|count| *count <= unreadable.len() as u64)
                .ok_or_else(|| "vessa's undated-record count is invalid".to_owned())
        })
        .transpose()?;
    if !whole && gaps.is_empty() {
        gaps.push(match code {
            Some(4) if !detail.is_empty() => {
                format!("vessa says its scan did not read everything: {detail}")
            }
            _ => "vessa says its scan did not read everything".to_owned(),
        });
    }
    // This scan's findings, each once however often it is listed.
    let mut read: Vec<Finding> = Vec::new();
    for listed in listed {
        let finding = finding(listed, now_ms).ok_or_else(|| {
            "vessa's secrets report names a finding without its agent, session, where, rule \
             or count"
                .to_owned()
        })?;
        match read.iter_mut().find(|same| same.key() == finding.key()) {
            Some(same) => {
                same.count = same
                    .count
                    .checked_add(finding.count)
                    .ok_or("vessa's finding counts overflow")?;
                same.opaque_count = match (same.opaque_count, finding.opaque_count) {
                    (Some(a), Some(b)) => {
                        Some(a.checked_add(b).ok_or("vessa's context counts overflow")?)
                    }
                    _ => None,
                };
                same.opaque_native = same
                    .opaque_native
                    .zip(finding.opaque_native)
                    .map(|(a, b)| a && b);
            }
            None => read.push(finding),
        }
    }
    // A vessa that compares with this machine's credentials (it says what it
    // compared with) lists only those as findings: what an earlier one
    // listed of every other secret-shaped string is no finding, and goes.
    let compares = report["compared_with"].is_array();
    let mut findings: Vec<Finding> = before
        .findings
        .iter()
        .filter(|kept| !compares || kept.held.is_some())
        .cloned()
        .collect();
    for finding in read {
        match findings.iter_mut().find(|same| same.key() == finding.key()) {
            // Read again: as it is now, found when it first was.
            Some(same) => {
                *same = Finding {
                    found_ms: same.found_ms,
                    ..finding
                }
            }
            None => findings.push(finding),
        }
    }
    let read_ms = if whole { Some(now_ms) } else { before.read_ms };
    let other_issues = undated_records.map(|undated| gaps.len() as u64 - undated);
    Ok((
        Secrets {
            read_ms,
            findings,
            undated_records,
            other_issues,
        },
        gaps,
    ))
}

/// One finding of the report. What should be a name and is not is not kept
/// as given (`other`, `unknown`, no fingerprint), so that whatever vessa
/// printed, no value is kept.
fn finding(listed: &Value, now_ms: i64) -> Option<Finding> {
    let text = |name: &str| listed[name].as_str().map(str::to_owned);
    let place = text("where")?;
    let rule = text("rule")?;
    let named = !rule.is_empty()
        && rule.len() <= 32
        && rule.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    let count = listed["count"].as_u64()?;
    let opaque_count = match listed.get("opaque_count") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().filter(|n| *n <= count)?),
    };
    let opaque_native = match listed.get("opaque_native") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_bool()?),
    };
    Some(Finding {
        agent: text("agent")?,
        session: text("session")?,
        cwd: text("cwd"),
        at_ms: listed["at"].as_str().and_then(intake::parse_time),
        place: match place.as_str() {
            "person" | "agent" | "tool_input" | "tool_output" | "other" => place,
            _ => "other".to_owned(),
        },
        rule: if named { rule } else { "unknown".to_owned() },
        count,
        opaque_count,
        opaque_native,
        fingerprint: text("fingerprint")
            .filter(|fp| fp.len() == 8 && fp.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|fp| fp.to_ascii_lowercase()),
        found_ms: now_ms,
        // A name, as vessa scrubbed it: where the credential is kept.
        held: text("held").filter(|held| {
            (1..=120).contains(&held.chars().count()) && !held.chars().any(char::is_control)
        }),
    })
}

/// Where GitHub and vessa are read from: where the next digest begins, by
/// GitHub's clock (and a little before, for what is indexed late), at least
/// a day back, at most a week.
pub fn from_ms(next_digest_ms: Option<i64>, now_ms: i64) -> i64 {
    let day = 86_400_000;
    next_digest_ms
        .map(|at| at - 2 * 3_600_000)
        .unwrap_or(now_ms - day)
        .clamp(now_ms - 7 * day, now_ms - day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn the_search_names_every_owner_and_the_minute() {
        let from = intake::parse_time("2026-10-12T08:30:45Z").unwrap();
        assert_eq!(
            search(&["example-org".into(), "devuser".into()], from),
            "org:example-org org:devuser is:pr updated:>=2026-10-12T08:30:00Z \
             sort:updated-desc"
        );
    }

    #[test]
    fn a_pull_request_says_whether_it_is_a_draft_and_when_it_last_changed() {
        let node = json!({
            "number": 12,
            "title": "Rename the config module",
            "url": "https://github.com/example-org/carpet/pull/12",
            "createdAt": "2026-11-08T08:00:00Z",
            "updatedAt": "2026-11-08T09:40:00Z",
            "isDraft": true,
            "author": { "login": "sam" },
            "repository": { "nameWithOwner": "example-org/carpet" },
        });
        let read = pr(&node).unwrap();
        assert!(read.draft && read.open());
        assert_eq!(read.updated_ms, intake::parse_time("2026-11-08T09:40:00Z"));
        // A snapshot written before ohjaa asked reads as not a draft, its
        // last change unknown, and still open.
        let mut old = serde_json::to_value(&read).unwrap();
        old.as_object_mut().unwrap().remove("draft");
        old.as_object_mut().unwrap().remove("updated_ms");
        let old: Pr = serde_json::from_value(old).unwrap();
        assert!(!old.draft && old.open());
        assert_eq!(old.updated_ms, None);
        let merged = Pr {
            merged_ms: Some(1),
            ..read
        };
        assert!(!merged.open());
    }

    #[test]
    fn a_failed_part_keeps_what_it_held_as_of_then() {
        let before = Part {
            as_of_ms: Some(5),
            from_ms: Some(1),
            value: vec![1],
            error: None,
            gaps: vec!["old".into()],
        };
        let failed = Part::read(Err("gh: no network".into()), &before, 9, Some(8));
        assert_eq!((failed.as_of_ms, failed.from_ms), (Some(5), Some(1)));
        assert_eq!(failed.value, vec![1]);
        assert_eq!(failed.error.as_deref(), Some("gh: no network"));
        assert!(failed.failed());
        let answered = Part::read(Ok((vec![2], Vec::new())), &before, 9, Some(8));
        assert_eq!((answered.as_of_ms, &answered.value), (Some(9), &vec![2]));
        assert!(!answered.failed());
        let partly = Part::read(Ok((vec![3], vec!["a page".into()])), &before, 9, None);
        assert!(partly.failed(), "a gap is no all-clear");
    }

    #[test]
    fn a_pull_request_merged_elsewhere_is_no_landing() {
        let hit = json!({
            "number": 7, "title": "t", "url": "u", "createdAt": "2026-10-26T10:00:00Z",
            "mergedAt": "2026-10-26T11:00:00Z", "baseRefName": "feat/x",
            "repository": {"nameWithOwner": "o/r", "defaultBranchRef": {"name": "main"}},
        });
        let stacked = pr(&hit).unwrap();
        assert!(!stacked.landed());
        let mut into_main = hit.clone();
        into_main["baseRefName"] = json!("main");
        assert!(pr(&into_main).unwrap().landed());
        // Not a pull request: an empty hit.
        assert_eq!(pr(&json!({})), None);
    }

    // ---- the secrets seen ----

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    #[test]
    fn malformed_scan_coverage_does_not_advance_the_complete_scan() {
        let before = Secrets {
            read_ms: Some(NOW - HOUR),
            findings: Vec::new(),
            ..Secrets::default()
        };
        for unreadable in [
            Value::Null,
            json!({"path":"/bad", "why":"denied"}),
            json!("unreadable"),
        ] {
            let report = json!({"schema":"vessa-secrets.v1", "complete":true, "findings":[], "unreadable":unreadable});
            assert!(
                read_secrets(Some(0), report.to_string().as_bytes(), "", &before, NOW).is_err()
            );
        }
        assert_eq!(before.read_ms, Some(NOW - HOUR));
    }

    fn scan(complete: bool, unreadable: Value, findings: Value) -> Vec<u8> {
        json!({"schema": "vessa-secrets.v1", "since": null, "complete": complete,
               "unreadable": unreadable, "findings": findings})
        .to_string()
        .into_bytes()
    }

    fn found(session: &str, at_ms: i64, place: &str, rule: &str, fingerprint: Value) -> Value {
        json!({"agent": "claude", "session": session, "cwd": "/p/carpet",
               "at": intake::rfc3339(at_ms / 1000), "where": place, "rule": rule,
               "count": 1, "fingerprint": fingerprint})
    }

    /// A vessa that compares with this machine's credentials lists only
    /// those: what an earlier one listed of every other secret-shaped string
    /// goes from the snapshot, and each kept finding says where it is held.
    #[test]
    fn a_report_of_held_credentials_replaces_an_earlier_vessas_findings() {
        let earlier = found("old", NOW - HOUR, "tool_output", "mixed_token", Value::Null);
        let (before, _) = read_secrets(
            Some(0),
            &scan(true, json!([]), json!([earlier])),
            "",
            &Secrets::default(),
            NOW,
        )
        .unwrap();
        assert_eq!(before.findings[0].held, None);
        let mut mine = found("new", NOW, "person", "id_and_secret", Value::Null);
        mine["held"] = json!("environment variable ZAI_API_KEY");
        let mut report: Value =
            serde_json::from_slice(&scan(true, json!([]), json!([mine]))).unwrap();
        report["compared_with"] = json!([{"source": "environment variable ZAI_API_KEY",
                                         "status": "read", "credentials": 1}]);
        let (after, _) = read_secrets(
            Some(0),
            report.to_string().as_bytes(),
            "",
            &before,
            NOW + 60_000,
        )
        .unwrap();
        assert_eq!(after.findings.len(), 1, "{:?}", after.findings);
        assert_eq!(
            after.findings[0].held.as_deref(),
            Some("environment variable ZAI_API_KEY")
        );
        // An earlier vessa's report keeps what came before, as it did.
        let (kept, _) = read_secrets(
            Some(0),
            &scan(true, json!([]), json!([])),
            "",
            &before,
            NOW + 60_000,
        )
        .unwrap();
        assert_eq!(kept.findings.len(), 1);
    }

    #[test]
    fn secret_context_counts_are_preserved_validated_and_unknown_for_older_reports() {
        let mut first = found("context", NOW, "agent", "mixed_token", Value::Null);
        first["count"] = json!(2);
        first["opaque_count"] = json!(1);
        first["opaque_native"] = json!(true);
        let report = json!({"schema":"vessa-secrets.v1", "complete":false,
            "undated_records":1, "unreadable":[{"path":"/synthetic/session.jsonl","why":"finding has no valid time"}],
            "findings":[first.clone(), first.clone()]});
        let (read, gaps) = read_secrets(
            Some(4),
            report.to_string().as_bytes(),
            "",
            &Secrets::default(),
            NOW,
        )
        .unwrap();
        let value = serde_json::to_value(read).unwrap();
        assert_eq!(value["findings"][0]["count"], 4);
        assert_eq!(value["findings"][0]["opaque_count"], 2);
        assert_eq!(value["findings"][0]["opaque_native"], true);
        assert_eq!(value["undated_records"], 1);
        assert_eq!(value["other_issues"], 0);
        assert!(value["read_ms"].is_null());
        assert_eq!(gaps.len(), 1);
        for bad in [json!(3), json!("1"), json!(-1)] {
            let mut invalid = report.clone();
            invalid["findings"][0]["opaque_count"] = bad;
            assert!(read_secrets(
                Some(4),
                invalid.to_string().as_bytes(),
                "",
                &Secrets::default(),
                NOW
            )
            .is_err());
        }
        let mut invalid = report.clone();
        invalid["undated_records"] = json!(2);
        assert!(read_secrets(
            Some(4),
            invalid.to_string().as_bytes(),
            "",
            &Secrets::default(),
            NOW
        )
        .is_err());
        let mut invalid = report.clone();
        invalid["findings"][0]["opaque_native"] = json!("true");
        assert!(read_secrets(
            Some(4),
            invalid.to_string().as_bytes(),
            "",
            &Secrets::default(),
            NOW
        )
        .is_err());
        let old = scan(
            true,
            json!([]),
            json!([found("old", NOW, "agent", "mixed_token", Value::Null)]),
        );
        let (read, _) = read_secrets(Some(0), &old, "", &Secrets::default(), NOW).unwrap();
        let value = serde_json::to_value(read).unwrap();
        assert!(value["findings"][0]["opaque_count"].is_null());
        assert!(value["undated_records"].is_null());
        let mut legacy_context = first;
        legacy_context
            .as_object_mut()
            .unwrap()
            .remove("opaque_native");
        let old = scan(true, json!([]), json!([legacy_context]));
        let (read, _) = read_secrets(Some(0), &old, "", &Secrets::default(), NOW).unwrap();
        assert_eq!(read.findings[0].opaque_count, Some(1));
        assert_eq!(read.findings[0].opaque_native, None);
        let mut unknown = report;
        unknown["undated_records"] = Value::Null;
        unknown["findings"][1]["opaque_count"] = Value::Null;
        let (read, _) = read_secrets(
            Some(4),
            unknown.to_string().as_bytes(),
            "",
            &Secrets::default(),
            NOW,
        )
        .unwrap();
        assert_eq!(read.findings[0].count, 4);
        assert_eq!(read.findings[0].opaque_count, None);
        assert_eq!(read.undated_records, None);
    }

    #[test]
    fn secrets_are_kept_as_where_and_how_many_and_read_on_from_the_last_whole_scan() {
        let typed = found("s1", NOW - HOUR, "person", "github", json!("1A2B3C4D"));
        let printed = found("s1", NOW - 2 * HOUR, "tool_output", "aws", Value::Null);
        let (first, gaps) = read_secrets(
            Some(0),
            &scan(true, json!([]), json!([typed, printed])),
            "",
            &Secrets::default(),
            NOW,
        )
        .unwrap();
        assert!(gaps.is_empty(), "{gaps:?}");
        assert_eq!(first.read_ms, Some(NOW));
        assert_eq!(first.findings.len(), 2);
        let said = &first.findings[0];
        assert_eq!(
            (said.place.as_str(), said.rule.as_str(), said.count),
            ("person", "github", 1)
        );
        assert_eq!(said.at_ms, Some(NOW - HOUR));
        assert_eq!(said.fingerprint.as_deref(), Some("1a2b3c4d"));
        assert_eq!(said.found_ms, NOW);
        // The next scan begins where this one ended, a little before, and
        // never before the window.
        assert_eq!(
            secrets_since(&first, NOW - 24 * HOUR),
            NOW - SECRETS_OVERLAP_MS
        );
        assert_eq!(secrets_since(&first, NOW), NOW);
        assert_eq!(
            secrets_since(&Secrets::default(), NOW - 24 * HOUR),
            NOW - 24 * HOUR
        );
        // Read again: the same one is one, found when it was first; one
        // more is new.
        let later = found("s2", NOW + 60_000, "agent", "jwt", Value::Null);
        let (again, _) = read_secrets(
            Some(0),
            &scan(true, json!([]), json!([typed, later])),
            "",
            &first,
            NOW + 15 * 60_000,
        )
        .unwrap();
        assert_eq!(again.findings.len(), 3);
        let found_at: Vec<i64> = again.findings.iter().map(|f| f.found_ms).collect();
        assert_eq!(found_at, vec![NOW, NOW, NOW + 15 * 60_000]);
        // Nothing that could be a value is kept as given: a fingerprint
        // that is not 8 hex digits, a rule or a place that is not a name.
        let odd = json!([{"agent": "claude", "session": "s3", "cwd": null, "at": null,
                          "where": "sk-ant-api03-abc", "rule": "sk-ant-api03-abc def",
                          "count": 2, "fingerprint": "sk-ant-api03"}]);
        let (kept, _) = read_secrets(
            Some(0),
            &scan(true, json!([]), odd),
            "",
            &Secrets::default(),
            NOW,
        )
        .unwrap();
        let odd = &kept.findings[0];
        assert_eq!(
            (odd.place.as_str(), odd.rule.as_str(), &odd.fingerprint),
            ("other", "unknown", &None)
        );
        assert_eq!((odd.at_ms, odd.count), (None, 2));
    }

    #[test]
    fn a_secrets_scan_that_could_not_read_everything_holds_its_place_and_says_why() {
        let before = Secrets {
            read_ms: Some(NOW - HOUR),
            findings: Vec::new(),
            ..Secrets::default()
        };
        let unreadable = json!([{"path": "/h/.claude/projects/-p/x.jsonl",
                                 "why": "Permission denied (os error 13)"}]);
        let finding = found("s1", NOW - 60_000, "person", "github", Value::Null);
        let (partial, gaps) = read_secrets(
            Some(4),
            &scan(false, unreadable, json!([finding])),
            "vessa: 1 transcript could not be read",
            &before,
            NOW,
        )
        .unwrap();
        assert_eq!(
            partial.read_ms,
            Some(NOW - HOUR),
            "the next scan begins where the last whole one ended"
        );
        assert_eq!(partial.findings.len(), 1, "what it found stands");
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(
            gaps[0].contains("/h/.claude/projects/-p/x.jsonl")
                && gaps[0].contains("Permission denied"),
            "{gaps:?}"
        );
        // Incomplete with nothing named, or exit 4 alone: no all-clear either.
        for (code, complete) in [(Some(0), false), (Some(4), true)] {
            let (kept, gaps) = read_secrets(
                code,
                &scan(complete, json!([]), json!([])),
                "",
                &before,
                NOW,
            )
            .unwrap();
            assert!(!gaps.is_empty(), "{code:?} {complete}");
            assert_eq!(kept.read_ms, Some(NOW - HOUR));
        }
        // An older vessa, a report that is not one, another schema, another
        // exit: no answer, and why.
        let old = read_secrets(
            Some(2),
            b"",
            "error: unrecognized subcommand 'secrets'",
            &before,
            NOW,
        )
        .unwrap_err();
        assert!(
            old.contains("no `secrets`") && old.contains("update vessa"),
            "{old}"
        );
        let broken = read_secrets(
            Some(0),
            b"{\"schema\": \"vessa-secrets.v1\", \"findings\": [",
            "",
            &before,
            NOW,
        )
        .unwrap_err();
        assert!(broken.contains("no report"), "{broken}");
        let other = json!({"schema": "vessa-secrets.v2", "complete": true, "findings": []});
        assert!(read_secrets(Some(0), other.to_string().as_bytes(), "", &before, NOW).is_err());
        let unsaid = json!({"schema": "vessa-secrets.v1", "findings": []});
        assert!(
            read_secrets(Some(0), unsaid.to_string().as_bytes(), "", &before, NOW).is_err(),
            "a report that does not say whether it is complete is none"
        );
        let nameless = scan(
            true,
            json!([]),
            json!([{"agent": "claude", "session": "s"}]),
        );
        assert!(read_secrets(Some(0), &nameless, "", &before, NOW).is_err());
        let failed = read_secrets(Some(1), b"", "vessa: disk on fire", &before, NOW).unwrap_err();
        assert!(failed.contains("disk on fire"), "{failed}");
        // vessa is not installed.
        let missing = vessa_failed(
            "secrets",
            &anyhow::anyhow!("cannot run vessa: No such file or directory (os error 2)"),
        );
        assert!(missing.contains("vessa is not installed"), "{missing}");
    }

    #[test]
    fn secrets_are_kept_only_for_the_window_the_digest_shows() {
        let from = NOW - 24 * HOUR;
        let mut collected = Collected::default();
        let finding = |at_ms: Option<i64>, found_ms: i64| Finding {
            agent: "claude".into(),
            session: "s".into(),
            place: "person".into(),
            rule: "github".into(),
            count: 1,
            at_ms,
            found_ms,
            ..Finding::default()
        };
        collected.secrets.value.findings = vec![
            finding(Some(from - 1), NOW),
            finding(None, from - 1),
            finding(Some(from + 1), NOW),
            finding(None, from + 1),
        ];
        collected.keep_window(from);
        let kept: Vec<(Option<i64>, i64)> = collected
            .secrets
            .value
            .findings
            .iter()
            .map(|f| (f.at_ms, f.found_ms))
            .collect();
        assert_eq!(kept, vec![(Some(from + 1), NOW), (None, from + 1)]);
    }

    #[test]
    fn github_and_vessa_read_since_the_next_digest_a_day_to_a_week() {
        let day = 86_400_000;
        let now = 100 * day;
        assert_eq!(from_ms(None, now), now - day);
        assert_eq!(
            from_ms(Some(now - 3 * day), now),
            now - 3 * day - 2 * 3_600_000
        );
        assert_eq!(from_ms(Some(now - 30 * day), now), now - 7 * day);
        assert_eq!(from_ms(Some(now - 60_000), now), now - day);
    }
}
