//! `ohjaa collect`: what the human view cannot work out while a terminal
//! opens, gathered into one snapshot that `ohjaa line` and `ohjaa digest`
//! read. Five parts, each asked once, at the same time:
//!
//! - `local`: the marks on each entry's branch and the open sessions as ohjaa
//!   finds them now (the marks' git and sykli, and `ps`), from
//!   [`report::build_marks`];
//! - `github`: the pull requests updated in the entries' owners, one search
//!   through `gh api graphql`, at most [`MAX_GH_CALLS`] calls;
//! - `usage`: what vessa reads in the transcripts of the sessions ended
//!   lately, joined by the agent's own session ids ohjaa found at leaving;
//! - `typed`: the first words the person typed in each of those sessions,
//!   from one `vessa messages --json`, joined the same way, cut short, and
//!   kept only for the window the digest shows;
//! - `secrets`: where vessa's secrets scan found secret-shaped tokens in
//!   agent sessions, and how many, never a value: one `vessa secrets --json`
//!   from where the last scan that read everything ended.
//!
//! vessa is asked at most three times, once for each of its parts, and only
//! when there is something to ask: never `usage` or `messages` without a
//! session that ended lately.
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
const MAX_USAGE_SESSIONS: usize = 50;

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
    pub local: Part<Local>,
    #[serde(default)]
    pub github: Part<Vec<Pr>>,
    /// By ohjaa session id.
    #[serde(default)]
    pub usage: Part<BTreeMap<String, TokenUsage>>,
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
        self.typed.value.retain(|_, typed| typed.ended_ms > from_ms);
        self.secrets
            .value
            .findings
            .retain(|finding| finding.at_ms.unwrap_or(finding.found_ms) >= from_ms);
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

/// Token usage for one ohjaa session, unknown (`None`) unless every one
/// of the agent's own sessions it ran was found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input, output and cache writes.
    pub tokens: Option<u64>,
    /// Said apart: most of a session's tokens are reads of its cache.
    #[serde(default)]
    pub cache_reads: Option<u64>,
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
    let ended = ended_since(dirs, from_ms);
    let ended = || ended.as_deref().map_err(String::clone);
    let mut collected = std::thread::scope(|scope| {
        let local = scope.spawn(|| repos.map(|repos| local(dirs, &repos, known, now_ms)));
        let github = scope.spawn(|| github(&owners?, from_ms));
        let usage = scope.spawn(|| usage(ended()?));
        let typed = scope.spawn(|| typed(ended()?, now_ms));
        let secrets = scope.spawn(|| secrets(&before.secrets.value, from_ms, now_ms));
        Collected {
            schema: SCHEMA.to_owned(),
            at_ms: now_ms,
            local: Part::read(joined(local), &before.local, now_ms, None),
            github: Part::read(joined(github), &before.github, now_ms, Some(from_ms)),
            usage: Part::read(joined(usage), &before.usage, now_ms, Some(from_ms)),
            typed: Part::read(joined(typed), &before.typed, now_ms, Some(from_ms)),
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
        draft: node["isDraft"].as_bool().unwrap_or(false),
        updated_ms: time("updatedAt"),
    })
}

/// The sessions that ended since `from_ms` and named their agent's own
/// sessions, the newest [`MAX_USAGE_SESSIONS`]: what vessa is asked about.
fn ended_since(dirs: &Dirs, from_ms: i64) -> Result<Vec<Session>, String> {
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
    logged.truncate(MAX_USAGE_SESSIONS);
    Ok(logged)
}

/// The usage of the sessions that ended lately (`logged`), from those
/// sessions' transcripts alone: vessa is given each transcript, never asked
/// to read them all.
fn usage(logged: &[Session]) -> Result<(BTreeMap<String, TokenUsage>, Vec<String>), String> {
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
    let output = process::run(&mut command, None, VESSA_TIMEOUT)
        .map_err(|error| vessa_failed("usage", &error))?;
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
        .map(|s| (s.id.clone(), usage_of(s, &read)))
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

/// What vessa read of one native session, as a [`TokenUsage`]. `None`: its id is
/// in vessa's answer twice, so it is no one's.
type Read = BTreeMap<(String, String), Option<TokenUsage>>;

/// All of them, or unknown when one is.
fn sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0u64, |total, value| total.checked_add(value?))
}

