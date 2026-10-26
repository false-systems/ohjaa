//! The bypass report: what went around the workflow in a window, derived now.
//!
//! Three lists, each from something ohjaa can observe, never from anyone's
//! word:
//! - sessions in the log that never read back their briefing, or left no
//!   handover;
//! - commits on a known repository's default branch that sykli refused, or
//!   that were never checked where a receipt would be;
//! - native agent sessions (vessa's list) a person started without
//!   `ohjaa run`.
//!
//! What could not be looked at, or not judged, is said and counted, never
//! shown as nothing. Only the time a complete report was last shown to a
//! person is stored, like intake's cursor; the report itself is derived each
//! time.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::intake::{self, Candidate};
use crate::knowledge::{self, Dirs};
use crate::lifecycle;
use crate::mark;
use crate::process;
use crate::sessions::{self, Check, Session};

/// The window a report covers unless told otherwise, and how often `ohjaa
/// run` says one to the person.
pub const WEEK_MS: i64 = 7 * 86_400_000;

/// How long before its `ohjaa run` launch was logged a native session may
/// start and still be that launch's agent: the log and the transcript each
/// keep their own clock.
const LAUNCH_EARLY_MS: i64 = 5_000;

/// How long vessa may take to list the sessions. Its header mode reads only
/// each transcript's opening records: about half a second for some 950
/// sessions here, where the full listing took 10 to 50 s. 15 s leaves a
/// loaded machine and a cold disk cache thirty times that, and a vessa that
/// hangs costs a report seconds, not a minute.
const VESSA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What `vessa sessions --json --headers` answers with (vessa#5).
const VESSA_HEADERS: &str = "vessa-session-headers.v1";

/// The agents vessa reads the sessions of and `ohjaa run` launches. opencode
/// is launched too, but vessa does not read its sessions.
const NATIVE_AGENTS: [&str; 3] = ["claude", "codex", "pi"];

/// A session that ended without the agent's testimony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Untold {
    pub id: String,
    pub agent: String,
    pub repo: String,
    pub intent: String,
    pub started_ms: i64,
    pub no_readback: bool,
    pub no_handover: bool,
    /// Its ohjaa was gone before leaving: it could testify to nothing after.
    /// For a session still open, its ohjaa and its process group are gone
    /// now (the next launch closes it as orphaned).
    pub orphaned: bool,
    /// Still open: its ohjaa is gone, but its own process group still runs.
    pub ohjaa_gone: bool,
}

/// A commit on a default branch without passing evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unproven {
    pub repo: String,
    pub on: lifecycle::OnDefault,
}

/// What became of every default-branch commit in the window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judged {
    pub repos: usize,
    pub commits: usize,
    pub verified: usize,
    /// Of the verified, those whose receipt reused tasks from sykli's cache:
    /// said apart, never folded into a run that passed.
    pub verified_reusing_cache: usize,
    pub no_contract: usize,
    /// Made in a checkout that is gone, with its receipts, and none kept.
    pub gone: usize,
    /// Of those, merge commits (a forge's "Merge pull request").
    pub gone_merges: usize,
    /// sykli could not say: exit 2, a crash, or no `--commit`.
    pub could_not_say: usize,
    pub stale: usize,
}

/// A native agent session that did not go through `ohjaa run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bypass {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    pub started_ms: i64,
}

/// A native session vessa could not date or place: not judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undated {
    pub agent: String,
    pub id: String,
    /// Its transcript, and when that was last written.
    pub source: String,
    pub written_ms: Option<i64>,
}

/// How a native session was found to be an `ohjaa run` launch's, surest
/// first: what ohjaa observed, then what a source said (law 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum By {
    /// Its transcript holds the launch's prompt, found at leaving: observed.
    LaunchPrompt,
    /// A usage receipt the launch imported names it: the source's word.
    Receipt,
    /// The agent's environment claimed it from inside the launch (`ohjaa
    /// oriented`, `ohjaa handover`): the agent's word.
    Env,
    /// Neither: the launch recorded no id, and the session is its agent's,
    /// in its folder, started while it ran.
    Window,
}

impl By {
    pub fn word(self) -> &'static str {
        match self {
            By::Receipt => "receipt",
            By::Env => "env",
            By::LaunchPrompt => "launch_prompt",
            By::Window => "time_and_folder",
        }
    }

    /// A source's word, not ohjaa's observation: an imported receipt, or
    /// the agent's environment (law 3).
    pub fn testimony(self) -> bool {
        matches!(self, By::Receipt | By::Env)
    }
}

/// A native session matched to the `ohjaa run` launch that ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub agent: String,
    pub id: String,
    /// The ohjaa session.
    pub session: String,
    pub by: By,
}

/// What vessa's list says about sessions run without ohjaa.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Natives {
    pub bypasses: Vec<Bypass>,
    /// The sessions launched through ohjaa, and how each was matched.
    pub matched: Vec<Matched>,
    /// Headless runs (`claude -p`, `codex exec`, intake's drafter among
    /// them): no person started them, so they bypass nothing.
    pub headless: usize,
    pub undated: Vec<Undated>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub since_ms: i64,
    pub until_ms: i64,
    pub untold: Vec<Untold>,
    pub unproven: Vec<Unproven>,
    pub judged: Judged,
    /// The native sessions not run through ohjaa, or why they could not be
    /// judged.
    pub natives: Result<Natives, String>,
    /// The marks on each entry's branch, or why it has none, by `owner/name`.
    pub marks: Vec<(String, Result<mark::Marks, String>)>,
    /// What waits for a person's decision, or why it could not be listed.
    pub proposals: Result<Vec<Waiting>, String>,
    /// Sessions in the log that are still open, and those closed as orphaned
    /// that started in the window.
    pub open: Vec<Untold>,
    /// What could not be read or judged, each with why: a report with gaps
    /// says so, and exits 4.
    pub gaps: Vec<String>,
    /// The window (its three lists) was read; `false` for the marks alone
    /// (`--marks`), which read no default-branch window and ask no vessa.
    pub window: bool,
    /// The integrated validations sessions asked sykli for (KATANA D5),
    /// each with the session that recorded it: references, never verdicts.
    pub integrated: Vec<(String, sessions::ValidationRef)>,
}

/// A proposal, or a proposed node, waiting for a person's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub scope: String,
    pub name: String,
    /// Its body's first line.
    pub title: String,
    /// Its `as_of` date.
    pub since: String,
    /// For a repo proposal: the repository.
    pub repo: Option<String>,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.gaps.is_empty() && self.natives.is_ok()
    }
}

