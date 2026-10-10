//! The bypass report: what went around the workflow in a window, derived now.
//!
//! Three lists, each from something ohjaa can observe, never from anyone's
//! word:
//! - sessions in the log that never read back their briefing, or left no
//!   handover;
//! - commits on a known repository's default branch that sykli refused, or
//!   that were never checked where a receipt would be;
//! - native agent sessions (vessa's list) that did not go through `ohjaa run`.
//!
//! What could not be looked at is said, never shown as nothing. Only the time
//! the last report was made is stored, like intake's cursor; the report
//! itself is derived each time.

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

/// A native agent session that did not go through `ohjaa run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bypass {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    pub started_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub since_ms: i64,
    pub until_ms: i64,
    pub untold: Vec<Untold>,
    pub unproven: Vec<Unproven>,
    /// The native sessions not run through ohjaa, or why they could not be
    /// looked for.
    pub bypasses: Result<Vec<Bypass>, String>,
    /// What could not be read, each with why: a report with gaps says so.
    pub gaps: Vec<String>,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.gaps.is_empty() && self.bypasses.is_ok()
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

/// The report for `since_ms` to `now_ms`: the log in `dirs`, the git
/// repositories under `roots`, and vessa's sessions when vessa is on PATH.
pub fn build(dirs: &Dirs, repos: &[PathBuf], since_ms: i64, now_ms: i64) -> Report {
    let mut gaps = Vec::new();
    let logged: Vec<Session> = match sessions::Store::open(&sessions::store_path(&dirs.state)) {
        Ok(store) => match store.all() {
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

    let sykli = lifecycle::Sykli::default();
    let mut unproven = Vec::new();
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
        match lifecycle::default_branch_since(repo, since_ms.div_euclid(1000), &sykli) {
            Some(commits) => unproven.extend(
                commits
                    .into_iter()
                    .filter(|on| matches!(on.check, Check::Failed { .. } | Check::Unchecked))
                    .map(|on| Unproven {
                        repo: repo.to_string_lossy().into_owned(),
                        on,
                    }),
            ),
            None => {
                // A repository with no default branch has nothing to report.
                if has_commits(repo) && has_default_branch(repo) {
                    gaps.push(format!(
                        "{}: its default branch could not be read",
                        repo.display()
                    ));
                }
            }
        }
    }
    if let Some(why) = lifecycle::sykli_cannot_bind(&sykli) {
        if !unproven.is_empty() || !repos.is_empty() {
            gaps.push(format!(
                "sykli could not verify commits ({why}): only commits sykli was never run on are listed"
            ));
        }
    }
    unproven.sort_by_key(|u| std::cmp::Reverse(u.on.committed));

    let bypasses = native_sessions(since_ms, now_ms).map(|native| bypasses(native, &logged));
    Report {
        since_ms,
        until_ms: now_ms,
        untold,
        unproven,
        bypasses,
        gaps,
    }
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
        .filter(|s| s.left.is_some() && s.started_ms >= since_ms && s.started_ms <= now_ms)
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

/// A native session, as vessa lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Native {
    pub agent: String,
    pub id: String,
    pub cwd: String,
    pub started_ms: i64,
}

/// The top-level Claude and Codex sessions vessa lists that started in the
/// window; `Err` says why they could not be listed.
fn native_sessions(since_ms: i64, now_ms: i64) -> Result<Vec<Native>, String> {
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
                    "vessa is not on PATH: agent sessions run without ohjaa were not looked for"
                        .to_owned()
                } else {
                    format!("vessa could not list sessions: {text}")
                },
            );
        }
    };
    if !output.status.success() && output.stdout.is_empty() {
        return Err(format!(
            "vessa could not list sessions: {}",
            output.failure_detail()
        ));
    }
    let listed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("vessa's sessions are not JSON: {error}"))?;
    parse_native(&listed, since_ms, now_ms)
}

