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
use crate::knowledge::Dirs;
use crate::lifecycle;
use crate::process;
use crate::sessions::{self, Check, Session};

/// The window a report covers unless told otherwise, and how often `ohjaa
/// run` says one to the person.
pub const WEEK_MS: i64 = 7 * 86_400_000;

/// How far a native session's start may lie from its `ohjaa run` launch and
/// still be that launch's agent: entering, toimija and the agent's own start
/// take seconds, and a loaded machine more.
const LAUNCH_SLACK_MS: i64 = 60_000;

/// How long vessa may take to read the transcripts.
const VESSA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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
    pub orphaned: bool,
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

/// What vessa's list says about sessions run without ohjaa.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Natives {
    pub bypasses: Vec<Bypass>,
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
    /// What could not be read or judged, each with why: a report with gaps
    /// says so, and exits 4.
    pub gaps: Vec<String>,
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
    let untold = untold(&logged, since_ms, now_ms);
    let (unproven, judged) = default_branches(repos, since_ms, &mut gaps);
    let natives = native_sessions(since_ms, now_ms, &mut gaps).map(|listed| {
        let (judgeable, mut natives) = listed;
        natives.bypasses = bypasses(judgeable, &logged);
        natives
    });
    if let Ok(natives) = &natives {
        if !natives.undated.is_empty() {
            gaps.push(format!(
                "{} native session(s) vessa could not date or place: not judged",
                natives.undated.len()
            ));
        }
    }
    Report {
        since_ms,
        until_ms: now_ms,
        untold,
        unproven,
        judged,
        natives,
        gaps,
    }
}

