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
use crate::sessions::{self, Check, Session};

/// The window a report covers unless told otherwise, and how often `ohjaa
/// run` says one to the person.
pub const WEEK_MS: i64 = 7 * 86_400_000;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub since_ms: i64,
    pub until_ms: i64,
    pub unproven: Vec<Unproven>,
    pub judged: Judged,
    /// The marks on each entry's branch, or why it has none, by `owner/name`.
    pub marks: Vec<(String, Result<mark::Marks, String>)>,
    /// Why no entry could be listed, when none could (the workflow home
    /// cannot be found, or its entries cannot be read): then `marks` is
    /// empty, and no entry's absence from it says anything.
    pub unlisted: Option<String>,
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
        self.gaps.is_empty()
    }
}

/// Why no agent session is judged against a launch: ohjaa launches none.
/// Said where `ohjaa-report.v1` keeps its keys (`bypasses`).
pub const NO_LAUNCHES: &str =
    "ohjaa no longer launches agents: the rules reach each through its own instruction file";

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
    // One sykli for both: a commit the window and the marks share is asked
    // about once, and a repository with an entry is judged by its gate.
    let sykli = lifecycle::sykli_with_entries();
    let (unproven, judged) = if window {
        default_branches(repos, since_ms, &sykli, &mut gaps)
    } else {
        (Vec::new(), Judged::default())
    };
    let listed = match crate::home::ohjaa_home() {
        Ok(home) => mark::of_entries(&crate::home::absolute(home), repos, &sykli)
            .map_err(|why| format!("the entries cannot be listed: {why}")),
        Err(error) => Err(format!("the workflow home cannot be found: {error:#}")),
    };
    let (marks, unlisted) = match listed {
        Ok(marks) => (marks, None),
        Err(why) => {
            gaps.push(why.clone());
            (Vec::new(), Some(why))
        }
    };
    for (slug, marked) in &marks {
        match marked {
            Err(why) => gaps.push(format!("{slug}: not marked: {why}")),
            // A receipt sykli named that cannot be read: never taken for one.
            Ok(marked) => gaps.extend(
                marked
                    .unreadable
                    .iter()
                    .map(|why| format!("{slug}: a receipt cannot be read: {why}")),
            ),
        }
    }
    let proposals = waiting(dirs);
    let open = open_sessions(&logged, since_ms, now_ms, sessions::observe_open);
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
        unproven,
        judged,
        marks,
        unlisted,
        proposals,
        open,
        gaps,
        window,
        integrated,
    }
}

/// What waits for a person: every proposal, and every proposed node in a
/// scope directory. Files only.
pub fn waiting(dirs: &Dirs) -> Result<Vec<Waiting>, String> {
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
        Check::Unchecked => "unchecked: no sykli run of it where it was made".to_owned(),
        other => sessions::describe_check(other),
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
            match &report.unlisted {
                Some(why) => {
                    let _ = writeln!(out, "- none marked: {why}");
                }
                None => {
                    let _ = writeln!(out, "- no repository has an entry (ohjaa adopt <repo>)");
                }
            }
        }
    }
    if !report.window {
        say_gaps(report, &mut out);
        return out;
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
    say_gaps(report, &mut out);
    out
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
                    // ("derived": from its own sykli.json, or what sykli init
                    // writes for its tree), or its own sykli.json ("own").
                    // "stored", an entry's copy, is no longer given.
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
        // Additive (review of #67): why no entry is listed, when none could
        // be; `entries` is then empty, and says nothing.
        "unlisted": report.unlisted,
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
        // Kept, empty: ohjaa launches no session to read back or hand over.
        "sessions_without_testimony": [],
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
        // Kept: no session is judged against a launch, and why.
        "bypasses": {"judged": false, "why": NO_LAUNCHES},
        "gaps": report.gaps,
    })
}

// ---- intake ------------------------------------------------------------------

/// The report's real failures and bypasses as intake candidates: never what
/// could not be checked or judged. A missing handover is intake's already,
/// from the session itself; a missing read-back is not.
pub fn candidates(report: &Report) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut push =
        |at_ms: i64, place: &str, session: Option<&str>, context: String, text: String| {
            out.push(Candidate {
                source: "observed",
                at_ms,
                at: intake::rfc3339(at_ms.div_euclid(1000)),
                place: place.to_owned(),
                who: "ohjaa".to_owned(),
                context,
                text,
                session: session.map(str::to_owned),
            });
        };
    for u in &report.unproven {
        push(
            u.on.committed * 1000,
            &u.repo,
            None,
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
    out
}

// ---- once a week -------------------------------------------------------------

/// When a complete report was last shown to a person: the week's.
fn marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join("report-at")
}

/// When any report was last shown to a person, complete or not.
fn shown_marker(dirs: &Dirs) -> PathBuf {
    dirs.state.join("report-shown-at")
}

fn write_time(dirs: &Dirs, path: &Path, now_ms: i64) -> Result<()> {
    std::fs::create_dir_all(&dirs.state)
        .with_context(|| format!("cannot create {}", dirs.state.display()))?;
    std::fs::write(path, format!("{now_ms}\n"))
        .with_context(|| format!("cannot write {}", path.display()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_days_hours_or_a_time() {
        let now = 10 * 86_400_000;
        assert_eq!(parse_since("7d", now).unwrap(), 3 * 86_400_000);
        assert_eq!(parse_since("24h", now).unwrap(), 9 * 86_400_000);
        assert_eq!(parse_since("1970-01-02", now).unwrap(), 86_400_000);
        assert!(parse_since("0d", now).is_err());
        assert!(parse_since("soon", now).is_err());
    }
}
