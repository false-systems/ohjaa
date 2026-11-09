//! Integrated-state validation (KATANA D5, BRIEF §5): what sykli says of the
//! State that reached an entry's observed branch, as against a candidate on
//! its way there; and a base that moved since a candidate was validated.
//!
//! - A candidate's pass is the integrated State's only when the trees are
//!   the same: sykli binds a receipt to a tree, and `sykli status --commit`
//!   judges every receipt of a commit's tree for that commit. ohjaa never
//!   carries a result across trees.
//! - When a landing reaches the observed branch (seen at leaving, or by a
//!   launch's marks walk) and sykli has no current receipt of its tree under
//!   its gate (derived from the commit), ohjaa asks sykli for a durable run
//!   of exactly that tree. Under the clone's lock, a `ValidationRef` of kind
//!   `integrated` is written into the session's record first (invariant A);
//!   then `sykli run --detach` runs in the clone's one rolling validation
//!   checkout
//!   (`$XDG_STATE_HOME/ohjaa/integrated/<clone>/checkout`, a linked worktree
//!   of the clone, reset to exactly the landed commit and verified clean
//!   first), whose cargo builds go to a target dir of its own. ohjaa keeps
//!   the reference, and a copy of the receipt once the run is done; what
//!   became of the run is sykli's to say.
//! - A candidate validated when `origin/<branch>` was at B, seen when it is
//!   elsewhere, is flagged "base moved since validation": at a push and in a
//!   briefing. Never refused (observe, mark and flag).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::gitconfig;
use crate::lifecycle::{self, Gate, StatusHelp, Sykli};
use crate::mark::{self, Marks};
use crate::process::{self};
use crate::sessions::{self, Check, ValidationKind, ValidationRef};

/// What this machine's sykli can say here, asked once: `sykli status --help`.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub status: StatusHelp,
}

impl Caps {
    pub fn ask() -> Caps {
        Caps {
            status: lifecycle::sykli_status_help(),
        }
    }
}

/// What sykli says applies to a commit (`sykli status --commit`), in its
/// own terms. Never stored: asked when shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// A current receipt of its tree verified for it, and passed.
    Passed,
    /// A current receipt of its tree verified for it, and failed.
    Failed,
    /// No current receipt; a run of its tree is requested or running.
    Pending(String),
    /// No current receipt, and no run of its tree.
    None,
    /// sykli's results of its tree disagree (`conflict`).
    Conflict,
    /// sykli could not be asked, or could not say: why.
    Unknown(String),
}

impl Said {
    /// Why, for people.
    pub fn why(&self) -> String {
        match self {
            Said::Passed => "a current sykli receipt of its tree passed".to_owned(),
            Said::Failed => "a current sykli receipt of its tree failed".to_owned(),
            Said::Pending(runs) => format!("a run of it is pending (sykli run {runs})"),
            Said::None => "sykli has no current receipt of its tree".to_owned(),
            Said::Conflict => "sykli's results of its tree disagree".to_owned(),
            Said::Unknown(why) => why.clone(),
        }
    }

    /// `passed`, `failed` or `unverified`: a pass or a failure only where a
    /// current receipt says so.
    pub fn word(&self) -> &'static str {
        match self {
            Said::Passed => "passed",
            Said::Failed => "failed",
            _ => "unverified",
        }
    }
}

/// The states BRIEF §5 names: candidate passed or failed, integrated
/// passed, failed or unverified (and a candidate with no word yet).
pub fn standing(kind: ValidationKind, said: &Said) -> String {
    let kind = match kind {
        ValidationKind::Candidate => "candidate",
        ValidationKind::Integrated => "integrated",
    };
    format!("{kind} {}", said.word())
}

/// `sykli status --commit <rev> --contract <its gate> --json`, from the
/// gate's checkout, handed the clone's facts when sykli takes them.
fn commit_status(
    gate: &Gate,
    contract: &Path,
    help: StatusHelp,
    rev: &str,
) -> Result<Value, String> {
    let mut command = Command::new("sykli");
    command
        .args(["status", "--commit", rev, "--contract"])
        .arg(contract);
    if help.facts {
        command
            .arg("--repo-root")
            .arg(&gate.top)
            .arg("--git-common-dir")
            .arg(&gate.common);
    }
    command.arg("--json").current_dir(&gate.top);
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    let output =
        process::run(&mut command, None, process::LOCAL).map_err(|error| format!("{error:#}"))?;
    if !output.status.success() {
        return Err(output.failure_detail());
    }
    let status: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("its answer is not JSON ({error})"))?;
    if status["schema"] != "sykli-commit-status.v1" {
        return Err(format!(
            "its answer is not sykli-commit-status.v1 (schema {})",
            status["schema"]
        ));
    }
    Ok(status)
}