/// Token totals from `vessa-usage.v2`, by agent and native session id.
/// The former v1 export has the same token fields; its money is ignored.
pub fn read_usage(answer: &Value, gaps: &mut Vec<String>) -> Result<Read, String> {
    match answer["schema"].as_str() {
        Some("vessa-usage.v1" | "vessa-usage.v2") => {}
        other => {
            return Err(format!(
                "vessa answered {}, not vessa-usage.v1 or vessa-usage.v2",
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
        let cost = TokenUsage {
            tokens: all(&["input_tokens", "output_tokens", "cache_write_tokens"]),
            cache_reads: all(&["cache_read_tokens"]),
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

/// The usage of one ohjaa session: known only when each of its agent's own
/// sessions has a transcript and vessa read it once.
pub fn usage_of(session: &Session, read: &Read) -> TokenUsage {
    let found: Vec<Option<&TokenUsage>> = natives(session)
        .map(|native| {
            has_transcript(native)
                .then(|| read.get(&(native.agent.clone(), native.session_id.clone())))
                .flatten()
                .and_then(Option::as_ref)
        })
        .collect();
    let total = |of: fn(&TokenUsage) -> Option<u64>| {
        found
            .iter()
            .try_fold(0u64, |total, cost| total.checked_add(of((*cost)?)?))
    };
    let tokens = total(|c| c.tokens);
    let cache_reads = total(|c| c.cache_reads).filter(|_| tokens.is_some());
    TokenUsage {
        cache_reads,
        tokens,
    }
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

// ---- the first words typed ----------------------------------------------------

/// The form of `vessa messages --json` the first words are read from.
const VESSA_MESSAGES: &str = "vessa-messages.v1";

/// The furthest back one list of what people wrote reaches: a session begun
/// before then gets no first words, rather than words that were not its
/// first.
const TYPED_REACH_MS: i64 = 7 * 86_400_000;

/// The first words the person typed in each session that ended lately
/// (`logged`), from one `vessa messages --json` call. vessa scrubs secrets
/// from what it prints (never asked for `--raw`), and ends a launch's task
/// at ohjaa's briefing: what it lists is the person's own.
fn typed(logged: &[Session], now_ms: i64) -> Answer<BTreeMap<String, Typed>> {
    let asked: Vec<&Session> = logged
        .iter()
        .filter(|s| natives(s).any(has_transcript))
        .collect();
    let Some(earliest) = asked.iter().map(|s| s.started_ms).min() else {
        return Ok((BTreeMap::new(), Vec::new()));
    };
    let since_ms = (earliest - 1000).max(now_ms - TYPED_REACH_MS);
    let mut command = Command::new("vessa");
    command
        .args(["messages", "--json", "--since"])
        .arg(intake::rfc3339_ms(since_ms));
    let output = process::run(&mut command, None, VESSA_TIMEOUT)
        .map_err(|error| vessa_failed("messages", &error))?;
    let (heard, gaps) = read_messages(
        output.status.code(),
        &output.stdout,
        &said_on_stderr(&output),
    )?;
    Ok((first_typed(&asked, &heard, since_ms), gaps))
}

#[derive(Deserialize)]
struct Listed {
    schema: String,
    #[serde(default)]
    messages: Vec<ListedMessage>,
    #[serde(default)]
    coverage: Option<ListedCoverage>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct ListedMessage {
    agent: String,
    session: String,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    text: String,
}

#[derive(Default, Deserialize)]
struct ListedCoverage {
    #[serde(default)]
    gaps: Vec<String>,
    #[serde(default)]
    limitations: Vec<String>,
}

/// vessa's list of what people wrote (`vessa-messages.v1`; its exit `code`,
/// and `detail`, what it said on stderr), with what it could not list. Exit
/// 4 is a partial list: what it listed stands, and why is a gap. A message
/// no time places is left out: it cannot be told first.
pub fn read_messages(
    code: Option<i32>,
    stdout: &[u8],
    detail: &str,
) -> Result<(Vec<Heard>, Vec<String>), String> {
    let exit = exit_of(code);
    if !matches!(code, Some(0 | 4)) {
        return Err(if detail.contains("unrecognized subcommand") {
            format!("this vessa does not list messages: update vessa (exit {exit}: {detail})")
        } else {
            format!("vessa messages --json: exit {exit}: {detail}")
        });
    }
    let listed: Listed = serde_json::from_slice(stdout).map_err(|error| {
        format!("vessa messages --json printed no {VESSA_MESSAGES} ({error}; exit {exit})")
    })?;
    if listed.schema != VESSA_MESSAGES {
        return Err(format!(
            "vessa answered {}, not {VESSA_MESSAGES}",
            listed.schema
        ));
    }
    let coverage = listed.coverage.unwrap_or_default();
    let unread: Vec<&String> = listed
        .warnings
        .iter()
        .chain(&coverage.gaps)
        .chain(&coverage.limitations)
        .collect();
    let mut gaps = Vec::new();
    if let Some(error) = &listed.error {
        gaps.push(format!(
            "vessa failed after listing {} message(s): {error}",
            listed.messages.len()
        ));
    }
    match unread.as_slice() {
        [] if code == Some(4) && gaps.is_empty() => gaps.push(format!(
            "vessa says its list of messages is partial: {detail}"
        )),
        [] => {}
        [one] => gaps.push(format!("vessa could not list every message: {one}")),
        [first, ..] => gaps.push(format!(
            "vessa could not list every message, for {} reasons, first: {first}",
            unread.len()
        )),
    }
    let heard = listed
        .messages
        .into_iter()
        .filter_map(|message| {
            Some(Heard {
                at_ms: message.at.as_deref().and_then(intake::parse_time)?,
                agent: message.agent,
                session: message.session,
                text: message.text,
            })
        })
        .collect();
    Ok((heard, gaps))
}

/// The first words typed in each of `sessions`, from what vessa listed
/// after `since_ms`: the earliest message with words in it of one of its
/// agent's own sessions found in a transcript (as usage is joined), between
/// its launch and its end. A session begun before the list does gets none:
/// its first words may lie before it.
pub fn first_typed(
    sessions: &[&Session],
    heard: &[Heard],
    since_ms: i64,
) -> BTreeMap<String, Typed> {
    sessions
        .iter()
        .filter_map(|session| {
            let left = session.left.as_ref()?;
            let start = session.started_ms - session.started_ms.rem_euclid(1000);
            if start <= since_ms {
                return None;
            }
            let own: BTreeSet<(&str, &str)> = natives(session)
                .filter(|native| has_transcript(native))
                .map(|native| (native.agent.as_str(), native.session_id.as_str()))
                .collect();
            let (_, text) = heard
                .iter()
                .filter(|said| own.contains(&(said.agent.as_str(), said.session.as_str())))
                .filter(|said| said.at_ms >= start && said.at_ms < left.ended_ms + 1000)
                .filter_map(|said| Some((said.at_ms, excerpt(&said.text)?)))
                .min_by_key(|(at_ms, _)| *at_ms)?;
            Some((
                session.id.clone(),
                Typed {
                    text,
                    ended_ms: left.ended_ms,
                },
            ))
        })
        .collect()
}

/// `text` with its runs of white space made one space, cut at a word to at
/// most [`EXCERPT_CHARS`], with `…` when cut. `None`: nothing in it.
pub fn excerpt(text: &str) -> Option<String> {
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!words.is_empty()).then(|| fit(&words, EXCERPT_CHARS))
}

/// At most `max` characters, cut after a word with `…` when cut; inside a
/// word only when the first is longer than all the room.
pub fn fit(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let room = max.saturating_sub(1);
    let mut out = String::new();
    for word in text.split(' ') {
        let width = out.chars().count() + usize::from(!out.is_empty()) + word.chars().count();
        if width > room {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        out = text.chars().take(room).collect();
    }
    out.push('…');
    out
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
    let mut findings = before.findings.clone();
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
        json!({"schema": "vessa-usage.v2", "sessions": sessions, "warnings": []})
    }

    /// `tokens` of input and `cached` cache reads.
    fn entry(tokens: u64, cached: u64, usd: Option<u64>) -> Value {
        json!({"input_tokens": tokens, "output_tokens": 0, "cache_read_tokens": cached,
               "cache_write_tokens": 0, "estimated_usd_micros": usd})
    }

    fn cost(tokens: Option<u64>, cache_reads: Option<u64>) -> TokenUsage {
        TokenUsage {
            tokens,
            cache_reads,
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
    fn a_pull_request_says_whether_it_is_a_draft_and_when_it_last_changed() {
        let node = json!({
            "number": 12,
            "title": "Rename the config module",
            "url": "https://github.com/false-systems/habitat/pull/12",
            "createdAt": "2026-11-08T08:00:00Z",
            "updatedAt": "2026-11-08T09:40:00Z",
            "isDraft": true,
            "author": { "login": "dima" },
            "repository": { "nameWithOwner": "false-systems/habitat" },
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
        // No spending is read or calculated.
        assert_eq!(usage_of(&s, &read), cost(Some(2_000), Some(100_000)));
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
        let unknown = TokenUsage::default();
        // Another agent's session of the same id is not this one.
        let other = session("oh-1", json!([native("claude", "c1", "/t/c1.jsonl")]));
        assert_eq!(usage_of(&other, &read), unknown);
        let twice = session("oh-2", json!([native("claude", "dup", "/t/d.jsonl")]));
        assert_eq!(usage_of(&twice, &read), unknown);
        // Only the agent's environment named it: no transcript was read.
        let mut claimed = native("codex", "c1", usage::ENV_SOURCE);
        claimed["testimony"] = json!(true);
        assert_eq!(usage_of(&session("oh-3", json!([claimed])), &read), unknown);
    }

    #[test]
    fn missing_usage_stays_unknown_and_legacy_spending_is_ignored() {
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
            usage_of(&session("oh", json!(natives)), &read)
        };
        assert_eq!(
            of(&["a"]),
            cost(Some(15), Some(2)),
            "legacy spending does not affect token totals"
        );
        assert_eq!(of(&["b"]), TokenUsage::default(), "no totals is unknown");
        assert_eq!(
            of(&["c", "missing"]),
            TokenUsage::default(),
            "one not found"
        );
        assert_eq!(of(&["c"]), cost(Some(3), Some(4)));
        let wrong = json!({"schema": "vessa-other.v1", "sessions": []});
        assert!(read_usage(&wrong, &mut Vec::new()).is_err());
    }

    #[test]
    fn token_only_v2_and_legacy_v1_have_the_same_token_evidence() {
        let expected = TokenUsage {
            tokens: Some(35),
            cache_reads: Some(400),
        };
        let entries = json!([{"input_tokens":20,"output_tokens":10,
            "cache_write_tokens":5,"cache_read_tokens":400}]);
        let mut answer = vessa(json!([{"agent":"pi","id":"pi-1",
            "totals":entries,"limitations":[]}]));
        let mut gaps = Vec::new();
        let read = read_usage(&answer, &mut gaps).unwrap();
        assert_eq!(read[&("pi".into(), "pi-1".into())], Some(expected.clone()));
        assert!(gaps.is_empty());
        answer["schema"] = json!("vessa-usage.v1");
        answer["sessions"][0]["totals"][0]["estimated_usd_micros"] = json!(9_000_000);
        let legacy = read_usage(&answer, &mut gaps).unwrap();
        assert_eq!(legacy[&("pi".into(), "pi-1".into())], Some(expected));
        assert!(gaps.is_empty());
        answer["sessions"][0]["totals"][0]
            .as_object_mut()
            .unwrap()
            .remove("input_tokens");
        let missing = read_usage(&answer, &mut gaps).unwrap();
        assert_eq!(
            missing[&("pi".into(), "pi-1".into())]
                .as_ref()
                .unwrap()
                .tokens,
            None
        );
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
