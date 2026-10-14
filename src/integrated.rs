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
//!   the entry's gate, ohjaa asks sykli for a durable run of exactly that
//!   tree. A `ValidationRef` of kind `integrated` is written into the
//!   session's record first (invariant A); then `sykli run --detach` runs in
//!   a checkout of the landed commit that ohjaa makes for it outside the
//!   repository (`$XDG_STATE_HOME/ohjaa/integrated/<clone>/<commit>`, a
//!   linked worktree of the clone, so its run and receipt are the clone's).
//!   ohjaa keeps the reference; what became of the run is sykli's to say.
//! - A candidate validated when `origin/<branch>` was at B, seen when it is
//!   elsewhere, is flagged "base moved since validation": at a push and in a
//!   briefing. Never refused (observe, mark and flag).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::gitconfig;
use crate::hook::facts_for_sykli;
use crate::lifecycle::{self, Gate, StatusHelp};
use crate::mark::{self, Marks};
use crate::process;
use crate::sessions::{self, Check, ObservedBranch, ValidationKind, ValidationRef};

/// What this machine's sykli can do here, asked once: `sykli status --help`
/// and, only where a run may be asked for, `sykli run --help`.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub status: StatusHelp,
    /// `sykli run --detach`: a durable run handed over (KATANA D3).
    pub detach: bool,
    run_help: String,
}