/// Read `vessa sessions --json` (`vessa-usage.v1`): `sessions[]` with `agent`,
/// `id`, `parent`, `started_at` (RFC 3339) and `cwd`. vessa exits non-zero when
/// a source is missing or usage is partial, and still lists what it read. Subagent sessions (with a parent) were
/// started by a session, not by a person, and are left out.
pub fn parse_native(listed: &Value, since_ms: i64, now_ms: i64) -> Result<Vec<Native>, String> {
    let sessions = listed["sessions"]
        .as_array()
        .ok_or("vessa's output has no sessions list")?;
    Ok(sessions
        .iter()
        .filter(|s| matches!(s["agent"].as_str(), Some("claude" | "codex")))
        .filter(|s| s["parent"].is_null())
        .filter_map(|s| {
            let started_ms = intake::parse_time(s["started_at"].as_str()?)?;
            (started_ms >= since_ms && started_ms <= now_ms).then(|| Native {
                agent: s["agent"].as_str().unwrap_or_default().to_owned(),
                id: s["id"].as_str().unwrap_or_default().to_owned(),
                cwd: s["cwd"].as_str().unwrap_or_default().to_owned(),
                started_ms,
            })
        })
        .collect())
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
    let _ = writeln!(
        out,
        "\nCommits on a default branch without passing evidence ({}):",
        report.unproven.len()
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
    match &report.bypasses {
        Ok(bypasses) => {
            let _ = writeln!(
                out,
                "\nAgent sessions not run through ohjaa ({}):",
                bypasses.len()
            );
            for b in bypasses {
                let _ = writeln!(
                    out,
                    "- {} {} in {}, started {}",
                    b.agent,
                    b.id,
                    b.cwd,
                    sessions::clock(b.started_ms)
                );
            }
        }
        Err(why) => {
            let _ = writeln!(
                out,
                "\nAgent sessions not run through ohjaa: not checked: {why}"
            );
        }
    }
    if !report.gaps.is_empty() {
        let _ = writeln!(out, "\nNot read, so absence above proves nothing there:");
        for gap in &report.gaps {
            let _ = writeln!(out, "- {gap}");
        }
    }
    out
}

/// The report as `ohjaa-report.v1` JSON.
pub fn to_json(report: &Report) -> Value {
    let rfc = |ms: i64| intake::rfc3339(ms.div_euclid(1000));
    json!({
        "schema": "ohjaa-report.v1",
        "since": rfc(report.since_ms),
        "until": rfc(report.until_ms),
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
        "bypasses": match &report.bypasses {
            Ok(list) => json!({
                "checked": true,
                "sessions": list.iter().map(|b| json!({
                    "agent": b.agent,
                    "id": b.id,
                    "cwd": b.cwd,
                    "started": rfc(b.started_ms),
                })).collect::<Vec<_>>(),
            }),
            Err(why) => json!({"checked": false, "why": why}),
        },
        "gaps": report.gaps,
    })
}

/// Three lines for the person at `ohjaa run`, before the agent starts.
pub fn summary(report: &Report) -> String {
    let bypassed = match &report.bypasses {
        Ok(list) => format!("{} agent session(s) not run through ohjaa", list.len()),
        Err(_) => "agent sessions run without ohjaa not checked (no vessa)".to_owned(),
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
    format!(
        "ohjaa: weekly report, {} to {}: {} session(s) without read-back or handover · {} \
         commit(s) on a default branch without passing evidence · {bypassed}\n\
         ohjaa:   first: {first}\n\
         ohjaa:   `ohjaa report` lists them all{}; the next summary is in 7 days.\n",
        &sessions::clock(report.since_ms)[..10],
        &sessions::clock(report.until_ms)[..10],
        report.untold.len(),
        report.unproven.len(),
        if report.complete() {
            ""
        } else {
            ", and what it could not read"
        }
    )
}

// ---- intake ------------------------------------------------------------------

/// The report's real failures and bypasses as intake candidates: never what
/// could not be checked. A missing handover is intake's already, from the
/// session itself; a missing read-back is not.
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
    if let Ok(bypasses) = &report.bypasses {
        for b in bypasses {
            push(
                b.started_ms,
                &b.cwd,
                format!("{} session {}, observed by ohjaa through vessa, not said by anyone", b.agent, b.id),
                format!(
                    "a {} session ({}) in {} started without `ohjaa run`: it was briefed with no knowledge and left no record",
                    b.agent, b.id, b.cwd
                ),
            );
        }
    }
    out
}

// ---- once a week -------------------------------------------------------------

fn marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join("report-at")
}

