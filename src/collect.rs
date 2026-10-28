//! `ohjaa collect`: what the human view cannot work out while a terminal
//! opens, gathered into one snapshot that `ohjaa line` and `ohjaa digest`
//! read. Three parts, each asked once, at the same time:
//!
//! - `local`: the marks on each entry's branch and the open sessions as ohjaa
//!   finds them now (the marks' git and sykli, and `ps`), from
//!   [`report::build_marks`];
//! - `github`: the pull requests updated in the entries' owners, one search
//!   through `gh api graphql`, at most [`MAX_GH_CALLS`] calls;
//! - `usage`: what vessa reads in the transcripts of the sessions ended
//!   lately, joined by the agent's own session ids ohjaa found at leaving.
//!
//! The session log and the proposals are not in it: the view reads them
//! itself, as it speaks, and `ohjaa digest` asks for the fleet itself. Each
//! part says when it was read, what it could not read, and, when it could
//! not answer, why; a part that failed keeps what it held before, as of
//! then, and so does a branch whose marks could not be read, and every
//! branch when the entries themselves could not be listed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::Dirs;
use crate::sessions::{self, Check, Session};
use crate::{home, intake, mark, process, report, usage};

pub const SCHEMA: &str = "ohjaa-collected.v1";

/// The most `gh` calls one collect makes: a second page, never a third.
pub const MAX_GH_CALLS: usize = 2;

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// vessa reads only the transcripts it is given, a fraction of a second
/// each; a vessa that hangs costs a collect this, never more.
const VESSA_TIMEOUT: Duration = Duration::from_secs(60);

/// The most sessions whose usage one collect asks vessa about, newest first.
const MAX_COSTED: usize = 50;

const SEARCH: &str = "query($q: String!, $after: String) {
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    issueCount
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        number title url createdAt closedAt mergedAt baseRefName
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
    pub local: Part<Local>,
    #[serde(default)]
    pub github: Part<Vec<Pr>>,
    /// By ohjaa session id.
    #[serde(default)]
    pub usage: Part<BTreeMap<String, Cost>>,
    /// The first words the person typed in each session ended lately, by
    /// ohjaa session id: their own words, shown back to them, kept only for
    /// the window the digest shows ([`Collected::keep_window`]).
    #[serde(default)]
    pub typed: Part<BTreeMap<String, Typed>>,
    /// Where vessa found secret-shaped tokens in agent sessions, and how
    /// many: never a value.
    #[serde(default)]
    pub secrets: Part<Secrets>,
}

impl Collected {
    /// What the snapshot keeps of the person's words and of the secrets
    /// found, only for the window the digest shows, from `from_ms`: never
    /// longer, and that holds what a read that failed keeps too.
    pub fn keep_window(&mut self, from_ms: i64) {
        let _ = from_ms;
    }
}

/// The first words a person typed in a session, cut at a word to about
/// [`EXCERPT_CHARS`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typed {
    pub text: String,
    /// When its session ended: it is kept while that is in the window.
    pub ended_ms: i64,
}

/// What vessa's secrets scan found, and up to when it read everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secrets {
    /// When a scan last read everything it looked at: the next one begins
    /// there. `None`: none has yet.
    pub read_ms: Option<i64>,
    pub findings: Vec<Finding>,
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
    /// The first 8 hex characters of the token's SHA-256, when vessa gave
    /// one.
    pub fingerprint: Option<String>,
    /// When a collect first found it, by the snapshot's clock.
    pub found_ms: i64,
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

/// One message a person wrote, as `vessa messages --json` lists it: what
/// the first words of a session are taken from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub agent: String,
    pub session: String,
    pub at_ms: i64,
    pub text: String,
}

/// The first words typed in a session are cut at a word to about this many
/// characters, `…` included.
pub const EXCERPT_CHARS: usize = 70;

/// At most this many vessa calls one collect makes: `usage`, `messages` and
/// `secrets`, each once.
pub const MAX_VESSA_CALLS: usize = 3;

/// A secrets scan begins this long before the last one that read
/// everything: a record written as that one read is read again, and is the
/// same finding.
pub const SECRETS_OVERLAP_MS: i64 = 5 * 60_000;