/// Every first-parent commit each repository's default branch gained in the
/// window, judged by sykli: the unproven ones, and a count of the rest.
fn default_branches(
    repos: &[PathBuf],
    since_ms: i64,
    gaps: &mut Vec<String>,
) -> (Vec<Unproven>, Judged) {
    let mut unproven = Vec::new();
    let mut judged = Judged::default();
    if repos.is_empty() {
        gaps.push("there are no git repositories under the roots".to_owned());
        return (unproven, judged);
    }
    let sykli = lifecycle::Sykli::default();
    let mut clones = std::collections::HashSet::new();
    for repo in repos {
        // A worktree of a clone already read is the same clone.
        let common = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| repo.join(String::from_utf8_lossy(&o.stdout).trim()))
            .and_then(|common| std::fs::canonicalize(common).ok());
        if common.is_some_and(|common| !clones.insert(common)) {
            continue;
        }
        let Some((commits, capped)) =
            lifecycle::default_branch_since(repo, since_ms.div_euclid(1000), &sykli)
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
                Check::Passed => judged.verified += 1,
                Check::Failed { .. } | Check::Unchecked => unproven.push(Unproven {
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
    if let Some(why) = lifecycle::sykli_cannot_bind(&sykli) {
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
        Command::new("vessa").args(["sessions", "--json"]),
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
/// exec). Intake's own drafter runs one of these.
fn headless(session: &Value) -> bool {
    session["entrypoint"]
        .as_str()
        .is_some_and(|entry| entry.starts_with("sdk"))
        || session["originator"]
            .as_str()
            .is_some_and(|origin| origin.contains("exec"))
}

/// Read `vessa sessions --json` (`vessa-usage.v1`): `sessions[]` with `agent`,
/// `id`, `parent`, `started_at` (RFC 3339), `cwd`, `source`, and how each was
/// started (`entrypoint`, `originator`). Subagent sessions (with a parent)
/// and headless runs were started by no person and are left out. A session
/// with no date or no folder cannot be judged against a launch: it is kept
/// apart as undated when its transcript was written in the window. Without
/// `entrypoint` and `originator` (an older vessa), headless runs cannot be
/// told apart, and nothing is judged.
pub fn parse_native(
    listed: &Value,
    since_ms: i64,
    now_ms: i64,
) -> Result<(Vec<Native>, Natives), String> {
    let sessions = listed["sessions"]
        .as_array()
        .ok_or("vessa's output has no sessions list")?;
    let says_how = |s: &Value| {
        s.as_object()
            .is_some_and(|o| o.contains_key("entrypoint") && o.contains_key("originator"))
    };
    if !sessions.is_empty() && !sessions.iter().any(says_how) {
        return Err(
            "vessa does not say how a session was started (no `entrypoint` or `originator`; \
             vessa#2), so headless runs cannot be told apart"
                .to_owned(),
        );
    }
    let in_window = |ms: i64| ms > since_ms && ms <= now_ms;
    let mut judgeable = Vec::new();
    let mut natives = Natives::default();
    for s in sessions
        .iter()
        .filter(|s| matches!(s["agent"].as_str(), Some("claude" | "codex")))
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
                });
            }
            _ => {
                let source = text("source");
                let written_ms = std::fs::metadata(&source)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    // Whole seconds, as the window's ends are.
                    .map(|at| at.as_secs() as i64 * 1000);
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

/// The native sessions no `ohjaa run` launched, oldest first. One matches a
/// logged session when that session's usage receipt names it, or when it is
/// the same agent, in that session's repository (or below it), and started
/// within [`LAUNCH_SLACK_MS`] after the launch (or 5 s before it); each
/// logged session claims at most one, the closest.
pub fn bypasses(native: Vec<Native>, logged: &[Session]) -> Vec<Bypass> {
    let canonical =
        |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let mut claimed = std::collections::HashSet::new();
    let mut by_receipt = std::collections::HashSet::new();
    for s in logged {
        for receipt in &s.usage {
            by_receipt.insert((receipt.agent.clone(), receipt.session_id.clone()));
        }
    }
    let mut native = native;
    native.sort_by_key(|n| n.started_ms);
    let mut out = Vec::new();
    for n in native {
        if by_receipt.contains(&(n.agent.clone(), n.id.clone())) {
            continue;
        }
        let cwd = canonical(&n.cwd);
        let launch = logged
            .iter()
            .filter(|s| !claimed.contains(&s.id))
            .filter(|s| {
                Path::new(&s.agent).file_name().and_then(|f| f.to_str()) == Some(n.agent.as_str())
            })
            .filter(|s| cwd.starts_with(canonical(&s.repo)))
            .filter(|s| {
                n.started_ms >= s.started_ms - 5_000
                    && n.started_ms <= s.started_ms + LAUNCH_SLACK_MS
            })
            .min_by_key(|s| (n.started_ms - s.started_ms).abs());
        match launch {
            Some(s) => {
                claimed.insert(s.id.clone());
            }
            None => out.push(Bypass {
                agent: n.agent,
                id: n.id,
                cwd: n.cwd,
                started_ms: n.started_ms,
            }),
        }
    }
    out
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
        Check::Failed { why } => format!("FAILED sykli verify: {why}"),
        Check::Unchecked => "unchecked: no sykli run of it where it was made".to_owned(),
        other => format!("{other:?}"),
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
    let mut parts = vec![format!("{} verified", j.verified)];
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
    if !report.gaps.is_empty() {
        let _ = writeln!(
            out,
            "\nNot read or not judged, so absence above proves nothing there:"
        );
        for gap in &report.gaps {
            let _ = writeln!(out, "- {gap}");
        }
    }
    out
}

/// The report as `ohjaa-report.v1` JSON.
pub fn to_json(report: &Report) -> Value {
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
                _ => "unchecked",
            },
            "why": describe_check(&u.on.check),
        })).collect::<Vec<_>>(),
        "default_branch_commits": {
            "repositories": j.repos,
            "commits": j.commits,
            "verified": j.verified,
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
        Err(why) if why.starts_with("vessa does not say how") => {
            Some("vessa lacks session origins — update vessa (vessa#2)".to_owned())
        }
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
        }
    }

    #[test]
    fn a_native_session_is_ohjaas_when_launched_by_it_here_and_then() {
        let launched = session("oh-1", "claude", "/nonexistent/r/app", 1_000_000);
        let ids = |list: Vec<Bypass>| list.into_iter().map(|b| b.id).collect::<Vec<_>>();
        // Its agent, in its repository (a folder below it too), seconds after.
        assert!(bypasses(
            vec![native("claude", "a", "/nonexistent/r/app/src", 1_003_000)],
            std::slice::from_ref(&launched)
        )
        .is_empty());
        // Another agent, another place, or long after: not launched by it.
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
        let listed = json!({"schema": "vessa-usage.v1", "warnings": [], "sessions": [
            s("tui", "claude", json!({"entrypoint": "cli"})),
            s("sub", "claude", json!({"parent": "tui"})),
            s("pi", "pi", json!({})),
            s("old", "codex", json!({"started_at": "2026-09-01T10:00:00Z"})),
            s("headless", "claude", json!({"entrypoint": "sdk-cli"})),
            s("exec", "codex", json!({"originator": "codex_exec"})),
            s("undated", "claude", json!({"started_at": null, "cwd": ""})),
            s("nowhere", "codex", json!({"cwd": ""})),
        ]});
        let (judgeable, natives) = parse_native(&listed, since, now).unwrap();
        assert_eq!(
            judgeable,
            [native(
                "claude",
                "tui",
                "/r/app",
                since + 6 * 86_400_000 + 36_000_500
            )]
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
        // An older vessa cannot tell a headless run from a person's session.
        let older = json!({"sessions": [{"agent": "claude", "id": "x", "parent": null,
            "started_at": "2026-10-09T10:00:00Z", "cwd": "/r/app"}]});
        assert!(parse_native(&older, since, now)
            .unwrap_err()
            .contains("headless runs cannot be told apart"));
        assert!(parse_native(&json!({"nothing": []}), since, now).is_err());
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
            gaps: Vec::new(),
        }
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
            "vessa does not say how a session was started (no `entrypoint` or `originator`; \
             vessa#2), so headless runs cannot be told apart"
                .to_owned(),
        ));
        let said = summary(&report);
        assert!(
            said.contains("incomplete: vessa lacks session origins — update vessa"),
            "{said}"
        );
        assert_eq!(said.lines().count(), 3, "{said}");
    }
}