impl Caps {
    pub fn ask(dispatch: bool) -> Caps {
        let status = lifecycle::sykli_status_help();
        let run_help = if dispatch && status.commit {
            crate::gate::sykli_help("run")
        } else {
            String::new()
        };
        Caps {
            status,
            detach: process::names_flag(&run_help, "--detach"),
            run_help,
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

/// `sykli status --commit <rev> --contract <gate> --json`, from the gate's
/// checkout, handed the clone's facts when sykli takes them.
pub fn commit_status(gate: &Gate, help: StatusHelp, rev: &str) -> Result<Value, String> {
    let mut command = Command::new("sykli");
    command
        .args(["status", "--commit", rev, "--contract"])
        .arg(&gate.contract);
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

/// What sykli says of `rev` under `gate`, when this sykli can say.
pub fn ask(gate: &Gate, caps: &Caps, rev: &str) -> Said {
    if !caps.status.commit {
        return Said::Unknown("this sykli cannot be asked (no `sykli status --commit`)".to_owned());
    }
    match commit_status(gate, caps.status, rev) {
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

/// FNV-1a, 64 bits: a stable name for a clone's folder of validation
/// checkouts (no hashing crate is a dependency here, and nothing here needs
/// one that resists collisions on purpose).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Where ohjaa's validation checkouts live: `integrated/` in its state
/// directory (`$XDG_STATE_HOME/ohjaa`, beside the session log).
pub fn checkouts_root(state: &Path) -> PathBuf {
    state.join("integrated")
}

/// The checkout an integrated validation of `commit` runs in, for the clone
/// whose common git dir is `common`.
pub fn checkout_for(state: &Path, common: &Path, commit: &str) -> PathBuf {
    let common = canonical(common);
    let name = common
        .parent()
        .and_then(|main| main.file_name())
        .map_or_else(
            || "clone".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    checkouts_root(state)
        .join(format!(
            "{name}-{:016x}",
            fnv(common.to_string_lossy().as_bytes())
        ))
        .join(commit)
}

/// A request for an integrated validation, before it is recorded.
#[derive(Debug, Clone)]
pub struct Request {
    pub validation: ValidationRef,
    pub checkout: PathBuf,
}

/// The request for an integrated validation of `commit` (whose tree is
/// `tree`), the observed branch's tip, under `gate`.
pub fn plan(gate: &Gate, commit: &str, tree: &str, state: &Path) -> Request {
    let at = now_ms();
    let checkout = checkout_for(state, &gate.common, commit);
    let validation = ValidationRef {
        request: sessions::new_request_id(at),
        run: None,
        kind: ValidationKind::Integrated,
        repo: checkout.to_string_lossy().into_owned(),
        commit: commit.to_owned(),
        tree: tree.to_owned(),
        contract: gate.contract.to_string_lossy().into_owned(),
        contract_digest: crate::gate::pinned(&gate.contract),
        lock_digest: None,
        requested_ms: at,
        receipt: None,
        common: Some(canonical(&gate.common).to_string_lossy().into_owned()),
        observed_branch: Some(ObservedBranch {
            name: gate.branch.clone(),
            tip: Some(commit.to_owned()),
            updated_ms: mark::updated(&gate.common, &gate.branch).map(|at| at.ms),
        }),
        ran: None,
        not_dispatched: None,
    };
    Request {
        validation,
        checkout,
    }
}

/// The commit a checkout's HEAD names, from its files.
fn checkout_head(checkout: &Path) -> Option<String> {
    let dot_git = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let gitdir = checkout.join(dot_git.strip_prefix("gitdir:")?.trim());
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    Some(head.trim().to_owned())
}

/// A checkout of exactly `commit` at `checkout`: a linked worktree of the
/// gate's clone, detached, made without running the repository's hooks
/// (ohjaa's own step, not the person's checkout). One made for this commit
/// before is used again, while it still sits there.
fn ensure_checkout(gate: &Gate, checkout: &Path, commit: &str) -> Result<(), String> {
    if checkout.join(".git").exists() {
        return match checkout_head(checkout) {
            Some(head) if head == commit => Ok(()),
            Some(head) => Err(format!(
                "its checkout {} is at {head}, not {commit}",
                checkout.display()
            )),
            None => Err(format!(
                "its checkout {} cannot be read",
                checkout.display()
            )),
        };
    }
    if let Some(parent) = checkout.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let mut command = Command::new("git");
    command
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(&gate.top)
        .args(["worktree", "add", "--detach", "--force", "-q"])
        .arg(checkout)
        .arg(commit);
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    match process::run(&mut command, None, process::LOCAL) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "git worktree add failed: {}",
            output.failure_detail()
        )),
        Err(error) => Err(format!("git could not be asked: {error:#}")),
    }
}

/// Hand a recorded request to sykli: its checkout, then `sykli run
/// <gate> --detach --json` there. sykli's run id, or why not.
pub fn dispatch(gate: &Gate, request: &Request, caps: &Caps) -> Result<String, String> {
    if !caps.detach {
        return Err("this sykli cannot run it detached (no `sykli run --detach`)".to_owned());
    }
    ensure_checkout(gate, &request.checkout, &request.validation.commit)?;
    let facts = facts_for_sykli(&caps.run_help, Some(&request.checkout), &gate.common);
    let mut command = Command::new("sykli");
    command
        .arg("run")
        .arg(&gate.contract)
        .args(&facts)
        .args(["--detach", "--json"])
        .current_dir(&request.checkout)
        // Not the run of any session: nothing it starts records into one.
        .env_remove(sessions::SESSION_ENV);
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    let output = process::run(&mut command, None, process::LOCAL)
        .map_err(|error| format!("sykli could not be run: {error:#}"))?;
    if !output.status.success() {
        return Err(format!(
            "sykli run --detach exited {}: {}",
            output.status.code().unwrap_or(1),
            output.failure_detail()
        ));
    }
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    answer["run"]
        .as_str()
        .filter(|_| answer["schema"] == "sykli-run.v1")
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "sykli run --detach named no run (sykli-run.v1): {}",
                output.stdout_text().trim()
            )
        })
}

/// What became of asking for one integrated validation in a session.
#[derive(Debug)]
pub enum Asked {
    /// Recorded in the session first, then handed to sykli: its run id, or
    /// why it was not (recorded too).
    Recorded {
        request: String,
        run: Result<String, String>,
    },
    /// Not recorded, so never handed to sykli (invariant A): why.
    NotRecorded(String),
}

/// Fill in what became of a recorded request.
fn fill(log: &Path, session: &str, request: &str, change: impl FnOnce(&mut ValidationRef)) {
    let written = sessions::Store::open(log).and_then(|store| {
        store.update(session, |s| {
            if let Some(validation) = s.validations.iter_mut().find(|v| v.request == request) {
                change(validation);
            }
        })
    });
    if let Err(error) = written {
        eprintln!("ohjaa: {error}: what sykli gave for request {request} is not recorded");
    }
}

/// Dispatch a request already written into `session`'s record (invariant
/// A: the caller wrote it before), and fill in sykli's run id, or why not.
pub fn dispatch_recorded(
    log: &Path,
    session: &str,
    gate: &Gate,
    request: &Request,
    caps: &Caps,
) -> Asked {
    let id = request.validation.request.clone();
    let run = dispatch(gate, request, caps);
    fill(log, session, &id, |validation| match &run {
        Ok(run) => validation.run = Some(run.clone()),
        Err(why) => validation.not_dispatched = Some(why.clone()),
    });
    Asked::Recorded { request: id, run }
}

/// Record `request` in `session`'s record, then dispatch it.
pub fn request_in_session(
    log: &Path,
    session: &str,
    gate: &Gate,
    request: &Request,
    caps: &Caps,
) -> Asked {
    let validation = request.validation.clone();
    let written = sessions::Store::open(log)
        .and_then(|store| store.update(session, |s| s.validations.push(validation)));
    match written {
        Ok(Some(_)) => dispatch_recorded(log, session, gate, request, caps),
        Ok(None) => Asked::NotRecorded(format!("session {session} is not in the session log")),
        Err(error) => Asked::NotRecorded(error.to_string()),
    }
}

/// At leaving: a commit this session saw land on `gate`'s observed branch,
/// which the branch still sits at. sykli is asked what applies to it; with
/// no current receipt of its tree and no run of it, an integrated
/// validation is recorded in the session, then dispatched. The line to say.
pub fn at_landing(
    log: &Path,
    state: &Path,
    session: &str,
    gate: &Gate,
    commit: &str,
    recorded: &[ValidationRef],
    caps: &Caps,
) -> String {
    let onto = format!("origin/{}", gate.branch);
    let said = ask(gate, caps, commit);
    let head = format!(
        "{}: {} on {onto}",
        standing(ValidationKind::Integrated, &said),
        short(commit)
    );
    if said != Said::None {
        return format!("{head} ({})", said.why());
    }
    if let Some(asked) = standing_request(recorded, commit) {
        return format!(
            "{head} has no current sykli receipt{}",
            asked_already(asked)
        );
    }
    if !caps.detach {
        return format!(
            "{head} has no current sykli receipt; this sykli cannot run it detached (no `sykli \
             run --detach`), so no run was asked for"
        );
    }
    let tree = match tree_of(&gate.top, commit) {
        Ok(tree) => tree,
        Err(why) => {
            return format!("{head} has no current sykli receipt; its tree cannot be read ({why})")
        }
    };
    let request = plan(gate, commit, &tree, state);
    match request_in_session(log, session, gate, &request, caps) {
        Asked::Recorded {
            request,
            run: Ok(run),
        } => format!(
            "{head} has no current sykli receipt; ohjaa asked sykli to validate it (request \
             {request}, sykli run {run}, in {})",
            checkout_for(state, &gate.common, commit).display()
        ),
        Asked::Recorded {
            request,
            run: Err(why),
        } => format!(
            "{head} has no current sykli receipt; ohjaa recorded request {request}, and could \
             not hand it to sykli: {why}"
        ),
        Asked::NotRecorded(why) => format!(
            "{head} has no current sykli receipt; no run was asked for, since the request could \
             not be recorded first ({why})"
        ),
    }
}

/// A commit's tree: one git process.
fn tree_of(top: &Path, commit: &str) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(top)
        .args(["rev-parse", "--verify", "-q", &format!("{commit}^{{tree}}")]);
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    match process::run(&mut command, None, process::LOCAL) {
        Ok(output) if output.status.success() => Ok(output.stdout_text().trim().to_owned()),
        Ok(output) => Err(output.failure_detail()),
        Err(error) => Err(format!("{error:#}")),
    }
}