/// `text` with its runs of white space made one space, cut at a word to at
/// most [`EXCERPT_CHARS`], with `…` when cut. `None`: nothing in it.
pub fn excerpt(text: &str) -> Option<String> {
    let _ = text;
    None
}

/// vessa's list of what people wrote (`vessa-messages.v1`; its exit `code`,
/// and `detail`, what it said on stderr), with what it could not list.
pub fn read_messages(
    code: Option<i32>,
    stdout: &[u8],
    detail: &str,
) -> Result<(Vec<Heard>, Vec<String>), String> {
    let _ = (code, stdout, detail);
    Ok((Vec::new(), Vec::new()))
}

/// The first words typed in each of `sessions`, by its own agent's
/// sessions, from what vessa listed after `since_ms`.
pub fn first_typed(
    sessions: &[&Session],
    heard: &[Heard],
    since_ms: i64,
) -> BTreeMap<String, Typed> {
    let _ = (sessions, heard, since_ms);
    BTreeMap::new()
}

/// Where the next secrets scan begins: where the last one that read
/// everything ended, a little before, and never before `from_ms`.
pub fn secrets_since(before: &Secrets, from_ms: i64) -> i64 {
    let _ = before;
    from_ms
}

/// vessa's secrets scan (`vessa-secrets.v1`; its exit `code`, and `detail`,
/// what it said on stderr), joined to what was found `before`.
pub fn read_secrets(
    code: Option<i32>,
    stdout: &[u8],
    detail: &str,
    before: &Secrets,
    now_ms: i64,
) -> Answer<Secrets> {
    let _ = (code, stdout, detail, now_ms);
    Ok((before.clone(), Vec::new()))
}

/// Why vessa could not be run for `what`.
pub fn vessa_failed(what: &str, error: &anyhow::Error) -> String {
    format!("vessa {what}: {error:#}")
}

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

/// What ohjaa works out here: the marks, and the open sessions now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Local {
    pub branches: Vec<Branch>,
    /// Open sessions whose ohjaa is gone while their process group runs on.
    #[serde(default)]
    pub gone: Vec<String>,
    /// Open sessions whose ohjaa and process group are both gone.
    #[serde(default)]
    pub orphaned: Vec<String>,
}

/// An entry's branch, as its marks say, tip first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    /// `owner/name`.
    pub slug: String,
    pub branch: String,
    /// Where this clone last saw origin's copy of it.
    pub tip: Option<String>,
    /// The commits above the first proven one, and of them how many failed
    /// and how many sykli could say nothing about.
    pub unproven: usize,
    pub failed: usize,
    pub unknown: usize,
    /// The oldest of them.
    pub oldest: Option<String>,
    /// The walk stopped before a proven commit: more may lie below.
    pub more: bool,
    /// When its marks were last read: what it says is as of then.
    #[serde(default)]
    pub as_of_ms: Option<i64>,
    /// Why its marks could not be read this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
}

impl Pr {
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

/// What vessa estimates one ohjaa session used: unknown (`None`) unless
/// every one of the agent's own sessions it ran was found, and an estimate
/// only when vessa priced all of it. An estimate, never a bill.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Input, output and cache writes.
    pub tokens: Option<u64>,
    /// Said apart: most of a session's tokens are reads of its cache.
    #[serde(default)]
    pub cache_reads: Option<u64>,
    pub usd_micros: Option<u64>,
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
/// held before. `repos` are the git repositories under the roots (for the
/// marks); GitHub and vessa are asked about what happened since `from_ms`.
pub fn collect(
    dirs: &Dirs,
    repos: Result<Vec<PathBuf>, String>,
    from_ms: i64,
    now_ms: i64,
) -> Collected {
    let before = read(dirs).unwrap_or_default();
    let owners = owners();
    let known = &before.local.value.branches;
    std::thread::scope(|scope| {
        let local = scope.spawn(|| repos.map(|repos| local(dirs, &repos, known, now_ms)));
        let github = scope.spawn(|| github(&owners?, from_ms));
        let usage = scope.spawn(|| usage(dirs, from_ms));
        Collected {
            schema: SCHEMA.to_owned(),
            at_ms: now_ms,
            local: Part::read(joined(local), &before.local, now_ms, None),
            github: Part::read(joined(github), &before.github, now_ms, Some(from_ms)),
            usage: Part::read(joined(usage), &before.usage, now_ms, Some(from_ms)),
            typed: Part::default(),
            secrets: Part::default(),
        }
    })
}

pub type Answer<T> = Result<(T, Vec<String>), String>;

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Answer<T>>) -> Answer<T> {
    handle
        .join()
        .unwrap_or_else(|_| Err("its reader panicked".to_owned()))
}