/// `7d`, `24h`, a date or an RFC 3339 time: the start of the window, in
/// milliseconds, before `now_ms`.
pub fn parse_since(text: &str, now_ms: i64) -> Result<i64> {
    let text = text.trim();
    let relative = |suffix: char, unit: i64| {
        text.strip_suffix(suffix)
            .and_then(|n| n.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .map(|n| now_ms - n * unit)
    };
    relative('d', 86_400_000)
        .or_else(|| relative('h', 3_600_000))
        .map(Ok)
        .unwrap_or_else(|| intake::normalize_since(text))
}

/// The report for after `since_ms` until `now_ms`: the log in `dirs`, the git
/// repositories `repos` (those under the roots), and vessa's sessions when
/// vessa is on PATH.
pub fn build(dirs: &Dirs, repos: &[PathBuf], since_ms: i64, now_ms: i64) -> Report {
    build_with(dirs, repos, since_ms, now_ms, true)
}

/// The marks of every entry, what waits for a person and the sessions still
/// open or orphaned, alone: no default-branch window and no vessa, so it
/// costs two git processes per entry and a `sykli verify` only where a
/// receipt says it passed (`ohjaa report --marks`).
pub fn build_marks(dirs: &Dirs, repos: &[PathBuf], since_ms: i64, now_ms: i64) -> Report {
    build_with(dirs, repos, since_ms, now_ms, false)
}

fn build_with(dirs: &Dirs, repos: &[PathBuf], since_ms: i64, now_ms: i64, window: bool) -> Report {
    let mut gaps = Vec::new();
    // Opened only to read: a report never writes the log (#41).
    let logged: Vec<Session> = match sessions::Reader::open(&sessions::store_path(&dirs.state)) {
        Ok(log) => match log.all() {
            Ok((all, unreadable)) => {
                if !unreadable.is_empty() {
                    gaps.push(format!(
                        "{} session record(s) in the log could not be read",
                        unreadable.len()
                    ));
                }
                all
            }
            Err(error) => {
                gaps.push(format!("the session log could not be read: {error}"));
                Vec::new()
            }
        },
        Err(error) => {
            gaps.push(format!("the session log could not be opened: {error}"));
            Vec::new()
        }
    };
    let untold = if window {
        untold(&logged, since_ms, now_ms)
    } else {
        Vec::new()
    };
    // One sykli for both: a commit the window and the marks share is asked
    // about once, and a repository with an entry is judged by its gate.
    let sykli = lifecycle::sykli_with_entries();
    let (unproven, judged) = if window {
        default_branches(repos, since_ms, &sykli, &mut gaps)
    } else {
        (Vec::new(), Judged::default())
    };
    let marks = match crate::home::ohjaa_home() {
        Ok(home) => mark::of_entries(&crate::home::absolute(home), repos, &sykli),
        Err(error) => {
            gaps.push(format!("the workflow home cannot be found: {error:#}"));
            Vec::new()
        }
    };
    for (slug, marked) in &marks {
        if let Err(why) = marked {
            gaps.push(format!("{slug}: not marked: {why}"));
        }
    }
    let proposals = waiting(dirs);
    let open = open_sessions(&logged, since_ms, now_ms, sessions::observe_open);
    let natives = if window {
        native_sessions(since_ms, now_ms, &mut gaps).map(|listed| {
            let (judgeable, mut natives) = listed;
            (natives.bypasses, natives.matched) = judge_natives(judgeable, &logged);
            natives
        })
    } else {
        Ok(Natives::default())
    };
    if let Ok(natives) = &natives {
        if !natives.undated.is_empty() {
            gaps.push(format!(
                "{} native session(s) vessa could not date or place: not judged",
                natives.undated.len()
            ));
        }
    }
    // The integrated validations sessions asked for: from the log, read
    // once above.
    let integrated = logged
        .iter()
        .flat_map(|session| {
            session
                .validations
                .iter()
                .filter(|v| v.kind == sessions::ValidationKind::Integrated)
                .map(|v| (session.id.clone(), v.clone()))
        })
        .collect();
    Report {
        since_ms,
        until_ms: now_ms,
        untold,
        unproven,
        judged,
        natives,
        marks,
        proposals,
        open,
        gaps,
        window,
        integrated,
    }
}

/// What waits for a person: every proposal, and every proposed node in a
/// scope directory. Files only.
fn waiting(dirs: &Dirs) -> Result<Vec<Waiting>, String> {
    let loaded = knowledge::load(dirs).map_err(|error| format!("{error:#}"))?;
    let (pending, _) = knowledge::pending(dirs, &loaded).map_err(|error| format!("{error:#}"))?;
    Ok(pending
        .into_iter()
        .map(|pending| Waiting {
            scope: pending.scope.name().to_owned(),
            name: pending.node.id.clone(),
            title: pending
                .node
                .body
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or_default()
                .to_owned(),
            since: pending.node.as_of.to_string(),
            repo: pending.repo.map(|repo| repo.to_string_lossy().into_owned()),
        })
        .collect())
}

/// Sessions in the log that never ended, each as `observe` finds it now (its
/// ohjaa running, gone while its process group runs on, or both gone), and
/// those closed as orphaned (their ohjaa was gone) that started in the
/// window, oldest first.
pub fn open_sessions(
    logged: &[Session],
    since_ms: i64,
    now_ms: i64,
    observe: impl Fn(&Session) -> sessions::Open,
) -> Vec<Untold> {
    let mut open: Vec<Untold> = logged
        .iter()
        .filter(|s| match &s.left {
            None => true,
            Some(left) => left.orphaned && s.started_ms > since_ms && s.started_ms <= now_ms,
        })
        .map(|s| {
            let now = match s.left {
                None => observe(s),
                Some(_) => sessions::Open::Orphaned,
            };
            Untold {
                id: s.id.clone(),
                agent: s.agent.clone(),
                repo: s.repo.clone(),
                intent: s.intent.clone(),
                started_ms: s.started_ms,
                no_readback: s.readback.is_none(),
                no_handover: s.handover.is_none(),
                orphaned: now == sessions::Open::Orphaned,
                ohjaa_gone: now == sessions::Open::OhjaaGone,
            }
        })
        .collect();
    open.sort_by_key(|s| s.started_ms);
    open
}

/// Every first-parent commit each repository's default branch gained in the
/// window, judged by sykli: the unproven ones, and a count of the rest.
fn default_branches(
    repos: &[PathBuf],
    since_ms: i64,
    sykli: &lifecycle::Sykli,
    gaps: &mut Vec<String>,
) -> (Vec<Unproven>, Judged) {
    let mut unproven = Vec::new();
    let mut judged = Judged::default();
    if repos.is_empty() {
        gaps.push("there are no git repositories under the roots".to_owned());
        return (unproven, judged);
    }
    let mut clones = std::collections::HashSet::new();
    for repo in repos {
        // A worktree of a clone already read is the same clone.
        // From the layout; git only when the layout does not say.
        let common = crate::gitconfig::common_dir(repo)
            .ok()
            .and_then(|common| std::fs::canonicalize(common).ok());
        if common.is_some_and(|common| !clones.insert(common)) {
            continue;
        }
        let Some((commits, capped)) =
            lifecycle::default_branch_since(repo, since_ms.div_euclid(1000), sykli)
        else {
            // A repository with no default branch has nothing to report.
            if has_commits(repo) && has_default_branch(repo) {
                gaps.push(format!(
                    "{}: its default branch could not be read",
                    repo.display()
                ));
            }
            continue;
        };
        judged.repos += 1;
        if capped {
            gaps.push(format!(
                "{}: only the newest {} default-branch commits in the window were judged",
                repo.display(),
                lifecycle::MAX_REPORTED
            ));
        }
        for on in commits {
            judged.commits += 1;
            match &on.check {
                Check::Passed => {
                    judged.verified += 1;
                    if sykli
                        .proof(&on.commit)
                        .is_some_and(|proof| proof.cached > 0)
                    {
                        judged.verified_reusing_cache += 1;
                    }
                }
                Check::Failed { .. } | Check::Unchecked | Check::OtherContract { .. } => unproven
                    .push(Unproven {
                        repo: repo.to_string_lossy().into_owned(),
                        on,
                    }),
                Check::UncheckedGone => {
                    judged.gone += 1;
                    if on.merge {
                        judged.gone_merges += 1;
                    }
                }
                Check::Unverified { why } if why.starts_with("no sykli contract") => {
                    judged.no_contract += 1
                }
                Check::Unverified { .. } => judged.could_not_say += 1,
                Check::Stale { .. } => judged.stale += 1,
            }
        }
    }
    if let Some(why) = lifecycle::sykli_cannot_bind(sykli) {
        if judged.could_not_say > 0 {
            gaps.push(format!(
                "sykli could not verify commits ({why}): only commits never run are listed"
            ));
        }
    }
    unproven.sort_by_key(|u| std::cmp::Reverse(u.on.committed));
    (unproven, judged)
}

fn has_commits(repo: &Path) -> bool {
    git_ok(repo, &["rev-parse", "--verify", "-q", "HEAD"])
}

fn has_default_branch(repo: &Path) -> bool {
    [
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .iter()
    .any(|reference| git_ok(repo, &["rev-parse", "--verify", "-q", reference]))
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Ended sessions that started in the window without a read-back or a
/// handover, oldest first.
pub fn untold(logged: &[Session], since_ms: i64, now_ms: i64) -> Vec<Untold> {
    logged
        .iter()
        .filter(|s| s.left.is_some() && s.started_ms > since_ms && s.started_ms <= now_ms)
        .filter(|s| s.readback.is_none() || s.handover.is_none())
        .map(|s| Untold {
            id: s.id.clone(),
            agent: s.agent.clone(),
            repo: s.repo.clone(),
            intent: s.intent.clone(),
            started_ms: s.started_ms,
            no_readback: s.readback.is_none(),
            no_handover: s.handover.is_none(),
            orphaned: s.left.as_ref().is_some_and(|left| left.orphaned),
            ohjaa_gone: false,
        })
        .collect()
}

/// A native session, as vessa lists it, dated and placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Native {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    pub started_ms: i64,
    /// When its transcript was last written, when that can be told.
    pub written_ms: Option<i64>,
}

/// When the transcript at `source` was last written, in whole seconds, as
/// the window's ends and the log's times are.
fn written(source: &str) -> Option<i64> {
    std::fs::metadata(source)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_secs() as i64 * 1000)
}

/// vessa's list for the window: the sessions a person started, dated and
/// placed, to judge against the launches; and what else it held. `Err` says
/// why they cannot be judged at all. What vessa could not read is a gap.
fn native_sessions(
    since_ms: i64,
    now_ms: i64,
    gaps: &mut Vec<String>,
) -> Result<(Vec<Native>, Natives), String> {
    let output = match process::run(
        Command::new("vessa").args(["sessions", "--json", "--headers"]),
        None,
        VESSA_TIMEOUT,
    ) {
        Ok(output) => output,
        Err(error) => {
            let text = format!("{error:#}");
            return Err(
                if text.contains("No such file") || text.contains("not found") {
                    "vessa is not on PATH".to_owned()
                } else {
                    format!("vessa could not list sessions: {text}")
                },
            );
        }
    };
    // A vessa from before vessa#5 refuses the flag as a usage error: exit 2,
    // nothing listed. It is updated, never read the slow way. What it said
    // is kept, for a usage error of another kind: its first line, where
    // clap puts the error (its last is a hint to try --help).
    if output.status.code() == Some(2) && output.stdout.trim_ascii().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = stderr.lines().map(str::trim).find(|line| !line.is_empty());
        return Err(format!(
            "vessa is too old for `sessions --headers`; update it{}",
            said.map(|line| format!(" (vessa said: {line})"))
                .unwrap_or_default()
        ));
    }
    let listed: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "vessa's sessions are not JSON ({error}; exit {:?}: {})",
            output.status.code(),
            output.failure_detail()
        )
    })?;
    // vessa exits 4 when usage evidence is partial, which says nothing about
    // which sessions it listed; any other failure, and every warning about
    // what it read, is a gap here.
    if !matches!(output.status.code(), Some(0 | 4)) {
        gaps.push(format!(
            "vessa exited {:?}: {}",
            output.status.code(),
            output.failure_detail()
        ));
    }
    for warning in listed["warnings"].as_array().into_iter().flatten() {
        if let Some(warning) = warning.as_str() {
            gaps.push(format!("vessa: {warning}"));
        }
    }
    parse_native(&listed, since_ms, now_ms)
}

/// Whether a session was started by no person: `claude -p` (Claude's
/// `entrypoint` is `sdk-…`), or `codex exec` (Codex's `originator` names
/// exec). Intake's own drafter runs one of these unless configured
/// otherwise. A pi transcript says nothing of how pi was started (vessa
/// gives both as null), so a `pi -p` run is judged as a person's session.
fn headless(session: &Value) -> bool {
    session["entrypoint"]
        .as_str()
        .is_some_and(|entry| entry.starts_with("sdk"))
        || session["originator"]
            .as_str()
            .is_some_and(|origin| origin.contains("exec"))
}

