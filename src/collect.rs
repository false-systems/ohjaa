//! `ohjaa collect`: what the human view cannot work out while a terminal
//! opens, gathered into one snapshot that `ohjaa line` and `ohjaa digest`
//! read. Four parts, each asked once, at the same time:
//!
//! - `local`: the marks on each entry's branch and the open sessions as ohjaa
//!   finds them now (the marks' git and sykli, and `ps`), from
//!   [`report::build_marks`];
//! - `github`: the pull requests updated in the entries' owners, one search
//!   through `gh api graphql`, at most [`MAX_GH_CALLS`] calls;
//! - `usage`: what vessa reads in the transcripts of the sessions ended
//!   lately, joined by the agent's own session ids ohjaa found at leaving;
//! - `fleet`: who is live, as toimija says ([`scan::scan_fleet`]).
//!
//! The session log and the proposals are not in it: the view reads them
//! itself, as it speaks. Each part says when it was read and, when it could
//! not be, why; a part that failed keeps what it held before, as of then.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::Dirs;
use crate::sessions::{self, Check, Session};
use crate::{home, intake, mark, process, report, scan, usage};

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
        mergeCommit { oid }
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
    #[serde(default)]
    pub fleet: Part<Vec<Live>>,
}

/// One source's answer, when it was read, and why not.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Part<T> {
    /// When it last answered; `None`: never.
    pub as_of_ms: Option<i64>,
    #[serde(default)]
    pub value: T,
    /// Why it did not answer this time: what it holds is as of `as_of_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What it said beside its answer: a gap, a page it did not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl<T: Clone> Part<T> {
    fn read(answer: Result<(T, Vec<String>), String>, before: &Part<T>, now_ms: i64) -> Part<T> {
        match answer {
            Ok((value, notes)) => Part {
                as_of_ms: Some(now_ms),
                value,
                error: None,
                notes,
            },
            Err(why) => Part {
                as_of_ms: before.as_of_ms,
                value: before.value.clone(),
                error: Some(why),
                notes: Vec::new(),
            },
        }
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
    /// Why it has no marks.
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
    pub tokens: Option<u64>,
    pub usd_micros: Option<u64>,
}

/// A session toimija says may still be working.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Live {
    pub repo: String,
    pub agent: String,
    pub intent: String,
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
/// marks), `fleet` those toimija knows; GitHub and vessa are asked about
/// what happened since `from_ms`.
pub fn collect(
    dirs: &Dirs,
    repos: Result<Vec<PathBuf>, String>,
    fleet: Result<Vec<PathBuf>, String>,
    from_ms: i64,
    now_ms: i64,
) -> Collected {
    let before = read(dirs).unwrap_or_default();
    let owners = owners();
    std::thread::scope(|scope| {
        let local = scope.spawn(|| repos.map(|repos| local(dirs, &repos, now_ms)));
        let github = scope.spawn(|| github(&owners?, from_ms));
        let usage = scope.spawn(|| usage(dirs, from_ms));
        let fleet = scope.spawn(|| fleet.map(|repos| live(&repos)));
        Collected {
            schema: SCHEMA.to_owned(),
            at_ms: now_ms,
            local: Part::read(joined(local), &before.local, now_ms),
            github: Part::read(joined(github), &before.github, now_ms),
            usage: Part::read(joined(usage), &before.usage, now_ms),
            fleet: Part::read(joined(fleet), &before.fleet, now_ms),
        }
    })
}

type Answer<T> = Result<(T, Vec<String>), String>;

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Answer<T>>) -> Answer<T> {
    handle
        .join()
        .unwrap_or_else(|_| Err("its reader panicked".to_owned()))
}