/// The marks and the open sessions, as `ohjaa report --marks` finds them;
/// what it could not read is said beside them, and a branch whose marks
/// could not be read keeps what `known` said of it.
fn local(dirs: &Dirs, repos: &[PathBuf], known: &[Branch], now_ms: i64) -> (Local, Vec<String>) {
    let report = report::build_marks(dirs, repos, now_ms - report::WEEK_MS, now_ms);
    let branches = branches(&report.marks, report.unlisted.as_deref(), known, now_ms);
    let open = |pick: fn(&report::Untold) -> bool| {
        report
            .open
            .iter()
            .filter(|s| pick(s))
            .map(|s| s.id.clone())
            .collect()
    };
    let local = Local {
        branches,
        gone: open(|s| s.ohjaa_gone),
        orphaned: open(|s| s.orphaned),
    };
    (local, report.gaps)
}

/// Each entry's branch now. When the entries themselves could not be listed
/// (`unlisted`, why), no entry's absence says anything: every branch known
/// stays as it was last read, with why it was not read this time.
pub fn branches(
    marks: &[(String, Result<mark::Marks, String>)],
    unlisted: Option<&str>,
    known: &[Branch],
    now_ms: i64,
) -> Vec<Branch> {
    match unlisted {
        Some(why) => known
            .iter()
            .map(|last| Branch {
                error: Some(why.to_owned()),
                ..last.clone()
            })
            .collect(),
        None => marks
            .iter()
            .map(|(slug, marked)| branch(slug, marked, known, now_ms))
            .collect(),
    }
}

/// An entry's branch now, or, when its marks cannot be read, as it was last
/// read: a branch that failed stays failed until a read says otherwise.
pub fn branch(
    slug: &str,
    marked: &Result<mark::Marks, String>,
    known: &[Branch],
    now_ms: i64,
) -> Branch {
    let marks = match marked {
        Ok(marks) => marks,
        Err(why) => {
            let last = known
                .iter()
                .find(|b| b.slug == slug && b.as_of_ms.is_some())
                .cloned();
            return Branch {
                slug: slug.to_owned(),
                error: Some(why.clone()),
                ..last.unwrap_or_default()
            };
        }
    };
    let unproven = marks.unproven();
    let count = |is: fn(&Check) -> bool| unproven.iter().filter(|w| is(&w.check)).count();
    Branch {
        slug: slug.to_owned(),
        branch: marks.gate.branch.clone(),
        tip: marks.commits.first().map(|w| w.commit.clone()),
        unproven: unproven.len(),
        failed: count(|c| matches!(c, Check::Failed { .. })),
        unknown: count(|c| matches!(c, Check::Unverified { .. })),
        oldest: unproven.last().map(|w| w.commit.clone()),
        more: matches!(marks.ended, mark::Ended::Cap | mark::Ended::NotAsked),
        as_of_ms: Some(now_ms),
        error: None,
    }
}

/// The owners of the workflow home's entries: whose pull requests to show.
fn owners() -> Result<Vec<String>, String> {
    let home = home::ohjaa_home().map_err(|error| format!("{error:#}"))?;
    let entries = home::entry_dirs(&home).map_err(|error| format!("{error:#}"))?;
    let owners: BTreeSet<String> = entries
        .iter()
        .filter_map(|(slug, _)| slug.split_once('/').map(|(owner, _)| owner.to_owned()))
        .collect();
    Ok(owners.into_iter().collect())
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
            vec!["the workflow home has no entries, so no owner to ask GitHub about".to_owned()],
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
                sessions::clock(from_ms),
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
    })
}