/// When the last report was made, if one was.
pub fn last_made(dirs: &Dirs) -> Option<i64> {
    std::fs::read_to_string(marker(dirs))
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Record that a report was made at `now_ms`.
pub fn made(dirs: &Dirs, now_ms: i64) -> Result<()> {
    std::fs::create_dir_all(&dirs.state)
        .with_context(|| format!("cannot create {}", dirs.state.display()))?;
    std::fs::write(marker(dirs), format!("{now_ms}\n"))
        .with_context(|| format!("cannot write {}", marker(dirs).display()))
}

/// Whether a week has passed since the last report (or none was made).
pub fn due(dirs: &Dirs, now_ms: i64) -> bool {
    last_made(dirs).is_none_or(|at| now_ms - at >= WEEK_MS)
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
    fn vessas_list_is_read_for_top_level_claude_and_codex_sessions_in_the_window() {
        let listed = json!({"schema": "vessa-usage.v1", "warnings": [], "sessions": [
            {"agent": "claude", "id": "in", "parent": null, "started_at": "2026-10-09T10:00:00.500Z", "cwd": "/r/app"},
            {"agent": "claude", "id": "sub", "parent": "in", "started_at": "2026-10-09T10:01:00Z", "cwd": "/r/app"},
            {"agent": "pi", "id": "pi", "parent": null, "started_at": "2026-10-09T10:00:00Z", "cwd": "/r/app"},
            {"agent": "codex", "id": "old", "parent": null, "started_at": "2026-09-01T10:00:00Z", "cwd": "/r/app"},
            {"agent": "codex", "id": "undated", "parent": null, "started_at": null, "cwd": "/r/app"}
        ]});
        let since = intake::parse_time("2026-10-03T00:00:00Z").unwrap();
        let now = intake::parse_time("2026-10-10T00:00:00Z").unwrap();
        let found = parse_native(&listed, since, now).unwrap();
        assert_eq!(
            found,
            [native(
                "claude",
                "in",
                "/r/app",
                since + 6 * 86_400_000 + 36_000_500
            )]
        );
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
        assert!(due(&dirs, now), "never made: due");
        made(&dirs, now).unwrap();
        assert_eq!(last_made(&dirs), Some(now));
        assert!(!due(&dirs, now + WEEK_MS - 1));
        assert!(due(&dirs, now + WEEK_MS));
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

    #[test]
    fn only_real_failures_and_bypasses_become_candidates() {
        let on = |check: Check| lifecycle::OnDefault {
            onto: "origin/main".to_owned(),
            commit: "c0ffee1234".to_owned(),
            subject: "s".to_owned(),
            committed: 100,
            check,
        };
        let report = Report {
            since_ms: 0,
            until_ms: 1,
            untold: vec![
                Untold {
                    id: "oh-1".to_owned(),
                    agent: "claude".to_owned(),
                    repo: "/r/app".to_owned(),
                    intent: "x".to_owned(),
                    started_ms: 5,
                    no_readback: true,
                    no_handover: true,
                    orphaned: false,
                },
                Untold {
                    id: "oh-2".to_owned(),
                    agent: "claude".to_owned(),
                    repo: "/r/app".to_owned(),
                    intent: "x".to_owned(),
                    started_ms: 6,
                    no_readback: false,
                    no_handover: true,
                    orphaned: false,
                },
            ],
            unproven: vec![Unproven {
                repo: "/r/app".to_owned(),
                on: on(Check::Failed {
                    why: "refused: outcome".to_owned(),
                }),
            }],
            bypasses: Ok(vec![Bypass {
                agent: "codex".to_owned(),
                id: "n1".to_owned(),
                cwd: "/r/app".to_owned(),
                started_ms: 7,
            }]),
            gaps: Vec::new(),
        };
        let texts: Vec<String> = candidates(&report).into_iter().map(|c| c.text).collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(texts[0].contains("never read back"));
        assert!(texts[1].contains("FAILED sykli verify"));
        assert!(texts[2].contains("without `ohjaa run`"));
        let summary = summary(&report);
        assert_eq!(summary.lines().count(), 3, "{summary}");
        assert!(summary.contains("2 session(s) without read-back or handover"));
        assert!(summary.contains("first: c0ffee1 on origin/main in app: FAILED"));
    }
}