/// How long a request stands before sykli is asked for another run of the
/// same commit, while sykli still has no current receipt of it (a run that
/// was lost or interrupted ends with none).
const ASK_AGAIN_AFTER_MS: i64 = 10 * 60_000;

/// A request for `commit` made recently enough to stand: sykli is not asked
/// for another run while it does (one that could not be handed over too:
/// tried again at most once in [`ASK_AGAIN_AFTER_MS`]).
fn standing_request<'a>(requested: &'a [ValidationRef], commit: &str) -> Option<&'a ValidationRef> {
    let now = now_ms();
    requested
        .iter()
        .filter(|v| {
            v.kind == ValidationKind::Integrated
                && v.commit == commit
                && now - v.requested_ms < ASK_AGAIN_AFTER_MS
        })
        .max_by_key(|v| v.requested_ms)
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

/// How a launch-time view is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `ohjaa run`: an unverified tip with no run is asked for.
    Launch,
    /// `ohjaa run --dry-run`: said what a launch would ask for.
    DryRun,
    /// `ohjaa brief`: said, nothing asked for.
    Brief,
}

/// The validation lines of a briefing, and the request a launch records
/// with its session, then dispatches.
#[derive(Debug, Default)]
pub struct AtLaunch {
    pub lines: Vec<String>,
    pub request: Option<Request>,
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
/// validated on a base the branch has moved from since (`candidates`: the
/// validations this clone's sessions recorded). In `Mode::Launch`, a tip
/// with no current receipt and no run of it gets a request (not yet
/// recorded: the caller records it with the session, then dispatches).
pub fn at_launch(
    marks: &Marks,
    head: Option<(&str, Option<&str>)>,
    recorded: &[ValidationRef],
    mode: Mode,
    state: &Path,
    caps: &Caps,
) -> AtLaunch {
    let mut view = AtLaunch::default();
    let gate = &marks.gate;
    let Some(tip) = marks.commits.first() else {
        return view;
    };
    let said = if caps.status.commit {
        ask(gate, caps, &tip.commit)
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
    let asked = standing_request(recorded, &tip.commit);
    if said == Said::None {
        match mode {
            _ if asked.is_some() => {
                line.push_str(&asked_already(asked.expect("checked")));
            }
            Mode::Launch if caps.detach => {
                let request = plan(gate, &tip.commit, &tip.tree, state);
                line.push_str(&format!(
                    "; ohjaa asks sykli to validate it: request {}, recorded in this session \
                     before sykli is asked",
                    request.validation.request
                ));
                view.request = Some(request);
            }
            Mode::DryRun if caps.detach => {
                line.push_str("; would ask sykli to validate it (a dry run asks for nothing)")
            }
            Mode::Launch | Mode::DryRun => line.push_str(
                "; this sykli cannot run it detached (no `sykli run --detach`), so no run is asked for",
            ),
            Mode::Brief => {}
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
        ask(gate, caps, head)
    } else {
        Said::Unknown("this sykli cannot be asked (no `sykli status --commit`)".to_owned())
    };
    view.lines.push(format!(
        "{}: HEAD {}{named}: {}",
        standing(ValidationKind::Candidate, &candidate),
        short(head),
        candidate.why()
    ));
    if let Some(line) = stale_base(recorded, head, &gate.branch, &tip.commit) {
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

    fn candidate(commit: &str, base: &str, at: i64) -> ValidationRef {
        ValidationRef {
            request: format!("rq-{at}-1"),
            run: None,
            kind: ValidationKind::Candidate,
            repo: "/r".to_owned(),
            commit: commit.to_owned(),
            tree: "t".to_owned(),
            contract: "/h/sykli.json".to_owned(),
            contract_digest: None,
            lock_digest: None,
            requested_ms: at,
            receipt: None,
            common: Some("/r/.git".to_owned()),
            observed_branch: Some(ObservedBranch {
                name: "main".to_owned(),
                tip: Some(base.to_owned()),
                updated_ms: None,
            }),
            ran: None,
            not_dispatched: None,
        }
    }

    /// The newest validation of a commit says what base it saw: flagged
    /// only when the branch is elsewhere now.
    #[test]
    fn a_base_moved_since_the_newest_validation_is_said() {
        let refs = [
            candidate("c0ffee1aaa", "base000aaa", 1),
            candidate("c0ffee1aaa", "base111aaa", 2),
        ];
        assert_eq!(stale_base(&refs, "c0ffee1aaa", "main", "base111aaa"), None);
        let said = stale_base(&refs, "c0ffee1aaa", "main", "moved22aaa").unwrap();
        assert!(
            said.starts_with(
                "c0ffee1 was validated when origin/main was at base111; it is at moved22 now"
            ),
            "{said}"
        );
        assert_eq!(stale_base(&refs, "other00aaa", "main", "moved22aaa"), None);
        assert_eq!(stale_base(&refs, "c0ffee1aaa", "trunk", "moved22aaa"), None);
    }

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

    #[test]
    fn a_clone_has_one_folder_of_validation_checkouts() {
        let state = Path::new("/s");
        let a = checkout_for(state, Path::new("/nowhere/widget/.git"), "c1");
        let b = checkout_for(state, Path::new("/nowhere/widget/.git"), "c2");
        let other = checkout_for(state, Path::new("/elsewhere/widget/.git"), "c1");
        assert_eq!(a.parent(), b.parent());
        assert_ne!(a.parent(), other.parent());
        assert!(
            a.to_string_lossy().starts_with("/s/integrated/widget-"),
            "{}",
            a.display()
        );
        assert!(a.ends_with("c1"));
    }
}