/// The usage of the sessions that ended since `from_ms` and named their
/// agent's own sessions, from those sessions' transcripts alone: vessa is
/// given each transcript, never asked to read them all.
fn usage(dirs: &Dirs, from_ms: i64) -> Result<(BTreeMap<String, Cost>, Vec<String>), String> {
    let log = sessions::Reader::open(&sessions::store_path(&dirs.state))
        .map_err(|error| format!("the session log could not be opened: {error}"))?;
    let (mut logged, _) = log
        .all()
        .map_err(|error| format!("the session log could not be read: {error}"))?;
    drop(log);
    logged.retain(|s| {
        s.left.as_ref().is_some_and(|left| {
            left.ended_ms > from_ms && left.native.as_ref().is_some_and(|n| !n.sessions.is_empty())
        })
    });
    logged.sort_by_key(|s| std::cmp::Reverse(s.left.as_ref().map(|left| left.ended_ms)));
    logged.truncate(MAX_COSTED);
    let transcripts: BTreeSet<&str> = logged
        .iter()
        .flat_map(natives)
        .filter(|n| has_transcript(n))
        .map(|n| n.source.as_str())
        .collect();
    if transcripts.is_empty() {
        return Ok((BTreeMap::new(), Vec::new()));
    }
    let mut command = Command::new("vessa");
    command.args(["usage", "--json"]);
    for transcript in &transcripts {
        command.arg("--input").arg(transcript);
    }
    let output =
        process::run(&mut command, None, VESSA_TIMEOUT).map_err(|error| format!("{error:#}"))?;
    // 4 is vessa's "partial": the numbers it printed stand, and it says why.
    let code = output.status.code();
    if !matches!(code, Some(0 | 4)) {
        return Err(format!("vessa usage --json: {}", output.failure_detail()));
    }
    let answer: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("vessa usage --json printed no report: {error}"))?;
    let mut gaps = Vec::new();
    if code == Some(4) {
        gaps.push("vessa says its usage evidence is partial".to_owned());
    }
    let read = read_usage(&answer, &mut gaps)?;
    let costs = logged
        .iter()
        .map(|s| (s.id.clone(), cost_of(s, &read)))
        .collect();
    Ok((costs, gaps))
}

fn natives(session: &Session) -> impl Iterator<Item = &usage::NativeSession> {
    session
        .left
        .iter()
        .filter_map(|left| left.native.as_ref())
        .flat_map(|native| native.sessions.iter())
}

/// Found in a transcript, not only claimed by the agent's environment.
fn has_transcript(native: &usage::NativeSession) -> bool {
    !native.testimony && native.source != usage::ENV_SOURCE && !native.source.is_empty()
}

/// What vessa read of one native session, as a [`Cost`]. `None`: its id is
/// in vessa's answer twice, so it is no one's.
type Read = BTreeMap<(String, String), Option<Cost>>;

/// All of them, or unknown when one is.
fn sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0u64, |total, value| total.checked_add(value?))
}