/// Read `vessa sessions --json --headers` (`vessa-session-headers.v1`):
/// `sessions[]` with exactly `agent`, `id`, `parent`, `started_at` (RFC
/// 3339), `cwd`, `source`, and how each was started (`entrypoint`,
/// `originator`), as the full listing names them; any other schema is not
/// read. Claude, Codex and pi sessions are judged. Subagent sessions (with a
/// parent) and headless runs were started by no person and are left out. A
/// session with no date or no folder cannot be judged against a launch: it
/// is kept apart as undated when its transcript was written in the window.
pub fn parse_native(
    listed: &Value,
    since_ms: i64,
    now_ms: i64,
) -> Result<(Vec<Native>, Natives), String> {
    match listed["schema"].as_str() {
        Some(VESSA_HEADERS) => {}
        Some(other) => {
            return Err(format!(
                "vessa listed sessions as `{other}`, not `{VESSA_HEADERS}`"
            ))
        }
        None => {
            return Err(format!(
                "vessa's output names no schema, not `{VESSA_HEADERS}`"
            ))
        }
    }
    let sessions = listed["sessions"]
        .as_array()
        .ok_or("vessa's output has no sessions list")?;
    let in_window = |ms: i64| ms > since_ms && ms <= now_ms;
    let mut judgeable = Vec::new();
    let mut natives = Natives::default();
    for s in sessions
        .iter()
        .filter(|s| {
            s["agent"]
                .as_str()
                .is_some_and(|a| NATIVE_AGENTS.contains(&a))
        })
        .filter(|s| s["parent"].is_null())
    {
        let text = |key: &str| s[key].as_str().unwrap_or_default().to_owned();
        let started_ms = s["started_at"].as_str().and_then(intake::parse_time);
        let cwd = text("cwd");
        match started_ms {
            Some(started_ms) if !cwd.is_empty() => {
                if !in_window(started_ms) {
                    continue;
                }
                if headless(s) {
                    natives.headless += 1;
                    continue;
                }
                judgeable.push(Native {
                    agent: text("agent"),
                    id: text("id"),
                    cwd,
                    started_ms,
                    written_ms: written(&text("source")),
                });
            }
            _ => {
                let source = text("source");
                let written_ms = written(&source);
                // Undated: in the window when its transcript was written then,
                // or when that cannot be told.
                let relevant = match (started_ms, written_ms) {
                    (Some(started_ms), _) => in_window(started_ms),
                    (None, Some(written_ms)) => in_window(written_ms),
                    (None, None) => true,
                };
                if !relevant {
                    continue;
                }
                if headless(s) {
                    natives.headless += 1;
                    continue;
                }
                natives.undated.push(Undated {
                    agent: text("agent"),
                    id: text("id"),
                    source,
                    written_ms,
                });
            }
        }
    }
    Ok((judgeable, natives))
}

/// An id a launch recorded: its agent, the agent's id, and how.
type Recorded<'a> = (&'a str, &'a str, By);

/// The ids of the agent's own sessions a launch recorded, each with how: its
/// usage receipts, and its agent's own sessions. For a launch that left,
/// those are what its leaving found (`left.native`: its launch prompt's, and
/// what its environment claimed before it left that the launch prompt did
/// not contradict); a leaving that recorded none of that leaves only what
/// the environment claimed before the launch ended. For a launch still open,
/// what its environment claimed so far. An id claimed after a launch ended
/// is never its: a shell that outlived it may be another session's.
fn recorded(session: &Session) -> Vec<Recorded<'_>> {
    let receipts = session
        .usage
        .iter()
        .map(|r| (r.agent.as_str(), r.session_id.as_str(), By::Receipt));
    let claimed = |until: Option<i64>| {
        session
            .native_env
            .iter()
            .filter(move |e| until.is_none_or(|end| e.at_ms <= end))
            .map(|e| (e.agent.as_str(), e.session_id.as_str(), By::Env))
    };
    let own: Vec<Recorded> = match &session.left {
        None => claimed(None).collect(),
        Some(left) => match &left.native {
            Some(native) => native
                .sessions
                .iter()
                .map(|n| {
                    let by = if n.source == crate::usage::ENV_SOURCE {
                        By::Env
                    } else {
                        By::LaunchPrompt
                    };
                    (n.agent.as_str(), n.session_id.as_str(), by)
                })
                .collect(),
            None => claimed(Some(left.ended_ms)).collect(),
        },
    };
    receipts.chain(own).collect()
}

/// The native sessions no `ohjaa run` launched, oldest first, and those one
/// did, each with how it was matched.
///
/// A launch that recorded the ids of its agent's own sessions ([`recorded`])
/// is matched by them alone: what leaving found by its launch prompt, a
/// usage receipt's, and what its agent's environment claimed from inside it
/// (`ohjaa oriented`, `ohjaa handover`). The sessions they name are its own,
/// wherever and whenever they ran, and it claims no other. A session named
/// by several launches (resumed in each) is each one's; it is said matched
/// by the surest way any named it: observed first, then a receipt, then the
/// agent's claim (law 3). A claim never outranks an observation: an id
/// another launch's prompt or receipt names is not the claiming launch's.
///
/// Only a launch with no id of its own falls back to time and folder (one
/// whose every claim another launch overruled has none): it may claim one
/// session of its agent (the launched command's file name), in its
/// repository or below, that started while it ran: from [`LAUNCH_EARLY_MS`]
/// before it was logged until its leaving was (to now while it runs). An
/// agent can write its first record long after its launch, so nothing
/// narrower holds.
///
/// One-to-one: each pair that could match is ranked by how well it fits,
/// best first, and taken while neither is taken. For a launch seen to end,
/// the fit is how close the session's transcript was last written to that
/// end, since its agent wrote until it exited and a session beside it
/// rarely stops then too; otherwise, and when the transcript cannot be
/// read, it is how close the session started to the launch. Ties go to the
/// closer start, then the earlier session and launch by id, so the same
/// list always gives the same answer.
pub fn judge_natives(native: Vec<Native>, logged: &[Session]) -> (Vec<Bypass>, Vec<Matched>) {
    let canonical =
        |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let ids: Vec<(&Session, Vec<Recorded>)> = logged.iter().map(|s| (s, recorded(s))).collect();
    let mut by_id: std::collections::HashMap<(&str, &str), (By, &str)> =
        std::collections::HashMap::new();
    for (s, recorded) in &ids {
        for &(agent, id, by) in recorded {
            let surest = by_id.entry((agent, id)).or_insert((by, &s.id));
            if (by, s.id.as_str()) < (surest.0, surest.1) {
                *surest = (by, &s.id);
            }
        }
    }
    // A claim counts for its launch unless an observation or a receipt of
    // another launch names that id.
    let launches: Vec<(&Session, PathBuf)> = ids
        .iter()
        .filter(|(_, recorded)| {
            !recorded.iter().any(|&(agent, id, by)| {
                by != By::Env
                    || by_id
                        .get(&(agent, id))
                        .is_some_and(|(surest, _)| *surest == By::Env)
            })
        })
        .map(|(s, _)| (*s, canonical(&s.repo)))
        .collect();
    let mut matched = Vec::new();
    let mut native: Vec<Native> = native
        .into_iter()
        .filter(|n| match by_id.get(&(n.agent.as_str(), n.id.as_str())) {
            Some((by, session)) => {
                matched.push(Matched {
                    agent: n.agent.clone(),
                    id: n.id.clone(),
                    session: (*session).to_owned(),
                    by: *by,
                });
                false
            }
            None => true,
        })
        .collect();
    native.sort_by(|a, b| (a.started_ms, &a.id).cmp(&(b.started_ms, &b.id)));
    let mut pairs = Vec::new();
    for (i, n) in native.iter().enumerate() {
        let cwd = canonical(&n.cwd);
        for (j, (s, repo)) in launches.iter().enumerate() {
            let agent = Path::new(&s.agent).file_name().and_then(|f| f.to_str());
            // Whole seconds in the log: its end may read up to one early.
            let ran = n.started_ms >= s.started_ms - LAUNCH_EARLY_MS
                && s.left
                    .as_ref()
                    .is_none_or(|left| n.started_ms < left.ended_ms + 1000);
            if agent != Some(n.agent.as_str()) || !cwd.starts_with(repo) || !ran {
                continue;
            }
            let from_start = (n.started_ms - s.started_ms).abs();
            // An orphan's end was never seen, only that it was over by then.
            let seen_end = s.left.as_ref().filter(|left| !left.orphaned);
            let fit = match (seen_end, n.written_ms) {
                (Some(left), Some(written)) => (written - left.ended_ms).abs(),
                _ => from_start,
            };
            pairs.push(((fit, from_start, i, s.id.as_str()), i, j));
        }
    }
    pairs.sort();
    let mut taken_native: Vec<Option<&str>> = vec![None; native.len()];
    let mut taken_launch = vec![false; launches.len()];
    for (_, i, j) in pairs {
        if taken_native[i].is_none() && !taken_launch[j] {
            taken_native[i] = Some(launches[j].0.id.as_str());
            taken_launch[j] = true;
        }
    }
    let mut bypasses = Vec::new();
    for (n, taken) in native.into_iter().zip(taken_native) {
        match taken {
            Some(session) => matched.push(Matched {
                agent: n.agent,
                id: n.id,
                session: session.to_owned(),
                by: By::Window,
            }),
            None => bypasses.push(Bypass {
                agent: n.agent,
                id: n.id,
                cwd: n.cwd,
                started_ms: n.started_ms,
            }),
        }
    }
    matched.sort_by(|a, b| (a.by, &a.agent, &a.id).cmp(&(b.by, &b.agent, &b.id)));
    (bypasses, matched)
}

// ---- saying it --------------------------------------------------------------

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

fn name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

fn folder(cwd: &str) -> &str {
    if cwd.is_empty() {
        "(no folder)"
    } else {
        cwd
    }
}

fn describe_check(check: &Check) -> String {
    match check {
        Check::Unchecked => "unchecked: no sykli run of it where it was made".to_owned(),
        other => sessions::describe_check(other),
    }
}

fn missing(u: &Untold) -> String {
    match (u.no_readback, u.no_handover) {
        (true, true) => "no read-back, no handover",
        (true, false) => "no read-back",
        _ => "no handover",
    }
    .to_owned()
        + if u.orphaned {
            " (its ohjaa was gone before leaving)"
        } else {
            ""
        }
}