/// sykli's verdict, in its own terms.
pub fn said_of(status: &Value) -> Said {
    match status["verdict"].as_str() {
        Some("passed") => Said::Passed,
        Some("failed") => Said::Failed,
        Some("pending") => {
            let runs: Vec<&str> = status["runs"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter(|run| matches!(run["state"].as_str(), Some("requested" | "running")))
                .filter_map(|run| run["run"].as_str())
                .collect();
            Said::Pending(if runs.is_empty() {
                "?".to_owned()
            } else {
                runs.join(", ")
            })
        }
        Some("none") => Said::None,
        Some("conflict") => Said::Conflict,
        Some(other) => Said::Unknown(format!("sykli said {other}")),
        None => Said::Unknown("sykli gave no verdict".to_owned()),
    }
}

/// The gate `commit` is judged by, as the file sykli is handed, read
/// through `sykli`'s one git process for the clone: kept in ohjaa's state
/// for a run that outlives this ohjaa (`keep`), else where sykli reads it
/// now.
pub fn contract_of(
    gate: &Gate,
    sykli: &Sykli,
    commit: &str,
    keep: bool,
) -> Result<PathBuf, String> {
    sykli
        .gate_at(gate, commit)
        .and_then(|at| at.contract(sykli.gates(), keep))
}

/// What sykli says of `rev` under its gate, when this sykli can say.
pub fn ask(gate: &Gate, sykli: &Sykli, caps: &Caps, rev: &str) -> Said {
    if !caps.status.commit {
        return Said::Unknown("this sykli cannot be asked (no `sykli status --commit`)".to_owned());
    }
    let contract = match contract_of(gate, sykli, rev, false) {
        Ok(contract) => contract,
        Err(why) => return Said::Unknown(format!("it has no gate to ask about: {why}")),
    };
    match commit_status(gate, &contract, caps.status, rev) {
        Ok(status) => said_of(&status),
        Err(why) => Said::Unknown(format!("sykli could not say: {why}")),
    }
}

/// The marks' own word of a commit, for a sykli without `status --commit`:
/// `sykli verify` of a receipt of its tree, as the marks asked it.
fn said_by_marks(check: &Check) -> Said {
    match check {
        Check::Passed => Said::Passed,
        Check::Failed { .. } => Said::Failed,
        other => Said::Unknown(format!(
            "no `sykli status --commit` here; the marks: {}",
            mark::why(other).unwrap_or_default()
        )),
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Where ohjaa's validation folders live: `integrated/` in its state
/// directory (`$XDG_STATE_HOME/ohjaa`, beside the session log).
pub fn checkouts_root(state: &Path) -> PathBuf {
    state.join("integrated")
}

/// What a request recorded and never handed to sykli is.
pub const NEVER_DISPATCHED: &str =
    "requested, never dispatched (ohjaa stopped before asking sykli)";

/// An integrated validation a session recorded, with the session.
#[derive(Debug, Clone)]
pub struct Requested {
    pub session: String,
    pub started_ms: i64,
    /// The session's record is closed (it left, or was closed as orphaned).
    pub ended: bool,
    pub validation: ValidationRef,
}

/// The integrated validations recorded in the clone whose common git dir is
/// `common`, from every session in the log.
pub fn requested_in(all: &[sessions::Session], common: &Path) -> Vec<Requested> {
    let common = canonical(common).to_string_lossy().into_owned();
    let mut out = Vec::new();
    for session in all {
        for validation in session.validations.iter().filter(|v| {
            v.kind == ValidationKind::Integrated && v.common.as_deref() == Some(common.as_str())
        }) {
            out.push(Requested {
                session: session.id.clone(),
                started_ms: session.started_ms,
                ended: session.left.is_some(),
                validation: validation.clone(),
            });
        }
    }
    out
}

/// How long a request stands before sykli is asked for another run of the
/// same commit, while sykli still has no current receipt of it (a run that
/// was lost or interrupted ends with none).
const ASK_AGAIN_AFTER_MS: i64 = 10 * 60_000;

/// What stands of the requests for a commit: its newest one.
#[derive(Debug, Clone)]
enum Stands<'a> {
    /// Handed to sykli (a run id) in the last ten minutes: not asked again.
    Asked(&'a Requested),
    /// Recorded, and could not be handed to sykli, in the last ten minutes:
    /// tried again at most that often.
    Failed(&'a Requested),
    /// Recorded, and nothing since: never handed to sykli unless the ohjaa
    /// that recorded it is handing it over now. Under the clone's lock that
    /// cannot be, and it is asked again at once, however old.
    Unconfirmed(&'a Requested),
    Nothing,
}

fn stands<'a>(requested: &'a [Requested], commit: &str, now: i64) -> Stands<'a> {
    let Some(newest) = requested
        .iter()
        .filter(|r| r.validation.commit == commit)
        .max_by_key(|r| r.validation.requested_ms)
    else {
        return Stands::Nothing;
    };
    let fresh = now - newest.validation.requested_ms < ASK_AGAIN_AFTER_MS;
    match (&newest.validation.run, &newest.validation.not_dispatched) {
        (Some(_), _) if fresh => Stands::Asked(newest),
        (None, Some(_)) if fresh => Stands::Failed(newest),
        (None, None) => Stands::Unconfirmed(newest),
        _ => Stands::Nothing,
    }
}

/// `; ohjaa asked sykli to validate it at <time> (request …, sykli run …):
/// no current receipt yet`.
fn asked_already(validation: &ValidationRef) -> String {
    format!(
        "; ohjaa asked sykli to validate it at {} (request {}{}), and sykli has no current \
         receipt of it yet",
        sessions::clock(validation.requested_ms),
        validation.request,
        match (&validation.run, &validation.not_dispatched) {
            (Some(run), _) => format!(", sykli run {run}"),
            (None, Some(why)) => format!(", never handed to sykli: {why}"),
            (None, None) => String::new(),
        }
    )
}

/// The words for a request recorded and never confirmed handed to sykli,
/// seen without the clone's lock: never dispatched, unless the session that
/// recorded it is live and its ohjaa still runs (handing it over now).
fn unconfirmed(requested: &Requested) -> String {
    let handing = !requested.ended
        && sessions::pid_of(&requested.session)
            .is_some_and(|pid| process::ohjaa_alive(pid, requested.started_ms) != Some(false));
    if handing {
        format!(
            "; request {} was recorded at {} by session {}, whose ohjaa is handing it to sykli now",
            requested.validation.request,
            sessions::clock(requested.validation.requested_ms),
            requested.session
        )
    } else {
        format!(
            "; request {} was recorded at {} by session {}: {NEVER_DISPATCHED}; the next launch \
             asks again",
            requested.validation.request,
            sessions::clock(requested.validation.requested_ms),
            requested.session
        )
    }
}

/// The validation lines of a briefing.
#[derive(Debug, Default)]
pub struct AtLaunch {
    pub lines: Vec<String>,
}

impl AtLaunch {
    /// The briefing part: empty when there is nothing to say.
    pub fn briefing(&self) -> String {
        if self.lines.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "Validation here (sykli's word, asked at launch; integrated: what is on the observed \
             branch, candidate: HEAD on its way there):\n",
        );
        for line in &self.lines {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// What sykli says of the observed branch's tip (the integrated State) and
/// of HEAD here when it is not the tip (a candidate), and whether HEAD was
/// validated on a base the branch has moved from since (`all`: the log's
/// sessions, read by the caller). Nothing is asked for: a request already
/// standing is said.
pub fn at_launch(
    marks: &Marks,
    sykli: &Sykli,
    head: Option<(&str, Option<&str>)>,
    all: &[sessions::Session],
    caps: &Caps,
) -> AtLaunch {
    let mut view = AtLaunch::default();
    let gate = &marks.gate;
    let Some(tip) = marks.commits.first() else {
        return view;
    };
    let said = if caps.status.commit {
        ask(gate, sykli, caps, &tip.commit)
    } else {
        said_by_marks(&tip.check)
    };
    let mut line = format!(
        "{}: {} of {} at {}: {}",
        standing(ValidationKind::Integrated, &said),
        gate.branch,
        gate.slug,
        short(&tip.commit),
        said.why()
    );
    if said == Said::None {
        let requested = requested_in(all, &gate.common);
        match stands(&requested, &tip.commit, now_ms()) {
            Stands::Asked(r) | Stands::Failed(r) => line.push_str(&asked_already(&r.validation)),
            Stands::Unconfirmed(r) => line.push_str(&unconfirmed(r)),
            Stands::Nothing => {}
        }
    }
    view.lines.push(line);
    let Some((head, branch)) = head else {
        return view;
    };
    if head == tip.commit {
        return view;
    }
    let named = branch.map_or_else(String::new, |branch| format!(" ({branch})"));
    let candidate = if caps.status.commit {
        ask(gate, sykli, caps, head)
    } else {
        Said::Unknown("this sykli cannot be asked (no `sykli status --commit`)".to_owned())
    };
    view.lines.push(format!(
        "{}: HEAD {}{named}: {}",
        standing(ValidationKind::Candidate, &candidate),
        short(head),
        candidate.why()
    ));
    if let Some(line) = stale_base(
        &recorded_in(all, &gate.common),
        head,
        &gate.branch,
        &tip.commit,
    ) {
        view.lines
            .push(format!("base moved since validation: HEAD {line}"));
    }
    view
}

/// `<commit7> was validated when origin/<branch> was at <B7>; it is at
/// <now7> now…` when the newest candidate validation of `commit` recorded
/// `origin/<branch>` elsewhere than `now`; `None` when it did not, or none
/// is recorded.
pub fn stale_base(
    candidates: &[ValidationRef],
    commit: &str,
    branch: &str,
    now: &str,
) -> Option<String> {
    let validated = candidates
        .iter()
        .filter(|v| v.kind == ValidationKind::Candidate && v.commit == commit)
        .filter_map(|v| {
            let observed = v.observed_branch.as_ref()?;
            (observed.name == branch).then_some((v.requested_ms, observed.tip.as_deref()?))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, base)| base)?;
    (validated != now).then(|| {
        format!(
            "{} was validated when origin/{branch} was at {}; it is at {} now: its merge is a \
             State no run has seen (flagged, never refused)",
            short(commit),
            short(validated),
            short(now)
        )
    })
}

/// The validations recorded in the clone whose common git dir is `common`,
/// candidate and integrated, from every session in the log.
pub fn recorded_in(all: &[sessions::Session], common: &Path) -> Vec<ValidationRef> {
    let common = canonical(common).to_string_lossy().into_owned();
    all.iter()
        .flat_map(|session| session.validations.iter())
        .filter(|v| v.common.as_deref() == Some(common.as_str()))
        .cloned()
        .collect()
}

/// At a push from the clone whose common git dir is `common`: for each
/// pushed `(commit, remote ref)` other than the observed branch, the flag
/// when a session validated that commit on a base `origin/<branch>` has
/// moved from since (this clone's `origin/<branch>`, read from its files).
/// Files only: the log, read-only, and the ref. Nothing when either cannot
/// be read.
pub fn stale_at_push(common: &Path, branch: &str, pushed: &[(String, String)]) -> Vec<String> {
    if pushed.is_empty() {
        return Vec::new();
    }
    let Some(now) = gitconfig::read_ref(common, &format!("refs/remotes/origin/{branch}")) else {
        return Vec::new();
    };
    let Ok(state) = crate::knowledge::state_home() else {
        return Vec::new();
    };
    // Never waited on: a log a writer holds now is not read.
    let Ok(log) = sessions::Reader::open_now(&sessions::store_path(&state.join("ohjaa"))) else {
        return Vec::new();
    };
    let Ok((all, _)) = log.all() else {
        return Vec::new();
    };
    drop(log);
    let candidates = recorded_in(&all, common);
    pushed
        .iter()
        .filter_map(|(commit, remote_ref)| {
            let line = stale_base(&candidates, commit, branch, &now)?;
            let name = remote_ref.strip_prefix("refs/heads/").unwrap_or(remote_ref);
            let line = line.replacen(short(commit), &format!("{} ({name})", short(commit)), 1);
            Some(format!(
                "ohjaa: base moved since validation: {line}. It is pushed; validate the integrated \
                 State once it lands."
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five states, and a candidate with no word yet: a pass or a
    /// failure only where a current receipt says so.
    #[test]
    fn the_states_are_named_from_sykli_word() {
        use ValidationKind::{Candidate, Integrated};
        assert_eq!(standing(Candidate, &Said::Passed), "candidate passed");
        assert_eq!(standing(Candidate, &Said::Failed), "candidate failed");
        assert_eq!(standing(Integrated, &Said::Passed), "integrated passed");
        assert_eq!(standing(Integrated, &Said::Failed), "integrated failed");
        for said in [
            Said::None,
            Said::Pending("r".to_owned()),
            Said::Conflict,
            Said::Unknown("x".to_owned()),
        ] {
            assert_eq!(standing(Integrated, &said), "integrated unverified");
        }
        let pending = said_of(&serde_json::json!({
            "verdict": "pending",
            "runs": [{"run": "r1", "state": "running"}, {"run": "r0", "state": "completed"}]
        }));
        assert_eq!(pending, Said::Pending("r1".to_owned()));
        assert_eq!(
            said_of(&serde_json::json!({"verdict": "conflict"})),
            Said::Conflict
        );
    }
}