/// `vessa-usage.v1`, by agent and native session id.
pub fn read_usage(answer: &Value, gaps: &mut Vec<String>) -> Result<Read, String> {
    match answer["schema"].as_str() {
        Some("vessa-usage.v1") => {}
        other => {
            return Err(format!(
                "vessa answered {}, not vessa-usage.v1",
                other.unwrap_or("without a schema")
            ))
        }
    }
    let mut read = Read::new();
    for session in answer["sessions"].as_array().into_iter().flatten() {
        let (Some(agent), Some(id)) = (session["agent"].as_str(), session["id"].as_str()) else {
            return Err("vessa's usage names a session without its agent or id".to_owned());
        };
        for why in session["limitations"].as_array().into_iter().flatten() {
            gaps.push(format!("{agent} {id}: {}", why.as_str().unwrap_or("?")));
        }
        // A session's usage is its subagents' too.
        let entries = match session["totals_with_subagents"].as_array() {
            Some(entries) if !entries.is_empty() => entries,
            _ => session["totals"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        };
        let all = |names: &[&str]| {
            (!entries.is_empty())
                .then(|| {
                    sum(entries
                        .iter()
                        .map(|entry| sum(names.iter().map(|name| entry[*name].as_u64()))))
                })
                .flatten()
        };
        let cost = Cost {
            tokens: all(&["input_tokens", "output_tokens", "cache_write_tokens"]),
            cache_reads: all(&["cache_read_tokens"]),
            usd_micros: all(&["estimated_usd_micros"]),
        };
        let key = (agent.to_owned(), id.to_owned());
        if read.insert(key, Some(cost)).is_some() {
            gaps.push(format!(
                "{agent} {id} is in vessa's usage twice: no usage is joined to it"
            ));
            read.insert((agent.to_owned(), id.to_owned()), None);
        }
    }
    Ok(read)
}

/// The cost of one ohjaa session: known only when each of its agent's own
/// sessions has a transcript and vessa read it once.
pub fn cost_of(session: &Session, read: &Read) -> Cost {
    let found: Vec<Option<&Cost>> = natives(session)
        .map(|native| {
            has_transcript(native)
                .then(|| read.get(&(native.agent.clone(), native.session_id.clone())))
                .flatten()
                .and_then(Option::as_ref)
        })
        .collect();
    let total = |of: fn(&Cost) -> Option<u64>| {
        found
            .iter()
            .try_fold(0u64, |total, cost| total.checked_add(of((*cost)?)?))
    };
    let tokens = total(|c| c.tokens);
    let cache_reads = total(|c| c.cache_reads).filter(|_| tokens.is_some());
    Cost {
        usd_micros: total(|c| c.usd_micros).filter(|_| tokens.is_some()),
        cache_reads,
        tokens,
    }
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

    fn session(id: &str, natives: Value) -> Session {
        serde_json::from_value(json!({
            "id": id, "agent": "claude", "repo": "/r/ohjaa", "intent": "x",
            "started_ms": 1_000, "delivered": [],
            "left": {
                "ended_ms": 2_000, "exit": {"how": "code", "code": 0},
                "finished": {"kind": "no_pull_request"},
                "native": {"sessions": natives},
            },
        }))
        .unwrap()
    }

    fn native(agent: &str, id: &str, source: &str) -> Value {
        json!({"agent": agent, "session_id": id, "source": source})
    }

    fn vessa(sessions: Value) -> Value {
        json!({"schema": "vessa-usage.v1", "sessions": sessions, "warnings": []})
    }

    /// `tokens` of input and `cached` cache reads.
    fn entry(tokens: u64, cached: u64, usd: Option<u64>) -> Value {
        json!({"input_tokens": tokens, "output_tokens": 0, "cache_read_tokens": cached,
               "cache_write_tokens": 0, "estimated_usd_micros": usd})
    }

    fn cost(tokens: Option<u64>, cache_reads: Option<u64>, usd_micros: Option<u64>) -> Cost {
        Cost {
            tokens,
            cache_reads,
            usd_micros,
        }
    }

    #[test]
    fn the_search_names_every_owner_and_the_minute() {
        let from = intake::parse_time("2026-10-12T08:30:45Z").unwrap();
        assert_eq!(
            search(&["false-systems".into(), "yairfalse".into()], from),
            "org:false-systems org:yairfalse is:pr updated:>=2026-10-12T08:30:00Z \
             sort:updated-desc"
        );
    }

    #[test]
    fn a_session_costs_what_vessa_read_of_each_of_its_own_sessions() {
        let s = session(
            "oh-1",
            json!([
                native("claude", "c1", "/t/c1.jsonl"),
                native("codex", "x1", "/t/x1.jsonl")
            ]),
        );
        let answer = vessa(json!([
            {"agent": "claude", "id": "c1", "totals": [entry(1_000, 9, None)],
             "totals_with_subagents": [entry(1_500, 90_000, None)]},
            {"agent": "codex", "id": "x1", "totals": [entry(500, 10_000, None)]},
        ]));
        let read = read_usage(&answer, &mut Vec::new()).unwrap();
        // A session's usage is its subagents' too; cache reads apart;
        // nothing priced, no estimate.
        assert_eq!(cost_of(&s, &read), cost(Some(2_000), Some(100_000), None));
    }

    #[test]
    fn usage_never_joins_another_agent_an_ambiguous_id_or_a_claim() {
        let answer = vessa(json!([
            {"agent": "codex", "id": "c1", "totals": [entry(1_000, 0, Some(5))]},
            {"agent": "claude", "id": "dup", "totals": [entry(1, 0, Some(1))]},
            {"agent": "claude", "id": "dup", "totals": [entry(2, 0, Some(2))]},
        ]));
        let mut gaps = Vec::new();
        let read = read_usage(&answer, &mut gaps).unwrap();
        assert!(gaps
            .iter()
            .any(|n| n.contains("dup") && n.contains("twice")));
        let unknown = Cost::default();
        // Another agent's session of the same id is not this one.
        let other = session("oh-1", json!([native("claude", "c1", "/t/c1.jsonl")]));
        assert_eq!(cost_of(&other, &read), unknown);
        let twice = session("oh-2", json!([native("claude", "dup", "/t/d.jsonl")]));
        assert_eq!(cost_of(&twice, &read), unknown);
        // Only the agent's environment named it: no transcript was read.
        let mut claimed = native("codex", "c1", usage::ENV_SOURCE);
        claimed["testimony"] = json!(true);
        assert_eq!(cost_of(&session("oh-3", json!([claimed])), &read), unknown);
    }

    #[test]
    fn missing_or_partly_priced_usage_stays_unknown_never_zero() {
        let answer = vessa(json!([
            {"agent": "claude", "id": "a", "totals": [entry(10, 1, Some(7)), entry(5, 1, None)]},
            {"agent": "claude", "id": "b", "totals": []},
            {"agent": "claude", "id": "c", "totals": [entry(3, 4, Some(2))]},
        ]));
        let read = read_usage(&answer, &mut Vec::new()).unwrap();
        let of = |ids: &[&str]| {
            let natives: Vec<Value> = ids
                .iter()
                .map(|id| native("claude", id, &format!("/t/{id}.jsonl")))
                .collect();
            cost_of(&session("oh", json!(natives)), &read)
        };
        assert_eq!(
            of(&["a"]),
            cost(Some(15), Some(2), None),
            "a part unpriced is no estimate"
        );
        assert_eq!(of(&["b"]), Cost::default(), "no totals is unknown");
        assert_eq!(of(&["c", "missing"]), Cost::default(), "one not found");
        assert_eq!(of(&["c"]), cost(Some(3), Some(4), Some(2)));
        let wrong = json!({"schema": "vessa-other.v1", "sessions": []});
        assert!(read_usage(&wrong, &mut Vec::new()).is_err());
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

    /// Review of #63, finding 2: a branch whose marks cannot be read stays
    /// as it was last read, with its time and why, until a read says
    /// otherwise.
    #[test]
    fn a_branch_that_cannot_be_read_stays_as_it_was_last_read() {
        let known = vec![Branch {
            slug: "false-systems/habitat".into(),
            branch: "main".into(),
            tip: Some("86bd44c0".into()),
            unproven: 3,
            failed: 1,
            oldest: Some("1a2b3c4d".into()),
            as_of_ms: Some(5_000),
            ..Branch::default()
        }];
        let unread = Err("git: cannot run".to_owned());
        let kept = branch("false-systems/habitat", &unread, &known, 9_000);
        assert_eq!(
            (kept.unproven, kept.failed, kept.as_of_ms),
            (3, 1, Some(5_000))
        );
        assert_eq!(kept.error.as_deref(), Some("git: cannot run"));
        let never = branch("false-systems/valvo", &unread, &known, 9_000);
        assert_eq!((never.unproven, never.as_of_ms), (0, None));
        assert!(never.error.is_some());
    }

    /// Second review of #63, finding 1: when the entries cannot be listed
    /// (or the workflow home cannot be found), every branch known stays as
    /// it was last read, with why; none is dropped, and no pseudo-entry
    /// takes their place.
    #[test]
    fn every_branch_stays_as_last_read_when_the_entries_cannot_be_listed() {
        let known = vec![
            Branch {
                slug: "false-systems/sykli".into(),
                branch: "main".into(),
                tip: Some("5fcdeef0".into()),
                unproven: 1,
                failed: 1,
                oldest: Some("5fcdeef0".into()),
                as_of_ms: Some(5_000),
                ..Branch::default()
            },
            Branch {
                slug: "false-systems/ohjaa".into(),
                branch: "main".into(),
                as_of_ms: Some(5_000),
                ..Branch::default()
            },
        ];
        let why = "the entries cannot be listed: cannot read /h/repos/false-systems";
        let kept = branches(&[], Some(why), &known, 9_000);
        assert_eq!(kept.len(), 2);
        for (kept, last) in kept.iter().zip(&known) {
            assert_eq!(
                (&kept.slug, kept.unproven, kept.failed, kept.as_of_ms),
                (&last.slug, last.unproven, last.failed, Some(5_000))
            );
            assert_eq!(kept.error.as_deref(), Some(why));
        }
        // Listed: an entry gone from the home is gone from the marks.
        let listed = vec![(
            "false-systems/ohjaa".to_owned(),
            Err::<mark::Marks, _>("git: cannot run".to_owned()),
        )];
        let now = branches(&listed, None, &known, 9_000);
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].slug, "false-systems/ohjaa");
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

    // ---- the first words typed, and the secrets seen (Yair, 2026-10-28) ----

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    /// A session that ran from `start` to `end`, with its agent's own
    /// sessions `natives`.
    fn ran(id: &str, start: i64, end: i64, natives: Value) -> Session {
        let mut session = session(id, natives);
        session.started_ms = start;
        session.left.as_mut().unwrap().ended_ms = end;
        session
    }

    fn heard(agent: &str, session: &str, at_ms: Option<i64>, text: &str) -> Value {
        json!({"agent": agent, "session": session, "parent": null, "kind": "message",
               "id": null, "at": at_ms.map(intake::rfc3339_ms), "cwd": "/r/ohjaa",
               "text": text, "question": null, "reply": null,
               "source": "/t/x.jsonl", "line": 1})
    }

    fn listed(messages: Value, warnings: Value) -> Vec<u8> {
        json!({"schema": "vessa-messages.v1", "messages": messages, "sessions": [],
               "coverage": {"gaps": [], "limitations": []}, "warnings": warnings,
               "redacted": 0, "redacted_by_rule": {}})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn the_first_words_typed_are_cut_at_a_word_to_about_seventy_characters() {
        assert_eq!(
            excerpt("  fix   the\n\tparser  ").as_deref(),
            Some("fix the parser")
        );
        assert_eq!(excerpt(" \n\t "), None);
        let long = "make the digest show the first thing I typed in each session, not \
                    its intent, which nobody reads";
        let cut = excerpt(long).unwrap();
        assert!(cut.chars().count() <= EXCERPT_CHARS, "{cut}");
        let kept = cut.strip_suffix('…').expect("said cut");
        assert!(
            long.starts_with(kept) && long[kept.len()..].starts_with(' '),
            "cut at a word: {cut}"
        );
        assert!(kept.chars().count() > EXCERPT_CHARS - 15, "about 70: {cut}");
        // A word longer than all the room is cut inside it.
        let word = excerpt(&"x".repeat(200)).unwrap();
        assert_eq!(word.chars().count(), EXCERPT_CHARS);
        assert!(word.ends_with('…'));
    }

    #[test]
    fn each_session_shows_the_first_words_typed_in_it_by_its_own_agent() {
        let start = NOW - 3 * HOUR;
        let end = NOW - HOUR;
        let own = ran(
            "oh-1",
            start,
            end,
            json!([native("claude", "c1", "/t/c1.jsonl")]),
        );
        let mut claim = native("claude", "c9", usage::ENV_SOURCE);
        claim["testimony"] = json!(true);
        let claimed = ran("oh-2", start, end, json!([claim]));
        let quiet = ran(
            "oh-3",
            start,
            end,
            json!([native("claude", "c3", "/t/c3.jsonl")]),
        );
        let document = listed(
            json!([
                heard(
                    "claude",
                    "c1",
                    Some(start - 60_000),
                    "an earlier run, resumed"
                ),
                heard(
                    "codex",
                    "c1",
                    Some(start + 1_000),
                    "another agent's session"
                ),
                heard(
                    "claude",
                    "c1/agent-a1",
                    Some(start + 1_000),
                    "to a subagent"
                ),
                heard("claude", "c1", Some(start + 9_000), "the second thing"),
                heard("claude", "c1", None, "no time"),
                heard("claude", "c1", Some(start + 2_000), "   "),
                heard(
                    "claude",
                    "c1",
                    Some(start + 5_000),
                    "  the first\n thing  typed "
                ),
                heard("claude", "c1", Some(end + 60_000), "after it ended"),
                heard("claude", "c9", Some(start + 1_000), "only claimed"),
            ]),
            json!([]),
        );
        let (heard, gaps) = read_messages(Some(0), &document, "").unwrap();
        assert!(gaps.is_empty(), "{gaps:?}");
        let typed = first_typed(&[&own, &claimed, &quiet], &heard, start - 1_000);
        assert_eq!(
            typed,
            BTreeMap::from([(
                "oh-1".to_owned(),
                Typed {
                    text: "the first thing typed".into(),
                    ended_ms: end,
                }
            )]),
            "its own agent's, in its own time, found in its transcript"
        );
        // Read from after it began: its first words may lie before, so none.
        assert!(first_typed(&[&own], &heard, start + 1_000).is_empty());
    }

    #[test]
    fn a_message_list_vessa_cannot_give_is_said() {
        let old = read_messages(Some(2), b"", "error: unrecognized subcommand 'messages'");
        assert!(old.unwrap_err().contains("update vessa"));
        let broken = read_messages(Some(0), b"{\"schema\": \"vessa-mess", "");
        assert!(broken.unwrap_err().contains("vessa-messages.v1"));
        let other = json!({"schema": "vessa-messages.v2", "messages": []}).to_string();
        assert!(read_messages(Some(0), other.as_bytes(), "").is_err());
        let failed = read_messages(Some(1), b"", "vessa: disk on fire").unwrap_err();
        assert!(failed.contains("disk on fire"), "{failed}");
        // Partial: what it listed stands, and what it could not read is said.
        let partial = listed(
            json!([heard("claude", "c1", Some(NOW), "hi")]),
            json!(["/h/.codex/sessions/r.jsonl: Permission denied (os error 13)"]),
        );
        let (heard, gaps) = read_messages(Some(4), &partial, "vessa: partial").unwrap();
        assert_eq!(heard.len(), 1);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("Permission denied"), "{gaps:?}");
    }

    fn scan(complete: bool, unreadable: Value, findings: Value) -> Vec<u8> {
        json!({"schema": "vessa-secrets.v1", "since": null, "complete": complete,
               "unreadable": unreadable, "findings": findings})
        .to_string()
        .into_bytes()
    }

    fn found(session: &str, at_ms: i64, place: &str, rule: &str, fingerprint: Value) -> Value {
        json!({"agent": "claude", "session": session, "cwd": "/p/habitat",
               "at": intake::rfc3339(at_ms / 1000), "where": place, "rule": rule,
               "count": 1, "fingerprint": fingerprint})
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
    fn first_words_and_secrets_are_kept_only_for_the_window_the_digest_shows() {
        let from = NOW - 24 * HOUR;
        let mut collected = Collected::default();
        for (id, ended_ms) in [("before", from - 1), ("in", from + 1)] {
            collected.typed.value.insert(
                id.into(),
                Typed {
                    text: "words".into(),
                    ended_ms,
                },
            );
        }
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
        assert_eq!(collected.typed.value.keys().collect::<Vec<_>>(), vec!["in"]);
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