/// What was read of the default branches and not listed, by why.
fn not_judged(j: &Judged) -> String {
    let mut parts = vec![if j.verified_reusing_cache > 0 {
        format!(
            "{} verified, {} of them reusing cached tasks",
            j.verified, j.verified_reusing_cache
        )
    } else {
        format!("{} verified", j.verified)
    }];
    if j.no_contract > 0 {
        parts.push(format!("{} without a sykli contract", j.no_contract));
    }
    if j.gone > 0 {
        parts.push(format!(
            "{} made in a checkout that is gone, with no receipt kept ({} of them merges)",
            j.gone, j.gone_merges
        ));
    }
    if j.could_not_say > 0 {
        parts.push(format!("{} sykli could not say", j.could_not_say));
    }
    if j.stale > 0 {
        parts.push(format!("{} judged on a checkout, not the commit", j.stale));
    }
    parts.join(" · ")
}

/// The report for people.
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "ohjaa report, {} to {}",
        sessions::clock(report.since_ms),
        sessions::clock(report.until_ms)
    );
    if !report.window {
        out.clear();
        let _ = writeln!(
            out,
            "ohjaa report, the marks at {}",
            sessions::clock(report.until_ms)
        );
    }
    if !report.marks.is_empty() || !report.window {
        let _ = writeln!(
            out,
            "\nMarks on each entry's branch, tip first (✓ proven · ✗ failed · ? unproven):"
        );
        for (slug, marked) in &report.marks {
            match marked {
                Ok(marks) => {
                    let _ = writeln!(out, "- {}", mark::line(marks));
                }
                Err(why) => {
                    let _ = writeln!(out, "- {slug}: not marked: {why}");
                }
            }
        }
        if report.marks.is_empty() {
            let _ = writeln!(out, "- no repository has an entry (ohjaa adopt <repo>)");
        }
    }
    if !report.window {
        say_gaps(report, &mut out);
        return out;
    }
    let _ = writeln!(
        out,
        "\nSessions without read-back or handover ({}):",
        report.untold.len()
    );
    for u in &report.untold {
        let _ = writeln!(
            out,
            "- {} · {} · {} · \"{}\" · {}: {}",
            u.id,
            u.agent,
            name(&u.repo),
            u.intent,
            sessions::clock(u.started_ms),
            missing(u)
        );
    }
    let j = &report.judged;
    let _ = writeln!(
        out,
        "\nCommits on a default branch without passing evidence ({}), of {} first-parent \
         commit(s) in {} repositor{}:",
        report.unproven.len(),
        j.commits,
        j.repos,
        if j.repos == 1 { "y" } else { "ies" }
    );
    for u in &report.unproven {
        let _ = writeln!(
            out,
            "- {} {} {} \"{}\" ({}): {}",
            name(&u.repo),
            u.on.onto,
            short(&u.on.commit),
            u.on.subject,
            sessions::clock(u.on.committed * 1000),
            describe_check(&u.on.check)
        );
    }
    if j.commits > 0 {
        let _ = writeln!(out, "  The rest: {}", not_judged(j));
    }
    match &report.natives {
        Ok(natives) => {
            let _ = writeln!(
                out,
                "\nAgent sessions a person started without ohjaa ({}):",
                natives.bypasses.len()
            );
            for b in &natives.bypasses {
                let _ = writeln!(
                    out,
                    "- {} {} in {}, started {}",
                    b.agent,
                    b.id,
                    folder(&b.cwd),
                    sessions::clock(b.started_ms)
                );
            }
            matched_lines(&natives.matched, &mut out);
            if natives.headless > 0 {
                let _ = writeln!(
                    out,
                    "  Not counted: {} headless run(s) (claude -p, codex exec, intake's drafter)",
                    natives.headless
                );
            }
            if !natives.undated.is_empty() {
                let _ = writeln!(
                    out,
                    "  Not judged, vessa could not date or place them ({}):",
                    natives.undated.len()
                );
                for u in &natives.undated {
                    let written = u
                        .written_ms
                        .map_or_else(|| "unknown".to_owned(), sessions::clock);
                    let _ = writeln!(
                        out,
                        "  - {} {} (transcript {}, written {written})",
                        u.agent, u.id, u.source
                    );
                }
            }
        }
        Err(why) => {
            let _ = writeln!(
                out,
                "\nAgent sessions a person started without ohjaa: not judged: {why}"
            );
        }
    }
    say_gaps(report, &mut out);
    out
}

/// How the sessions launched through ohjaa were told apart, counted by how;
/// each one matched by time and folder alone is named, since only that is
/// inferred rather than recorded.
fn matched_lines(matched: &[Matched], out: &mut String) {
    if matched.is_empty() {
        return;
    }
    let count = |by: By| matched.iter().filter(|m| m.by == by).count();
    let counts: Vec<String> = [
        (By::LaunchPrompt, "by its launch prompt"),
        (By::Receipt, "by a usage receipt"),
        (By::Env, "claimed by the agent (its environment's id)"),
        (By::Window, "by time and folder alone"),
    ]
    .into_iter()
    .filter(|(by, _)| count(*by) > 0)
    .map(|(by, how)| format!("{} {how}", count(by)))
    .collect();
    let _ = writeln!(
        out,
        "  Launched through ohjaa ({}): {}",
        matched.len(),
        counts.join(", ")
    );
    for m in matched.iter().filter(|m| m.by == By::Window) {
        let _ = writeln!(
            out,
            "  - {} {} taken as {}'s by time and folder: it recorded no id",
            m.agent, m.id, m.session
        );
    }
}

fn say_gaps(report: &Report, out: &mut String) {
    if !report.gaps.is_empty() {
        let _ = writeln!(
            out,
            "\nNot read or not judged, so absence above proves nothing there:"
        );
        for gap in &report.gaps {
            let _ = writeln!(out, "- {gap}");
        }
    }
}

/// `ohjaa report --json`: `ohjaa-marks.v1` (the README's "ohjaa-marks.v1"
/// pins it). The marks on every entry's branch, what waits for a person,
/// the sessions still open or orphaned, and the window's report as
/// `ohjaa-report.v1` under `report` (`null` with `--marks`). Computing it
/// writes nothing.
pub fn to_json(report: &Report) -> Value {
    let rfc = |ms: i64| intake::rfc3339(ms.div_euclid(1000));
    let entries: Vec<Value> = report
        .marks
        .iter()
        .map(|(slug, marked)| match marked {
            Ok(marks) => json!({
                "repo": marks.gate.slug,
                "branch": marks.gate.branch,
                "ref": format!("origin/{}", marks.gate.branch),
                "tip": marks.commits.first().map(|tip| tip.commit.as_str()),
                "clone": marks.gate.top,
                "updated": marks.updated.as_ref().map(|at| rfc(at.ms)),
                "updated_by": marks.updated.as_ref().and_then(|at| at.by),
                "ended": marks.ended.word(),
                "proven_at_tip": marks.proven_at_tip(),
                // Additive (KATANA D5): every commit here is on the observed
                // branch, so its mark is the integrated State's; and the
                // integrated validations sessions asked sykli for, of these
                // commits, by reference.
                "integrated": json!({
                    "commit": marks.commits.first().map(|tip| tip.commit.as_str()),
                    "status": marks.commits.first().map_or("unverified", |tip| integrated_word(&tip.check)),
                    "requested": report
                        .integrated
                        .iter()
                        .filter(|(_, v)| marks.commits.iter().any(|c| c.commit == v.commit))
                        .map(|(session, v)| json!({
                            "request": v.request,
                            "run": v.run,
                            "commit": v.commit,
                            "tree": v.tree,
                            "requested": rfc(v.requested_ms),
                            "session": session,
                            "checkout": v.repo,
                            "not_dispatched": v.not_dispatched,
                        }))
                        .collect::<Vec<_>>(),
                }),
                "commits": marks.commits.iter().map(|walked| json!({
                    "sha": walked.commit,
                    "tree": walked.tree,
                    "mark": mark::word(&walked.check),
                    "code": mark::code(&walked.check),
                    "why": mark::why(&walked.check),
                    "integrated": integrated_word(&walked.check),
                    // For a proven commit, the accepted receipt's tasks and
                    // how many of them it reused from sykli's cache.
                    "tasks": walked.proof.as_ref().map(|proof| proof.tasks),
                    "cached": walked.proof.as_ref().map(|proof| proof.cached),
                    // Additive (G1a): for a proven commit, the contract that
                    // receipt ran under: the gate derived from the commit
                    // ("derived"), its own sykli.json ("own"), or the entry's
                    // stored copy ("stored").
                    "contract": walked.under.map(crate::derived::Under::word),
                    "author": walked.author,
                    "authored": rfc(walked.authored * 1000),
                    "subject": walked.subject,
                })).collect::<Vec<_>>(),
                "error": Value::Null,
            }),
            Err(why) => json!({
                "repo": slug,
                "branch": Value::Null,
                "ref": Value::Null,
                "tip": Value::Null,
                "clone": Value::Null,
                "updated": Value::Null,
                "updated_by": Value::Null,
                "ended": Value::Null,
                "proven_at_tip": false,
                "integrated": Value::Null,
                "commits": [],
                "error": why,
            }),
        })
        .collect();
    let session = |s: &Untold| {
        json!({
            "id": s.id,
            "agent": s.agent,
            "repo": s.repo,
            "intent": s.intent,
            "started": rfc(s.started_ms),
            "state": if s.orphaned {
                "orphaned"
            } else if s.ohjaa_gone {
                "ohjaa_gone"
            } else {
                "open"
            },
        })
    };
    json!({
        "schema": "ohjaa-marks.v1",
        "generated": rfc(report.until_ms),
        "complete": report.complete(),
        "entries": entries,
        "proposals": match &report.proposals {
            Ok(waiting) => json!({
                "listed": true,
                "waiting": waiting.iter().map(|w| json!({
                    "scope": w.scope,
                    "name": w.name,
                    "title": w.title,
                    "since": w.since,
                    "repo": w.repo,
                })).collect::<Vec<_>>(),
            }),
            Err(why) => json!({"listed": false, "why": why, "waiting": []}),
        },
        "sessions": report.open.iter().map(session).collect::<Vec<_>>(),
        "report": if report.window { report_json(report) } else { Value::Null },
    })
}

/// A commit on the observed branch is the integrated State: `passed`,
/// `failed` or `unverified`, from its mark (sykli's word on a receipt of
/// exactly its tree; a candidate's result counts only where the trees are
/// the same).
fn integrated_word(check: &Check) -> &'static str {
    match check {
        Check::Passed => "passed",
        Check::Failed { .. } => "failed",
        _ => "unverified",
    }
}