/// The marks and the open sessions, as `ohjaa report --marks` finds them;
/// what it could not read is said beside them.
fn local(dirs: &Dirs, repos: &[PathBuf], now_ms: i64) -> (Local, Vec<String>) {
    let report = report::build_marks(dirs, repos, now_ms - report::WEEK_MS, now_ms);
    let branches = report
        .marks
        .iter()
        .map(|(slug, marked)| branch(slug, marked))
        .collect();
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

fn branch(slug: &str, marked: &Result<mark::Marks, String>) -> Branch {
    let marks = match marked {
        Ok(marks) => marks,
        Err(why) => {
            return Branch {
                slug: slug.to_owned(),
                error: Some(why.clone()),
                ..Branch::default()
            }
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
            vec!["the workflow home has no entries: no owner to ask GitHub about".to_owned()],
        ));
    }
    let search = search(owners, from_ms);
    let mut prs = Vec::new();
    let mut notes = Vec::new();
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
            notes.push(format!(
                "{} pull requests were updated since {}; the {} updated last are kept",
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
    Ok((prs, notes))
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
    let mut notes = Vec::new();
    if code == Some(4) {
        notes.push("vessa says its usage evidence is partial".to_owned());
    }
    let read = read_usage(&answer, &mut notes)?;
    let costs = logged
        .iter()
        .map(|s| (s.id.clone(), cost_of(s, &read)))
        .collect();
    Ok((costs, notes))
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

/// What vessa read of one native session: its tokens, its estimate, and
/// whether every entry was priced. `None`: its id is in vessa's answer twice,
/// so it is no one's.
type Read = BTreeMap<(String, String), Option<(Option<u64>, Option<u64>)>>;

/// All of them, or unknown when one is.
fn sum(mut values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    values.try_fold(0u64, |total, value| total.checked_add(value?))
}

/// `vessa-usage.v1`, by agent and native session id.
pub fn read_usage(answer: &Value, notes: &mut Vec<String>) -> Result<Read, String> {
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
            notes.push(format!("{agent} {id}: {}", why.as_str().unwrap_or("?")));
        }
        // A session's usage is its subagents' too.
        let entries = match session["totals_with_subagents"].as_array() {
            Some(entries) if !entries.is_empty() => entries,
            _ => session["totals"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        };
        let tokens = (!entries.is_empty())
            .then(|| {
                sum(entries.iter().map(|entry| {
                    sum([
                        "input_tokens",
                        "output_tokens",
                        "cache_read_tokens",
                        "cache_write_tokens",
                    ]
                    .iter()
                    .map(|name| entry[*name].as_u64()))
                }))
            })
            .flatten();
        let priced = (!entries.is_empty())
            .then(|| sum(entries.iter().map(|e| e["estimated_usd_micros"].as_u64())))
            .flatten();
        let key = (agent.to_owned(), id.to_owned());
        if read.insert(key, Some((tokens, priced))).is_some() {
            notes.push(format!(
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
    let mut tokens = Some(0u64);
    let mut usd = Some(0u64);
    for native in natives(session) {
        let found = has_transcript(native)
            .then(|| read.get(&(native.agent.clone(), native.session_id.clone())))
            .flatten()
            .copied()
            .flatten();
        let (t, u) = found.unwrap_or((None, None));
        tokens = tokens.zip(t).and_then(|(a, b)| a.checked_add(b));
        usd = usd.zip(u).and_then(|(a, b)| a.checked_add(b));
    }
    Cost {
        usd_micros: usd.filter(|_| tokens.is_some()),
        tokens,
    }
}

fn live(repos: &[PathBuf]) -> (Vec<Live>, Vec<String>) {
    let scans = scan::scan_fleet(repos);
    let failed = scans.iter().filter(|scan| scan.error.is_some()).count();
    let live = scans
        .iter()
        .flat_map(|scan| {
            scan.occupying().map(|session| Live {
                repo: scan.name(),
                agent: session.agent.clone(),
                intent: session.intent.clone(),
            })
        })
        .collect();
    let notes = (failed > 0)
        .then(|| format!("{failed} repositories could not be scanned"))
        .into_iter()
        .collect();
    (live, notes)
}

/// Where GitHub and vessa are read from: since the digest was last shown
/// (and a little before, for what is indexed late), at least a day, at most
/// a week.
pub fn from_ms(digest_ms: Option<i64>, now_ms: i64) -> i64 {
    let day = 86_400_000;
    digest_ms
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

    fn entry(tokens: u64, usd: Option<u64>) -> Value {
        json!({"input_tokens": tokens, "output_tokens": 0, "cache_read_tokens": 0,
               "cache_write_tokens": 0, "estimated_usd_micros": usd})
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
            {"agent": "claude", "id": "c1", "totals": [entry(1_000, None)],
             "totals_with_subagents": [entry(1_500, None)]},
            {"agent": "codex", "id": "x1", "totals": [entry(500, None)]},
        ]));
        let read = read_usage(&answer, &mut Vec::new()).unwrap();
        // A session's usage is its subagents' too; nothing priced, no estimate.
        assert_eq!(
            cost_of(&s, &read),
            Cost {
                tokens: Some(2_000),
                usd_micros: None
            }
        );
    }

    #[test]
    fn usage_never_joins_another_agent_an_ambiguous_id_or_a_claim() {
        let answer = vessa(json!([
            {"agent": "codex", "id": "c1", "totals": [entry(1_000, Some(5))]},
            {"agent": "claude", "id": "dup", "totals": [entry(1, Some(1))]},
            {"agent": "claude", "id": "dup", "totals": [entry(2, Some(2))]},
        ]));
        let mut notes = Vec::new();
        let read = read_usage(&answer, &mut notes).unwrap();
        assert!(notes
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
            {"agent": "claude", "id": "a", "totals": [entry(10, Some(7)), entry(5, None)]},
            {"agent": "claude", "id": "b", "totals": []},
            {"agent": "claude", "id": "c", "totals": [entry(3, Some(2))]},
        ]));
        let read = read_usage(&answer, &mut Vec::new()).unwrap();
        let cost = |ids: &[&str]| {
            let natives: Vec<Value> = ids
                .iter()
                .map(|id| native("claude", id, &format!("/t/{id}.jsonl")))
                .collect();
            cost_of(&session("oh", json!(natives)), &read)
        };
        assert_eq!(
            cost(&["a"]),
            Cost {
                tokens: Some(15),
                usd_micros: None
            },
            "a part unpriced is no estimate"
        );
        assert_eq!(cost(&["b"]), Cost::default(), "no totals is unknown");
        assert_eq!(cost(&["c", "missing"]), Cost::default(), "one not found");
        assert_eq!(
            cost(&["c"]),
            Cost {
                tokens: Some(3),
                usd_micros: Some(2)
            }
        );
        let wrong = json!({"schema": "vessa-other.v1", "sessions": []});
        assert!(read_usage(&wrong, &mut Vec::new()).is_err());
    }

    #[test]
    fn a_failed_part_keeps_what_it_held_as_of_then() {
        let before = Part {
            as_of_ms: Some(5),
            value: vec![1],
            error: None,
            notes: vec!["old".into()],
        };
        let failed = Part::read(Err("gh: no network".into()), &before, 9);
        assert_eq!(failed.as_of_ms, Some(5));
        assert_eq!(failed.value, vec![1]);
        assert_eq!(failed.error.as_deref(), Some("gh: no network"));
        let answered = Part::read(Ok((vec![2], Vec::new())), &before, 9);
        assert_eq!((answered.as_of_ms, answered.value), (Some(9), vec![2]));
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

    #[test]
    fn github_and_vessa_read_since_the_last_digest_a_day_to_a_week() {
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