/// The window's report as `ohjaa-report.v1` JSON.
fn report_json(report: &Report) -> Value {
    let rfc = |ms: i64| intake::rfc3339(ms.div_euclid(1000));
    let j = &report.judged;
    json!({
        "schema": "ohjaa-report.v1",
        "since": rfc(report.since_ms),
        "until": rfc(report.until_ms),
        "complete": report.complete(),
        "sessions_without_testimony": report.untold.iter().map(|u| json!({
            "id": u.id,
            "agent": u.agent,
            "repo": u.repo,
            "intent": u.intent,
            "started": rfc(u.started_ms),
            "no_readback": u.no_readback,
            "no_handover": u.no_handover,
            "orphaned": u.orphaned,
        })).collect::<Vec<_>>(),
        "unproven_commits": report.unproven.iter().map(|u| json!({
            "repo": u.repo,
            "onto": u.on.onto,
            "commit": u.on.commit,
            "subject": u.on.subject,
            "committed": rfc(u.on.committed * 1000),
            "check": match &u.on.check {
                Check::Failed { .. } => "failed",
                Check::OtherContract { .. } => "other_contract",
                _ => "unchecked",
            },
            "why": describe_check(&u.on.check),
        })).collect::<Vec<_>>(),
        "default_branch_commits": {
            "repositories": j.repos,
            "commits": j.commits,
            "verified": j.verified,
            "verified_reusing_cache": j.verified_reusing_cache,
            "no_contract": j.no_contract,
            "checkout_gone": j.gone,
            "checkout_gone_merges": j.gone_merges,
            "sykli_could_not_say": j.could_not_say,
            "stale": j.stale,
        },
        "bypasses": match &report.natives {
            Ok(natives) => json!({
                "judged": true,
                "sessions": natives.bypasses.iter().map(|b| json!({
                    "agent": b.agent,
                    "id": b.id,
                    "cwd": b.cwd,
                    "started": rfc(b.started_ms),
                })).collect::<Vec<_>>(),
                // Additive: the sessions launched through ohjaa, and how each
                // was matched to its launch.
                "matched": natives.matched.iter().map(|m| json!({
                    "agent": m.agent,
                    "id": m.id,
                    "session": m.session,
                    "by": m.by.word(),
                    // The agent's claim or a receipt's word, not what ohjaa
                    // observed.
                    "testimony": m.by.testimony(),
                })).collect::<Vec<_>>(),
                "headless": natives.headless,
                "undated": natives.undated.iter().map(|u| json!({
                    "agent": u.agent,
                    "id": u.id,
                    "source": u.source,
                    "written": u.written_ms.map(rfc),
                })).collect::<Vec<_>>(),
            }),
            Err(why) => json!({"judged": false, "why": why}),
        },
        "gaps": report.gaps,
    })
}

/// What left a report incomplete, said so a person can act on it.
fn first_gap(report: &Report) -> Option<String> {
    match &report.natives {
        Err(why) if why.starts_with("vessa is not on PATH") => Some(
            "vessa is not on PATH — install vessa to see sessions started without ohjaa".to_owned(),
        ),
        Err(why) => Some(why.clone()),
        Ok(_) => report.gaps.first().cloned(),
    }
}

/// Three lines for the person, after a session's record.
pub fn summary(report: &Report) -> String {
    let bypassed = match &report.natives {
        Ok(natives) => format!(
            "{} agent session(s) started without ohjaa",
            natives.bypasses.len()
        ),
        // Why is said on the last line, once.
        Err(_) => "sessions started without ohjaa not judged".to_owned(),
    };
    // A refusal first, then a commit never checked, then a session.
    let first = report
        .unproven
        .iter()
        .find(|u| matches!(u.on.check, Check::Failed { .. }))
        .or_else(|| report.unproven.first())
        .map(|u| {
            // On the branch's history, not necessarily where it is now.
            format!(
                "{} on {} in {}: {}",
                short(&u.on.commit),
                u.on.onto,
                name(&u.repo),
                describe_check(&u.on.check)
            )
        })
        .or_else(|| {
            report.untold.first().map(|u| {
                format!(
                    "session {} ({}, {}): {}",
                    u.id,
                    u.agent,
                    name(&u.repo),
                    missing(u)
                )
            })
        })
        .unwrap_or_else(|| "nothing went around the workflow that ohjaa could see".to_owned());
    let last = match first_gap(report) {
        None => "`ohjaa report` lists them all; the next summary is in 7 days.".to_owned(),
        Some(gap) => {
            let more = report.gaps.len() + usize::from(report.natives.is_err()) - 1;
            format!(
                "incomplete: {gap}{}; `ohjaa report` lists it all, and it is said again \
                 tomorrow.",
                if more > 0 {
                    format!(" (and {more} more)")
                } else {
                    String::new()
                }
            )
        }
    };
    format!(
        "ohjaa: weekly report, {} to {}: {} session(s) without read-back or handover · {} \
         commit(s) on a default branch without passing evidence · {bypassed}\n\
         ohjaa:   first: {first}\n\
         ohjaa:   {last}\n",
        &sessions::clock(report.since_ms)[..10],
        &sessions::clock(report.until_ms)[..10],
        report.untold.len(),
        report.unproven.len(),
    )
}

// ---- intake ------------------------------------------------------------------

/// The report's real failures and bypasses as intake candidates: never what
/// could not be checked or judged. A missing handover is intake's already,
/// from the session itself; a missing read-back is not.
pub fn candidates(report: &Report) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut push = |at_ms: i64, place: &str, context: String, text: String| {
        out.push(Candidate {
            source: "observed",
            at_ms,
            at: intake::rfc3339(at_ms.div_euclid(1000)),
            place: place.to_owned(),
            who: "ohjaa".to_owned(),
            context,
            text,
        });
    };
    for u in report
        .untold
        .iter()
        .filter(|u| u.no_readback && !u.orphaned)
    {
        push(
            u.started_ms,
            &u.repo,
            format!(
                "session {} ({}, {}), observed by ohjaa, not said by anyone",
                u.id, u.agent, u.intent
            ),
            format!(
                "in session {}: the agent never read back its briefing (`ohjaa oriented`)",
                u.id
            ),
        );
    }
    for u in &report.unproven {
        push(
            u.on.committed * 1000,
            &u.repo,
            format!(
                "{} on {} in {}, observed by ohjaa, not said by anyone",
                short(&u.on.commit),
                u.on.onto,
                name(&u.repo)
            ),
            format!(
                "{} \"{}\" is on {} in {} without passing evidence ({})",
                short(&u.on.commit),
                u.on.subject,
                u.on.onto,
                name(&u.repo),
                describe_check(&u.on.check)
            ),
        );
    }
    if let Ok(natives) = &report.natives {
        for b in &natives.bypasses {
            push(
                b.started_ms,
                &b.cwd,
                format!(
                    "{} session {}, observed by ohjaa through vessa, not said by anyone",
                    b.agent, b.id
                ),
                format!(
                    "a {} session ({}) in {} was started without `ohjaa run`: it was briefed \
                     with no knowledge and left no record",
                    b.agent,
                    b.id,
                    folder(&b.cwd)
                ),
            );
        }
    }
    out
}

// ---- once a week -------------------------------------------------------------

/// How often a report with a gap is said again: a gap that does not go away
/// (vessa too old, say) must not follow every session.
pub const DAY_MS: i64 = 86_400_000;

/// When a complete report was last shown to a person: the week's.
fn marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join("report-at")
}

/// When any report was last shown to a person, complete or not.
fn shown_marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join("report-shown-at")
}

fn read_time(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

fn write_time(dirs: &Dirs, path: &Path, now_ms: i64) -> Result<()> {
    std::fs::create_dir_all(&dirs.state)
        .with_context(|| format!("cannot create {}", dirs.state.display()))?;
    std::fs::write(path, format!("{now_ms}\n"))
        .with_context(|| format!("cannot write {}", path.display()))
}

/// When a complete report was last shown to a person, if one was.
pub fn last_made(dirs: &Dirs) -> Option<i64> {
    read_time(&marker(dirs))
}

/// Record that a report was shown to a person at `now_ms`; a complete one
/// is also the week's.
pub fn shown(dirs: &Dirs, now_ms: i64, complete: bool) -> Result<()> {
    write_time(dirs, &shown_marker(dirs), now_ms)?;
    if complete {
        write_time(dirs, &marker(dirs), now_ms)?;
    }
    Ok(())
}

/// Whether a report is due: a week since a complete one was shown (or none
/// was), and a day since any was, so one with a gap is said again at most
/// once a day.
pub fn due(dirs: &Dirs, now_ms: i64) -> bool {
    last_made(dirs).is_none_or(|at| now_ms - at >= WEEK_MS)
        && read_time(&shown_marker(dirs)).is_none_or(|at| now_ms - at >= DAY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, agent: &str, repo: &str, started_ms: i64) -> Session {
        serde_json::from_value(json!({
            "id": id,
            "agent": agent,
            "repo": repo,
            "intent": "x",
            "started_ms": started_ms,
            "delivered": [],
        }))
        .unwrap()
    }

    fn native(agent: &str, id: &str, cwd: &str, started_ms: i64) -> Native {
        Native {
            agent: agent.to_owned(),
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            started_ms,
            written_ms: None,
        }
    }

    fn last_written(native: Native, written_ms: i64) -> Native {
        Native {
            written_ms: Some(written_ms),
            ..native
        }
    }

    fn bypasses(native: Vec<Native>, logged: &[Session]) -> Vec<Bypass> {
        judge_natives(native, logged).0
    }

    /// `session` as left at `ended_ms`, its leaving observed.
    fn ended(mut session: Session, ended_ms: i64) -> Session {
        let mut left = sessions::Left::orphaned(ended_ms);
        left.orphaned = false;
        session.left = Some(left);
        session
    }

    fn ids(list: Vec<Bypass>) -> Vec<String> {
        list.into_iter().map(|b| b.id).collect()
    }

    #[test]
    fn a_native_session_is_ohjaas_when_launched_by_it_here_and_then() {
        // It ran for ten minutes.
        let launched = ended(
            session("oh-1", "claude", "/nonexistent/r/app", 1_000_000),
            1_600_000,
        );
        // Its agent, in its repository (a folder below it too), seconds after.
        assert!(bypasses(
            vec![native("claude", "a", "/nonexistent/r/app/src", 1_003_000)],
            std::slice::from_ref(&launched)
        )
        .is_empty());
        // Another agent, another place, or after it ended: not launched by it.
        assert_eq!(
            ids(bypasses(
                vec![
                    native("codex", "b", "/nonexistent/r/app", 1_003_000),
                    native("claude", "c", "/nonexistent/r/other", 1_003_000),
                    native("claude", "d", "/nonexistent/r/app", 1_000_000 + 3_600_000),
                ],
                std::slice::from_ref(&launched)
            )),
            ["b", "c", "d"]
        );
        // One launch is one session: a second one started then is not it.
        assert_eq!(
            ids(bypasses(
                vec![
                    native("claude", "e", "/nonexistent/r/app", 1_002_000),
                    native("claude", "f", "/nonexistent/r/app", 1_004_000),
                ],
                std::slice::from_ref(&launched)
            )),
            ["f"]
        );
        // A usage receipt names it outright, wherever and whenever.
        let mut receipted = session("oh-2", "/usr/bin/claude", "/elsewhere", 0);
        receipted.usage = vec![serde_json::from_value(json!({
            "schema": "agent-usage.v1", "agent": "claude", "session_id": "g", "source": "s",
            "observed_at_ms": 0, "complete": true, "limitations": [], "entries": []
        }))
        .unwrap()];
        assert!(bypasses(
            vec![native("claude", "g", "/nonexistent/r/app", 9_000_000)],
            &[receipted]
        )
        .is_empty());
        // pi by the same rule: its launch claims it, and a claude launch
        // claims no pi session, nor a pi launch a claude one.
        let pi = session("oh-3", "pi", "/nonexistent/r/app", 2_000_000);
        assert!(bypasses(
            vec![native("pi", "h", "/nonexistent/r/app", 2_004_000)],
            std::slice::from_ref(&pi)
        )
        .is_empty());
        assert_eq!(
            ids(bypasses(
                vec![
                    native("pi", "i", "/nonexistent/r/app", 1_003_000),
                    native("claude", "j", "/nonexistent/r/app", 2_003_000),
                ],
                &[launched, pi]
            )),
            ["i", "j"]
        );
    }

    /// A launch that recorded its agent's own session ids is matched by them
    /// alone, wherever and whenever those ran: what ohjaa observed (the
    /// launch prompt) first, then what the agent's environment claimed. Only
    /// a launch that recorded none falls back to time and folder. Each match
    /// says how it was made, a claim as a claim.
    #[test]
    fn a_recorded_id_matches_exactly_and_only_a_launch_without_one_falls_back() {
        let repo = "/nonexistent/r/app";
        // Left, its session named by its launch prompt, and one more claimed
        // by its environment after a `/clear`.
        let mut prompt = ended(session("oh-1", "claude", repo, 1_000_000), 1_600_000);
        prompt.left.as_mut().unwrap().native = Some(
            serde_json::from_value(json!({"sessions": [
                {"agent": "claude", "session_id": "by-prompt", "source": "/t/1.jsonl"},
                {"agent": "claude", "session_id": "cleared", "source": "env", "env": true,
                 "testimony": true},
            ]}))
            .unwrap(),
        );
        // Still open, its session named by its environment.
        let mut open = session("oh-2", "codex", repo, 1_000_000);
        open.native_env = vec![crate::usage::FromEnv {
            agent: "codex".to_owned(),
            session_id: "by-env".to_owned(),
            at_ms: 1_001_000,
        }];
        // Recorded nothing.
        let bare = ended(session("oh-3", "pi", repo, 1_000_000), 1_600_000);
        let (bypassed, matched) = judge_natives(
            vec![
                // Elsewhere and hours later: still its own.
                native("claude", "by-prompt", "/nonexistent/elsewhere", 9_000_000),
                native("claude", "cleared", repo, 1_300_000),
                // Its agent, its folder, its time: not its own, since it
                // recorded the ids of its own.
                native("claude", "beside", repo, 1_002_000),
                native("codex", "by-env", repo, 50_000_000),
                native("codex", "codex-beside", repo, 1_002_000),
                native("pi", "own", repo, 1_003_000),
            ],
            &[prompt, open, bare],
        );
        assert_eq!(ids(bypassed), ["beside", "codex-beside"]);
        let how: Vec<(&str, &str, &str)> = matched
            .iter()
            .map(|m| (m.id.as_str(), m.session.as_str(), m.by.word()))
            .collect();
        assert_eq!(
            how,
            [
                ("by-prompt", "oh-1", "launch_prompt"),
                ("cleared", "oh-1", "env"),
                ("by-env", "oh-2", "env"),
                ("own", "oh-3", "time_and_folder"),
            ]
        );
        assert!(matched
            .iter()
            .all(|m| m.by.testimony() == (m.by == By::Env)));
        // The fallback is named in the report, the rest counted, a claim
        // said as the agent's.
        let mut out = String::new();
        matched_lines(&matched, &mut out);
        assert_eq!(
            out,
            "  Launched through ohjaa (4): 1 by its launch prompt, 2 claimed by the agent (its \
             environment's id), 1 by time and folder alone\n  \
             - pi own taken as oh-3's by time and folder: it recorded no id\n"
        );
    }

    /// Review of #57: a shell that kept a session's OHJAA_SESSION after it
    /// left (a tmux server started inside it) can run `ohjaa handover` in a
    /// session a person starts by hand days later. An id its environment
    /// named after the session left is not the session's: that session is
    /// still a bypass, and the old launch still falls back to its own.
    #[test]
    fn an_id_named_after_a_session_left_clears_no_bypass() {
        let repo = "/nonexistent/r/app";
        let stale = crate::usage::FromEnv {
            agent: "claude".to_owned(),
            session_id: "by-hand".to_owned(),
            at_ms: 9_000_000,
        };
        let mut found_none = ended(session("oh-1", "claude", repo, 1_000_000), 1_600_000);
        found_none.left.as_mut().unwrap().native = Some(
            serde_json::from_value(
                json!({"why": "none found: none of the 0 claude transcript(s)"}),
            )
            .unwrap(),
        );
        let older = ended(session("oh-1", "claude", repo, 1_000_000), 1_600_000);
        for mut left in [found_none, older] {
            left.native_env = vec![stale.clone()];
            let (bypassed, matched) = judge_natives(
                vec![
                    native("claude", "own", repo, 1_003_000),
                    native("claude", "by-hand", "/nonexistent/elsewhere", 9_000_000),
                ],
                &[left],
            );
            assert_eq!(ids(bypassed), ["by-hand"]);
            assert_eq!(matched.len(), 1);
            assert_eq!((matched[0].id.as_str(), matched[0].by), ("own", By::Window));
        }
    }

    /// Review of #57: the agent's environment is its claim, and any shell can
    /// set it. A claim of an id another launch's prompt was observed in is
    /// not taken: the id is the observing launch's, and the claiming launch,
    /// left with no id of its own, falls back to time and folder.
    #[test]
    fn an_environments_claim_never_outranks_an_observed_launch_prompt() {
        let repo = "/nonexistent/r/app";
        let mut observed = ended(session("oh-a", "claude", repo, 1_000_000), 1_600_000);
        observed.left.as_mut().unwrap().native = Some(
            serde_json::from_value(json!({"sessions": [
                {"agent": "claude", "session_id": "x", "source": "/t/x.jsonl"},
            ]}))
            .unwrap(),
        );
        let mut forged = session("oh-b", "claude", repo, 2_000_000);
        forged.native_env = vec![crate::usage::FromEnv {
            agent: "claude".to_owned(),
            session_id: "x".to_owned(),
            at_ms: 2_001_000,
        }];
        for logged in [
            vec![observed.clone(), forged.clone()],
            vec![forged.clone(), observed.clone()],
        ] {
            let (bypassed, matched) = judge_natives(
                vec![
                    native("claude", "x", repo, 1_002_000),
                    native("claude", "b-own", repo, 2_003_000),
                ],
                &logged,
            );
            assert!(ids(bypassed).is_empty());
            let how: Vec<(&str, &str, By)> = matched
                .iter()
                .map(|m| (m.id.as_str(), m.session.as_str(), m.by))
                .collect();
            assert_eq!(
                how,
                [
                    ("x", "oh-a", By::LaunchPrompt),
                    ("b-own", "oh-b", By::Window)
                ]
            );
        }
    }

    /// A session resumed in two launches is named by both: matched by the
    /// surest way either named it, and a bypass of neither.
    #[test]
    fn a_session_two_launches_named_is_matched_the_surest_way() {
        let repo = "/nonexistent/r/app";
        let named = |id: &str, source: &str| {
            let mut s = ended(session(id, "claude", repo, 1_000_000), 1_600_000);
            s.left.as_mut().unwrap().native = Some(
                serde_json::from_value(json!({"sessions": [
                    {"agent": "claude", "session_id": "resumed", "source": source, "env": true},
                ]}))
                .unwrap(),
            );
            s
        };
        for logged in [
            [named("oh-1", "env"), named("oh-2", "/t.jsonl")],
            [named("oh-2", "/t.jsonl"), named("oh-1", "env")],
        ] {
            let (bypassed, matched) =
                judge_natives(vec![native("claude", "resumed", repo, 500_000)], &logged);
            assert!(bypassed.is_empty());
            assert_eq!(matched.len(), 1);
            assert_eq!(
                (matched[0].session.as_str(), matched[0].by),
                ("oh-2", By::LaunchPrompt),
                "observed, not claimed"
            );
        }
    }

    /// An agent can write its first record long after its launch (Claude at
    /// the first prompt: 96 s in syva on 10-20, Codex 115 s and 188 s): what
    /// starts while a launch runs can be its own, and nothing after it ended.
    #[test]
    fn a_launch_claims_what_starts_while_it_runs() {
        let repo = "/nonexistent/r/syva";
        let day = 86_400_000;
        let launched = ended(
            session("oh-1", "claude", repo, 1_000_000),
            1_000_000 + 2 * day,
        );
        assert!(
            bypasses(
                vec![native("claude", "own", repo, 1_096_000)],
                std::slice::from_ref(&launched)
            )
            .is_empty(),
            "started 96 s into a launch that ran two days"
        );
        // A launch that was over in half a minute claims nothing after.
        let short = ended(session("oh-2", "codex", repo, 1_000_000), 1_030_000);
        assert_eq!(
            ids(bypasses(
                vec![native("codex", "later", repo, 1_045_000)],
                &[short]
            )),
            ["later"]
        );
        // One still running claims what starts hours in, once.
        let open = session("oh-3", "pi", repo, 1_000_000);
        assert_eq!(
            ids(bypasses(
                vec![
                    native("pi", "own", repo, 1_000_000 + 7_200_000),
                    native("pi", "second", repo, 1_000_000 + 7_300_000),
                ],
                &[open]
            )),
            ["second"]
        );
    }

    /// A launch a usage receipt names a session of is matched by its receipt
    /// alone: it never also clears a session by time.
    #[test]
    fn a_launch_with_a_receipt_claims_no_other_session() {
        let repo = "/nonexistent/r/app";
        let mut launched = ended(session("oh-1", "claude", repo, 1_000_000), 1_600_000);
        launched.usage = vec![serde_json::from_value(json!({
            "schema": "agent-usage.v1", "agent": "claude", "session_id": "mine", "source": "s",
            "observed_at_ms": 0, "complete": true, "limitations": [], "entries": []
        }))
        .unwrap()];
        for (mine, by_hand) in [(1_003_000, 1_020_000), (1_004_000, 1_001_000)] {
            assert_eq!(
                ids(bypasses(
                    vec![
                        native("claude", "mine", repo, mine),
                        native("claude", "by-hand", repo, by_hand),
                    ],
                    std::slice::from_ref(&launched)
                )),
                ["by-hand"],
                "its own at {mine}, one by hand at {by_hand}"
            );
        }
        // Its own session outside the window still is its own: the launch
        // clears nothing else.
        assert_eq!(
            ids(bypasses(
                vec![native("claude", "by-hand", repo, 1_002_000)],
                std::slice::from_ref(&launched)
            )),
            ["by-hand"]
        );
    }

    /// Two sessions of one agent start while one launch runs. The one whose
    /// transcript was last written as the launch ended is its own, whichever
    /// started first; without an end seen, the closer start is.
    #[test]
    fn of_two_sessions_during_a_launch_the_one_written_until_it_ended_is_its_own() {
        let repo = "/nonexistent/r/app";
        let end = 1_600_000;
        let launched = ended(session("oh-1", "pi", repo, 1_000_000), end);
        let own = |at: i64| last_written(native("pi", "own", repo, at), end - 1_000);
        let by_hand = |at: i64| last_written(native("pi", "by-hand", repo, at), at + 2_000);
        for (own_at, by_hand_at) in [(1_004_000, 1_001_000), (1_001_000, 1_004_000)] {
            for listed in [
                vec![own(own_at), by_hand(by_hand_at)],
                vec![by_hand(by_hand_at), own(own_at)],
            ] {
                assert_eq!(
                    ids(bypasses(listed, std::slice::from_ref(&launched))),
                    ["by-hand"],
                    "own at {own_at}, by hand at {by_hand_at}"
                );
            }
        }
        // A session by hand that goes on after the launch ended is not it
        // either, though it started closer.
        let outlived = last_written(native("pi", "by-hand", repo, 1_001_000), end + 3_600_000);
        assert_eq!(
            ids(bypasses(
                vec![outlived, own(1_004_000)],
                std::slice::from_ref(&launched)
            )),
            ["by-hand"]
        );
        // Still running, or closed as an orphan (its end never seen): the
        // closer start, the same either way the list comes.
        let mut orphan = launched.clone();
        orphan.left.as_mut().unwrap().orphaned = true;
        for launch in [session("oh-2", "pi", repo, 1_000_000), orphan] {
            for listed in [
                vec![own(1_001_000), by_hand(1_004_000)],
                vec![by_hand(1_004_000), own(1_001_000)],
            ] {
                assert_eq!(
                    ids(bypasses(listed, std::slice::from_ref(&launch))),
                    ["by-hand"]
                );
            }
        }
        // Two launches and their two sessions: each its own.
        let second = ended(session("oh-3", "pi", repo, 1_010_000), 1_300_000);
        assert!(bypasses(
            vec![
                last_written(native("pi", "a", repo, 1_003_000), end - 1_000),
                last_written(native("pi", "b", repo, 1_013_000), 1_299_000),
            ],
            &[launched, second]
        )
        .is_empty());
    }

    #[test]
    fn vessas_list_keeps_what_a_person_started_and_says_what_it_could_not_date() {
        let since = intake::parse_time("2026-10-03T00:00:00Z").unwrap();
        let now = intake::parse_time("2026-10-10T00:00:00Z").unwrap();
        let s = |id: &str, agent: &str, extra: Value| {
            let mut s = json!({"agent": agent, "id": id, "parent": null,
                "started_at": "2026-10-09T10:00:00.500Z", "cwd": "/r/app",
                "source": "/nonexistent/transcript.jsonl",
                "entrypoint": null, "originator": null});
            for (key, value) in extra.as_object().unwrap() {
                s[key] = value.clone();
            }
            s
        };
        let listed = json!({"schema": "vessa-session-headers.v1", "warnings": [], "sessions": [
            s("tui", "claude", json!({"entrypoint": "cli"})),
            s("sub", "claude", json!({"parent": "tui"})),
            s("pi", "pi", json!({})),
            s("pi-sub", "pi", json!({"parent": "pi"})),
            s("other", "opencode", json!({})),
            s("old", "codex", json!({"started_at": "2026-09-01T10:00:00Z"})),
            s("headless", "claude", json!({"entrypoint": "sdk-cli"})),
            s("exec", "codex", json!({"originator": "codex_exec"})),
            s("undated", "claude", json!({"started_at": null, "cwd": ""})),
            s("nowhere", "codex", json!({"cwd": ""})),
        ]});
        let (judgeable, natives) = parse_native(&listed, since, now).unwrap();
        let at = since + 6 * 86_400_000 + 36_000_500;
        assert_eq!(
            judgeable,
            [
                native("claude", "tui", "/r/app", at),
                native("pi", "pi", "/r/app", at)
            ],
            "a person's pi session is judged too; opencode, which vessa does not read, is not"
        );
        assert_eq!(
            natives.headless, 2,
            "claude -p and codex exec bypass nothing"
        );
        let undated: Vec<&str> = natives.undated.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(
            undated,
            ["undated", "nowhere"],
            "never dropped: said apart, not judged"
        );
        // Only the header listing is read: the full one, or none named, is
        // not judged.
        let full = json!({"schema": "vessa-usage.v1", "warnings": [], "sessions": []});
        assert_eq!(
            parse_native(&full, since, now).unwrap_err(),
            "vessa listed sessions as `vessa-usage.v1`, not `vessa-session-headers.v1`"
        );
        assert!(parse_native(&json!({"sessions": []}), since, now)
            .unwrap_err()
            .contains("names no schema"));
        assert!(parse_native(
            &json!({"schema": "vessa-session-headers.v1", "nothing": []}),
            since,
            now
        )
        .is_err());
    }

    #[test]
    fn a_summary_is_due_once_a_week() {
        let root = std::env::temp_dir().join(format!("ohjaa-weekly-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: root.join("mine"),
            machine: root.join("machine"),
            proposals: root.join("proposals"),
            state: root.join("state"),
        };
        let now = 100 * 86_400_000;
        assert!(due(&dirs, now), "never shown: due");
        // One with a gap is said again, but at most once a day.
        shown(&dirs, now, false).unwrap();
        assert_eq!(last_made(&dirs), None, "an incomplete one marks no week");
        assert!(!due(&dirs, now + DAY_MS - 1));
        assert!(due(&dirs, now + DAY_MS));
        // A complete one is the week's.
        let later = now + DAY_MS;
        shown(&dirs, later, true).unwrap();
        assert_eq!(last_made(&dirs), Some(later));
        assert!(!due(&dirs, later + WEEK_MS - 1));
        assert!(due(&dirs, later + WEEK_MS));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_window_is_days_hours_or_a_time() {
        let now = 10 * 86_400_000;
        assert_eq!(parse_since("7d", now).unwrap(), 3 * 86_400_000);
        assert_eq!(parse_since("24h", now).unwrap(), 9 * 86_400_000);
        assert_eq!(parse_since("1970-01-02", now).unwrap(), 86_400_000);
        assert!(parse_since("0d", now).is_err());
        assert!(parse_since("soon", now).is_err());
    }

    fn sample(natives: Result<Natives, String>) -> Report {
        let on = |check: Check| lifecycle::OnDefault {
            onto: "origin/main".to_owned(),
            commit: "c0ffee1234".to_owned(),
            subject: "s".to_owned(),
            committed: 100,
            merge: false,
            check,
        };
        let untold = |id: &str, no_readback: bool| Untold {
            id: id.to_owned(),
            agent: "claude".to_owned(),
            repo: "/r/app".to_owned(),
            intent: "x".to_owned(),
            started_ms: 5,
            no_readback,
            no_handover: true,
            orphaned: false,
            ohjaa_gone: false,
        };
        Report {
            since_ms: 0,
            until_ms: 1,
            untold: vec![untold("oh-1", true), untold("oh-2", false)],
            unproven: vec![Unproven {
                repo: "/r/app".to_owned(),
                on: on(Check::Failed {
                    why: "refused: outcome".to_owned(),
                }),
            }],
            judged: Judged::default(),
            natives,
            marks: Vec::new(),
            proposals: Ok(Vec::new()),
            open: Vec::new(),
            gaps: Vec::new(),
            window: true,
            integrated: Vec::new(),
        }
    }

    /// `ohjaa report --json` is `ohjaa-marks.v1`, read by other tools
    /// (viemäri): renaming or dropping a field breaks them silently.
    #[test]
    fn the_marks_json_keeps_its_v1_shape() {
        let gate = lifecycle::Gate {
            slug: "acme/widget".to_owned(),
            branch: "main".to_owned(),
            entry: PathBuf::from("/h/repos/acme/widget"),
            overlay: crate::derived::Overlay::standard(),
            top: PathBuf::from("/r/widget"),
            common: PathBuf::from("/r/widget/.git"),
        };
        let walked = |sha: &str, check: Check| mark::Walked {
            commit: sha.to_owned(),
            tree: format!("tree-{sha}"),
            author: "Ann".to_owned(),
            authored: 60,
            subject: format!("subject {sha}"),
            check,
            proof: None,
            under: None,
        };
        let mut report = sample(Err("vessa is not on PATH".to_owned()));
        report.marks = vec![
            (
                "acme/widget".to_owned(),
                Ok(mark::Marks {
                    gate,
                    updated: Some(mark::Updated {
                        ms: 5_000,
                        by: Some("fetch"),
                    }),
                    commits: vec![
                        walked("c3", Check::Unchecked),
                        walked(
                            "c2",
                            Check::Failed {
                                why: "refused: outcome".to_owned(),
                            },
                        ),
                        mark::Walked {
                            // Proven by a receipt that reused one of its two tasks.
                            proof: Some(crate::sessions::Evidence {
                                outcome: "passed".to_owned(),
                                dirty: false,
                                tasks: 2,
                                cached: 1,
                            }),
                            // A run of the commit's own sykli.json (CI's).
                            under: Some(crate::derived::Under::Own),
                            ..walked("c1", Check::Passed)
                        },
                    ],
                    ended: mark::Ended::Proven,
                }),
            ),
            (
                "acme/gone".to_owned(),
                Err("no clone under the roots has it as origin".to_owned()),
            ),
        ];
        report.proposals = Ok(vec![Waiting {
            scope: "mine".to_owned(),
            name: "lean".to_owned(),
            title: "Lean first.".to_owned(),
            since: "2026-10-01".to_owned(),
            repo: None,
        }]);
        let validation = |request: &str, commit: &str| sessions::ValidationRef {
            request: request.to_owned(),
            run: Some("run-9".to_owned()),
            kind: sessions::ValidationKind::Integrated,
            repo: "/s/ohjaa/integrated/widget-0/c3".to_owned(),
            commit: commit.to_owned(),
            tree: format!("tree-{commit}"),
            contract: "/h/repos/acme/widget/sykli.json".to_owned(),
            contract_digest: None,
            lock_digest: None,
            requested_ms: 6_000,
            receipt: None,
            common: Some("/r/widget/.git".to_owned()),
            observed_branch: None,
            ran: None,
            not_dispatched: None,
        };
        report.integrated = vec![
            ("oh-8".to_owned(), validation("rq-9-9", "c3")),
            ("oh-8".to_owned(), validation("rq-9-8", "elsewhere")),
        ];
        report.open = vec![Untold {
            id: "oh-9".to_owned(),
            agent: "claude".to_owned(),
            repo: "/r/widget".to_owned(),
            intent: "fix".to_owned(),
            started_ms: 7_000,
            no_readback: true,
            no_handover: true,
            orphaned: true,
            ohjaa_gone: false,
        }];
        let json = to_json(&report);
        let keys = |value: &Value| {
            let mut keys: Vec<&str> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys.join(",")
        };
        assert_eq!(json["schema"], "ohjaa-marks.v1");
        assert_eq!(
            keys(&json),
            "complete,entries,generated,proposals,report,schema,sessions"
        );
        let entry = &json["entries"][0];
        assert_eq!(
            keys(entry),
            "branch,clone,commits,ended,error,integrated,proven_at_tip,ref,repo,tip,updated,\
             updated_by"
        );
        // Additive (KATANA D5): the integrated State at the tip, and the
        // integrated validations asked for, by reference.
        assert_eq!(keys(&entry["integrated"]), "commit,requested,status");
        assert_eq!(entry["integrated"]["commit"], "c3");
        assert_eq!(entry["integrated"]["status"], "unverified");
        let requested = &entry["integrated"]["requested"][0];
        assert_eq!(
            keys(requested),
            "checkout,commit,not_dispatched,request,requested,run,session,tree"
        );
        assert_eq!(requested["request"], "rq-9-9");
        assert_eq!(requested["run"], "run-9");
        assert_eq!(requested["session"], "oh-8");
        assert_eq!(requested["commit"], "c3");
        assert_eq!(
            entry["integrated"]["requested"].as_array().unwrap().len(),
            1,
            "only the validations of commits on the branch"
        );
        let integrated: Vec<&str> = entry["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["integrated"].as_str().unwrap())
            .collect();
        assert_eq!(integrated, ["unverified", "failed", "passed"]);
        assert_eq!(entry["repo"], "acme/widget");
        assert_eq!(entry["ref"], "origin/main");
        assert_eq!(entry["tip"], "c3");
        assert_eq!(entry["updated"], "1970-01-01T00:00:05Z");
        assert_eq!(entry["updated_by"], "fetch");
        assert_eq!(entry["ended"], "proven");
        assert_eq!(entry["proven_at_tip"], false);
        assert_eq!(entry["error"], Value::Null);
        let commit = &entry["commits"][1];
        assert_eq!(
            keys(commit),
            "author,authored,cached,code,contract,integrated,mark,sha,subject,tasks,tree,why"
        );
        // Additive (G1a): which contract proved a proven commit.
        assert_eq!(commit["contract"], Value::Null);
        assert_eq!(entry["commits"][2]["contract"], "own");
        // Additive: the accepted receipt's tasks and how many it reused from
        // the cache, for a proven commit only.
        assert_eq!(
            (&commit["tasks"], &commit["cached"]),
            (&Value::Null, &Value::Null)
        );
        assert_eq!(
            (
                &entry["commits"][2]["tasks"],
                &entry["commits"][2]["cached"]
            ),
            (&json!(2), &json!(1))
        );
        assert_eq!(commit["sha"], "c2");
        assert_eq!(commit["tree"], "tree-c2");
        assert_eq!(commit["author"], "Ann");
        assert_eq!(commit["authored"], "1970-01-01T00:01:00Z");
        assert_eq!(commit["subject"], "subject c2");
        let marks: Vec<&str> = entry["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["mark"].as_str().unwrap())
            .collect();
        assert_eq!(marks, ["unproven", "failed", "proven"]);
        assert_eq!(commit["code"], "failed");
        assert_eq!(commit["why"], "refused: outcome");
        assert_eq!(entry["commits"][0]["code"], "no_receipt");
        assert_eq!(entry["commits"][0]["why"], "no receipt of its tree here");
        assert_eq!(entry["commits"][2]["code"], "proven");
        assert_eq!(entry["commits"][2]["why"], Value::Null);
        let gone = &json["entries"][1];
        assert_eq!(keys(gone), keys(entry), "one shape, marked or not");
        assert_eq!(gone["error"], "no clone under the roots has it as origin");
        assert_eq!(gone["commits"], json!([]));
        assert_eq!(gone["integrated"], Value::Null);
        assert_eq!(keys(&json["proposals"]), "listed,waiting");
        assert_eq!(
            keys(&json["proposals"]["waiting"][0]),
            "name,repo,scope,since,title"
        );
        assert_eq!(
            keys(&json["sessions"][0]),
            "agent,id,intent,repo,started,state"
        );
        assert_eq!(json["sessions"][0]["state"], "orphaned");
        // Open, its ohjaa gone while its process group runs on: its own state.
        report.open[0].orphaned = false;
        report.open[0].ohjaa_gone = true;
        assert_eq!(to_json(&report)["sessions"][0]["state"], "ohjaa_gone");
        report.open[0].ohjaa_gone = false;
        assert_eq!(to_json(&report)["sessions"][0]["state"], "open");
        assert_eq!(json["report"]["schema"], "ohjaa-report.v1");
        assert_eq!(json["complete"], false, "vessa could not be read");
        // The marks alone: the same keys, no window.
        report.window = false;
        let marks_only = to_json(&report);
        assert_eq!(keys(&marks_only), keys(&json));
        assert_eq!(marks_only["report"], Value::Null);
        assert_eq!(marks_only["entries"], json["entries"]);
    }

    #[test]
    fn only_real_failures_and_judged_bypasses_become_candidates() {
        let natives = Natives {
            bypasses: vec![Bypass {
                agent: "codex".to_owned(),
                id: "n1".to_owned(),
                cwd: String::new(),
                started_ms: 7,
            }],
            matched: Vec::new(),
            headless: 3,
            undated: Vec::new(),
        };
        let report = sample(Ok(natives));
        let texts: Vec<String> = candidates(&report).into_iter().map(|c| c.text).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(texts[0].contains("never read back"));
        assert!(texts[1].contains("FAILED sykli verify"));
        assert!(texts[2].contains("in (no folder) was started without `ohjaa run`"));
        let said = summary(&report);
        assert_eq!(said.lines().count(), 3, "{said}");
        assert!(said.contains("2 session(s) without read-back or handover"));
        assert!(said.contains("1 agent session(s) started without ohjaa"));
        assert!(said.contains("first: c0ffee1 on origin/main in app: FAILED"));
        assert!(said.contains("the next summary is in 7 days"));
        // Not judged: no bypass is a candidate, none is counted.
        let report = sample(Err("vessa is not on PATH".to_owned()));
        assert_eq!(candidates(&report).len(), 2);
        let said = summary(&report);
        assert!(
            said.contains("not judged")
                && said.contains("incomplete: vessa is not on PATH — install vessa")
                && said.contains("said again tomorrow"),
            "{said}"
        );
        // What the gap is, said so a person can fix it.
        let report = sample(Err(
            "vessa is too old for `sessions --headers`; update it".to_owned()
        ));
        let said = summary(&report);
        assert!(
            said.contains("incomplete: vessa is too old for `sessions --headers`; update it;"),
            "{said}"
        );
        assert_eq!(said.lines().count(), 3, "{said}");
    }
}
