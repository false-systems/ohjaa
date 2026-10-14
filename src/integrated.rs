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
//!   tree. Under the clone's lock, a `ValidationRef` of kind `integrated` is
//!   written into the session's record first (invariant A); then `sykli run
//!   --detach` runs in the clone's one rolling validation checkout
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
use std::time::Duration;

use serde_json::Value;

use crate::gitconfig;
use crate::hook::facts_for_sykli;
use crate::lifecycle::{self, Gate, StatusHelp};
use crate::mark::{self, Marks};
use crate::process::{self, FileLock};
use crate::sessions::{self, Check, ObservedBranch, ReceiptRef, ValidationKind, ValidationRef};

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

/// FNV-1a, 64 bits: a stable name for a clone's folders in ohjaa's state (no
/// hashing crate is a dependency here, and nothing here needs one that
/// resists collisions on purpose).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A clone's name in ohjaa's state: its main checkout's folder name and a
/// hash of its common git dir (`widget-0123456789abcdef`).
pub fn clone_id(common: &Path) -> String {
    let common = canonical(common);
    let name = common
        .parent()
        .and_then(|main| main.file_name())
        .map_or_else(
            || "clone".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    format!("{name}-{:016x}", fnv(common.to_string_lossy().as_bytes()))
}

/// Where ohjaa's validation folders live: `integrated/` in its state
/// directory (`$XDG_STATE_HOME/ohjaa`, beside the session log).
pub fn checkouts_root(state: &Path) -> PathBuf {
    state.join("integrated")
}

/// How long a launch or a leaving waits for another ohjaa that is asking
/// sykli to validate the same clone.
const LOCK_WAIT: Duration = Duration::from_secs(20);

/// A clone's folder of integrated validation, `integrated/<clone>/` in
/// ohjaa's state: its one rolling checkout, the cargo config and target dir
/// that checkout builds with, the receipts kept of its runs, and its lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneDir {
    pub dir: PathBuf,
}

impl CloneDir {
    /// The folder for the clone whose common git dir is `common`.
    pub fn of(state: &Path, common: &Path) -> CloneDir {
        CloneDir {
            dir: checkouts_root(state).join(clone_id(common)),
        }
    }

    /// The rolling validation checkout: a linked worktree of the clone,
    /// reset to the commit before each run.
    pub fn checkout(&self) -> PathBuf {
        self.dir.join("checkout")
    }

    /// Where cargo builds what runs in the checkout: never a target dir
    /// another checkout shares (`~/.cargo/config.toml`'s `build.target-dir`,
    /// say), whose newer artifacts of another tree cargo would take as fresh.
    pub fn target(&self) -> PathBuf {
        self.dir.join("target")
    }

    /// `.cargo/config.toml` in the folder above the checkout: cargo reads
    /// the config of every folder above the one it runs in, and a deeper
    /// one wins over `$CARGO_HOME/config.toml`. Nothing is written into the
    /// checkout.
    pub fn cargo_config(&self) -> PathBuf {
        self.dir.join(".cargo").join("config.toml")
    }

    /// Copies of the receipts the checkout's runs made, by request.
    pub fn receipts(&self) -> PathBuf {
        self.dir.join("receipts")
    }

    /// Held from deciding to ask sykli to the run handed over: one ohjaa at
    /// a time records a request and dispatches it in this clone.
    pub fn lock(&self) -> Result<FileLock, String> {
        process::lock_file(&self.dir.join("lock"), LOCK_WAIT)
    }
}

/// A request for an integrated validation, before it is recorded.
#[derive(Debug, Clone)]
pub struct Request {
    pub validation: ValidationRef,
    pub checkout: PathBuf,
    /// An earlier request of the same commit that was recorded and never
    /// handed to sykli (its ohjaa stopped first): `(session, request)`.
    pub supersedes: Option<(String, String)>,
}

/// The request for an integrated validation of `commit` (whose tree is
/// `tree`), the observed branch's tip, under `gate`.
pub fn plan(gate: &Gate, commit: &str, tree: &str, state: &Path) -> Request {
    let at = now_ms();
    let checkout = CloneDir::of(state, &gate.common).checkout();
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
        supersedes: None,
    }
}

/// The commit a checkout's HEAD names, from its files.
fn checkout_head(checkout: &Path) -> Option<String> {
    let dot_git = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let gitdir = checkout.join(dot_git.strip_prefix("gitdir:")?.trim());
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    Some(head.trim().to_owned())
}

/// `git <args>`, run where `dir` says, as ohjaa's own step: no hook of the
/// repository's runs, and no repository named by a hook's environment.
fn git_in(dir: &Path, args: &[&str]) -> Result<process::Output, String> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.hooksPath=/dev/null", "-C"])
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C");
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    process::run(&mut command, None, process::LOCAL)
        .map_err(|error| format!("git could not be asked: {error:#}"))
}

fn git_ok_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_in(dir, args)?;
    if output.status.success() {
        Ok(output.stdout_text())
    } else {
        Err(output.failure_detail())
    }
}

/// The clone's validation checkout, at exactly `commit` and clean. A
/// checkout there is reset ([`reset_checkout`]) and verified
/// ([`verify_checkout`]); when either fails, it is made again from nothing
/// (`git worktree remove --force`, then `git worktree add --detach --force`)
/// and verified again. A run never starts in another run's leftovers, and a
/// leftover never blocks validation for good.
fn ensure_checkout(gate: &Gate, clone: &CloneDir, commit: &str) -> Result<(), String> {
    let checkout = clone.checkout();
    if checkout.join(".git").exists()
        && reset_checkout(&checkout, commit)
            .and_then(|()| verify_checkout(&checkout, commit))
            .is_ok()
    {
        return Ok(());
    }
    if checkout.exists() {
        let path = checkout.to_string_lossy().into_owned();
        let removed = git_ok_in(
            &gate.top,
            &["worktree", "remove", "--force", "--force", &path],
        );
        if removed.is_err() && checkout.exists() {
            std::fs::remove_dir_all(&checkout)
                .map_err(|error| format!("cannot remove {}: {error}", checkout.display()))?;
        }
    }
    std::fs::create_dir_all(&clone.dir)
        .map_err(|error| format!("cannot create {}: {error}", clone.dir.display()))?;
    let path = checkout.to_string_lossy().into_owned();
    git_ok_in(
        &gate.top,
        &[
            "worktree", "add", "--detach", "--force", "-q", &path, commit,
        ],
    )
    .map_err(|why| format!("git worktree add failed: {why}"))?;
    verify_checkout(&checkout, commit)
}

/// Reset the checkout to `commit`: `checkout -f --detach`; every submodule
/// a run initialised taken out again (`submodule deinit -f --all`: its work
/// tree emptied, ignored files and changes with it, as in a fresh checkout,
/// and no network: the gate initialises what it needs); then `clean -ffdx`
/// of everything but sykli's own folder at the top (`/.sykli`, anchored: a
/// `.sykli` deeper down is a run's leftover like any other).
fn reset_checkout(checkout: &Path, commit: &str) -> Result<(), String> {
    git_ok_in(checkout, &["checkout", "-q", "-f", "--detach", commit])?;
    if checkout.join(".gitmodules").exists() {
        git_ok_in(checkout, &["submodule", "deinit", "-q", "-f", "--all"])?;
    }
    git_ok_in(checkout, &["clean", "-q", "-ffdx", "-e", "/.sykli"])?;
    Ok(())
}

/// The checkout is exactly `commit`: HEAD is it; `git status` (untracked and
/// ignored files too, and changes inside submodules) says nothing but
/// sykli's folder at the top; and no submodule is populated (`submodule
/// foreach --recursive`, where there are submodules).
fn verify_checkout(checkout: &Path, commit: &str) -> Result<(), String> {
    let left = git_ok_in(
        checkout,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
            "--ignore-submodules=none",
            "--",
            ".",
            ":(exclude).sykli",
        ],
    )
    .map_err(|why| format!("its checkout's state cannot be read: {why}"))?;
    if !left.trim().is_empty() {
        return Err(format!(
            "its checkout {} is not clean after its reset: {}",
            checkout.display(),
            left.lines().next().unwrap_or_default()
        ));
    }
    if checkout.join(".gitmodules").exists() {
        let populated = git_ok_in(
            checkout,
            &["submodule", "foreach", "--quiet", "--recursive", "pwd"],
        )
        .map_err(|why| format!("its checkout's submodules cannot be read: {why}"))?;
        if !populated.trim().is_empty() {
            return Err(format!(
                "its checkout {} still holds a populated submodule: {}",
                checkout.display(),
                populated.lines().next().unwrap_or_default()
            ));
        }
    }
    match checkout_head(checkout) {
        Some(head) if head == commit => Ok(()),
        head => Err(format!(
            "its checkout {} is at {}, not {commit}",
            checkout.display(),
            head.unwrap_or_else(|| "an unreadable HEAD".to_owned())
        )),
    }
}

/// `<clone>/.cargo/config.toml`, naming `<clone>/target`: written when it
/// does not say so already.
fn ensure_cargo_config(clone: &CloneDir) -> Result<(), String> {
    let text = format!(
        "# Written by ohjaa. cargo, run in ohjaa's validation checkout of this clone (checkout/\n\
         # below), builds here, never in a target dir another checkout shares: cargo would take\n\
         # another tree's newer artifacts there as fresh.\n\
         [build]\n\
         target-dir = {}\n",
        crate::flat_toml::quote(&clone.target().to_string_lossy())
    );
    let path = clone.cargo_config();
    if std::fs::read_to_string(&path).is_ok_and(|said| said == text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(&path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Hand a recorded request to sykli: the clone's checkout, reset to the
/// commit and verified clean, then `sykli run <gate> --detach --json` there.
/// sykli's run id, or why not. The caller holds the clone's lock.
pub fn dispatch(
    gate: &Gate,
    request: &Request,
    caps: &Caps,
    state: &Path,
) -> Result<String, String> {
    if !caps.detach {
        return Err("this sykli cannot run it detached (no `sykli run --detach`)".to_owned());
    }
    let clone = CloneDir::of(state, &gate.common);
    ensure_cargo_config(&clone)?;
    ensure_checkout(gate, &clone, &request.validation.commit)?;
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
        eprintln!("ohjaa: {error}: what became of request {request} is not recorded");
    }
}

/// Dispatch a request already written into `session`'s record (invariant
/// A: the caller wrote it before, holding the clone's lock), and fill in
/// sykli's run id, or why not; and, for a request that supersedes one never
/// dispatched, say so in that one's record.
pub fn dispatch_recorded(
    log: &Path,
    state: &Path,
    session: &str,
    gate: &Gate,
    request: &Request,
    caps: &Caps,
) -> Asked {
    let id = request.validation.request.clone();
    let run = dispatch(gate, request, caps, state);
    fill(log, session, &id, |validation| match &run {
        Ok(run) => validation.run = Some(run.clone()),
        Err(why) => validation.not_dispatched = Some(why.clone()),
    });
    if let Some((old_session, old_request)) = &request.supersedes {
        let why = format!("{STOPPED}; asked again as request {id} by session {session}");
        fill(log, old_session, old_request, |validation| {
            if validation.run.is_none() && validation.not_dispatched.is_none() {
                validation.not_dispatched = Some(why);
            }
        });
    }
    Asked::Recorded { request: id, run }
}

/// Record `request` in `session`'s record, then dispatch it. The caller
/// holds the clone's lock.
pub fn request_in_session(
    log: &Path,
    state: &Path,
    session: &str,
    gate: &Gate,
    request: &Request,
    caps: &Caps,
) -> Asked {
    let validation = request.validation.clone();
    let written = sessions::Store::open(log)
        .and_then(|store| store.update(session, |s| s.validations.push(validation)));
    match written {
        Ok(Some(_)) => dispatch_recorded(log, state, session, gate, request, caps),
        Ok(None) => Asked::NotRecorded(format!("session {session} is not in the session log")),
        Err(error) => Asked::NotRecorded(error.to_string()),
    }
}

/// Why a request was recorded and never handed to sykli, when nothing
/// says otherwise: its ohjaa stopped in between.
pub const STOPPED: &str = "ohjaa stopped before asking sykli";
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

/// What a run already in the clone's checkout leaves for the next one.
enum Previous {
    /// Nothing runs there: the checkout may be reset.
    Free(Option<String>),
    /// A run there may still be going: `(run, commit, why)`.
    Busy(String, String, String),
}

/// Why sykli's word on a run (`sykli-run-status.v1`) leaves its checkout
/// busy: the run requested or running; any task still running or any
/// attempt not finished (a run sykli calls `lost`, its coordinator gone,
/// may still have one); or any attempt's process group still alive (a task
/// that left a process behind). `None`: every task and attempt has ended,
/// and none of their groups is left.
fn still_going(status: &Value, group_exists: &dyn Fn(u32) -> Option<bool>) -> Option<String> {
    let state = status["state"].as_str().unwrap_or("unknown");
    if matches!(state, "requested" | "running") {
        return Some(format!("sykli says it is {state}"));
    }
    for task in status["tasks"].as_array().map_or(&[][..], Vec::as_slice) {
        let name = task["name"].as_str().unwrap_or("?");
        if task["state"] == "running" {
            return Some(format!(
                "sykli says the run is {state}, its task {name} still running"
            ));
        }
        for attempt in task["attempts"].as_array().map_or(&[][..], Vec::as_slice) {
            if attempt.is_object() && attempt["outcome"].is_null() {
                return Some(format!(
                    "sykli says the run is {state}, an attempt of its task {name} not finished"
                ));
            }
            let Some(group) = attempt["process_group"].as_u64() else {
                continue;
            };
            let Ok(group) = u32::try_from(group) else {
                continue;
            };
            match group_exists(group) {
                Some(false) => {}
                Some(true) => {
                    return Some(format!(
                        "process group {group} of its task {name} still runs (sykli says the run \
                         is {state})"
                    ))
                }
                None => {
                    return Some(format!(
                        "whether process group {group} of its task {name} still runs cannot be \
                         told"
                    ))
                }
            }
        }
    }
    None
}

/// Before the checkout is reset for another run: the newest run handed to
/// sykli there. While it may still be going, the checkout is busy and
/// nothing is reset under it (see [`still_going`]); so too while sykli
/// cannot say what became of it, or cannot be asked. Ended: its receipt
/// (which lives in the checkout's `.sykli/`, kept across resets, until
/// sykli prunes it) is copied into ohjaa's state,
/// `<clone>/receipts/<request>.json`, and recorded with its digest in that
/// request's `ValidationRef`, so no evidence goes with a checkout. One
/// `sykli status --run`, only while a run there has no receipt recorded. A
/// checkout a person removed holds nothing to reset under.
fn previous_run(log: &Path, clone: &CloneDir, requested: &[Requested], caps: &Caps) -> Previous {
    let checkout = clone.checkout().to_string_lossy().into_owned();
    let Some(last) = requested
        .iter()
        .filter(|r| r.validation.repo == checkout && r.validation.run.is_some())
        .max_by_key(|r| r.validation.requested_ms)
    else {
        return Previous::Free(None);
    };
    let run = last.validation.run.clone().unwrap_or_default();
    if last.validation.receipt.is_some() || !clone.checkout().exists() {
        return Previous::Free(None);
    }
    let busy = |why: String| Previous::Busy(run.clone(), last.validation.commit.clone(), why);
    if !caps.status.runs {
        return busy(
            "this sykli cannot be asked what became of it (no `sykli status --run`)".to_owned(),
        );
    }
    let common = last.validation.common.as_deref().map(Path::new);
    let status = match lifecycle::sykli_run_status(&clone.checkout(), common, caps.status, &run) {
        Ok(status) => status,
        Err(why) => return busy(format!("sykli cannot say what became of it ({why})")),
    };
    if let Some(why) = still_going(&status, &process::group_exists) {
        return busy(why);
    }
    let Some(receipt) = lifecycle::receipt_of_status(&status, &clone.checkout()) else {
        return Previous::Free(None);
    };
    let kept = clone
        .receipts()
        .join(format!("{}.json", last.validation.request));
    let copied = std::fs::create_dir_all(clone.receipts())
        .and_then(|()| std::fs::copy(&receipt.path, &kept));
    let (path, said) = match copied {
        Ok(_) => (kept.to_string_lossy().into_owned(), None),
        Err(error) => (
            receipt.path.clone(),
            Some(format!(
                "the receipt of run {run} could not be copied into ohjaa's state ({error})"
            )),
        ),
    };
    let ran = lifecycle::ran_of_status(&status);
    let digest = receipt.digest.clone();
    fill(log, &last.session, &last.validation.request, |validation| {
        validation.receipt = Some(ReceiptRef { path, digest });
        if validation.ran.is_none() {
            validation.ran = ran;
        }
    });
    Previous::Free(said)
}

/// What a launch or a leaving does about an unverified commit (sykli has
/// no current receipt of its tree, and can run one detached).
enum Decided {
    /// Not asked now: the words why.
    Not(String),
    /// Asked: the request to record, then dispatch, holding the lock until
    /// it is handed over; and the words.
    Ask(Box<Request>, FileLock, String),
}

/// Under the clone's lock, with the log read again (another ohjaa may have
/// asked while this one waited): a request of the commit that stands is
/// said, never repeated; one recorded and never dispatched is asked again
/// at once; a checkout busy with a run is not reset under it.
fn decide(log: &Path, state: &Path, gate: &Gate, commit: &str, tree: &str, caps: &Caps) -> Decided {
    let clone = CloneDir::of(state, &gate.common);
    let lock = match clone.lock() {
        Ok(lock) => lock,
        Err(why) => {
            return Decided::Not(format!(
                "; not asked: another ohjaa is asking sykli to validate this clone now ({why})"
            ))
        }
    };
    let all = match sessions::Reader::open(log).and_then(|reader| reader.all()) {
        Ok((all, _)) => all,
        Err(error) => {
            return Decided::Not(format!(
                "; not asked: the session log cannot be read, so no request can be recorded \
                 first ({error})"
            ))
        }
    };
    let requested = requested_in(&all, &gate.common);
    let mut said = String::new();
    let mut supersedes = None;
    match stands(&requested, commit, now_ms()) {
        Stands::Asked(r) | Stands::Failed(r) => {
            return Decided::Not(asked_already(&r.validation));
        }
        Stands::Unconfirmed(r) => {
            said = format!(
                "; request {} was recorded at {} by session {}: {NEVER_DISPATCHED}",
                r.validation.request,
                sessions::clock(r.validation.requested_ms),
                r.session
            );
            supersedes = Some((r.session.clone(), r.validation.request.clone()));
        }
        Stands::Nothing => {}
    }
    match previous_run(log, &clone, &requested, caps) {
        Previous::Busy(run, of, why) => {
            return Decided::Not(format!(
                "{said}; the validation checkout is busy with sykli run {run} (of {}: {why}), so \
                 none is asked for now and nothing is reset under it: a later launch asks (if \
                 that run is gone for good, removing {} lets the next launch start afresh)",
                short(&of),
                clone.checkout().display()
            ))
        }
        Previous::Free(Some(note)) => said.push_str(&format!("; {note}")),
        Previous::Free(None) => {}
    }
    let mut request = plan(gate, commit, tree, state);
    request.supersedes = supersedes;
    Decided::Ask(Box::new(request), lock, said)
}

/// At leaving: a commit this session saw land on `gate`'s observed branch,
/// which the branch still sits at. sykli is asked what applies to it; with
/// no current receipt of its tree and no request of it standing, an
/// integrated validation is recorded in the session, then dispatched, under
/// the clone's lock. The line to say.
pub fn at_landing(
    log: &Path,
    state: &Path,
    session: &str,
    gate: &Gate,
    commit: &str,
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
    let (request, lock, words) = match decide(log, state, gate, commit, &tree, caps) {
        Decided::Not(words) => return format!("{head} has no current sykli receipt{words}"),
        Decided::Ask(request, lock, words) => (request, lock, words),
    };
    let line = match request_in_session(log, state, session, gate, &request, caps) {
        Asked::Recorded {
            request: id,
            run: Ok(run),
        } => format!(
            "{head} has no current sykli receipt{words}; ohjaa asked sykli to validate it \
             (request {id}, sykli run {run}, in {})",
            request.checkout.display()
        ),
        Asked::Recorded {
            request: id,
            run: Err(why),
        } => format!(
            "{head} has no current sykli receipt{words}; ohjaa recorded request {id}, and could \
             not hand it to sykli: {why}"
        ),
        Asked::NotRecorded(why) => format!(
            "{head} has no current sykli receipt{words}; no run was asked for, since the request \
             could not be recorded first ({why})"
        ),
    };
    drop(lock);
    line
}

/// A commit's tree: one git process.
fn tree_of(top: &Path, commit: &str) -> Result<String, String> {
    git_ok_in(
        top,
        &["rev-parse", "--verify", "-q", &format!("{commit}^{{tree}}")],
    )
    .map(|tree| tree.trim().to_owned())
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
/// with its session, then dispatches, holding the clone's lock until then.
#[derive(Debug, Default)]
pub struct AtLaunch {
    pub lines: Vec<String>,
    pub request: Option<Request>,
    /// The clone's lock, held from deciding to ask to the dispatch: drop it
    /// once the request is handed over (or cannot be recorded).
    pub lock: Option<FileLock>,
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
/// sessions, read by the caller). In `Mode::Launch`, a tip with no current
/// receipt and no request of it standing gets a request, decided under the
/// clone's lock with the log (`log`) read again (not yet recorded: the
/// caller records it with the session, then dispatches, then drops the
/// lock).
pub fn at_launch(
    marks: &Marks,
    head: Option<(&str, Option<&str>)>,
    all: &[sessions::Session],
    mode: Mode,
    state: &Path,
    log: &Path,
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
    if said == Said::None {
        let requested = requested_in(all, &gate.common);
        match (mode, caps.detach) {
            (Mode::Launch, true) => {
                match decide(log, state, gate, &tip.commit, &tip.tree, caps) {
                    Decided::Not(words) => line.push_str(&words),
                    Decided::Ask(request, lock, words) => {
                        line.push_str(&format!(
                            "{words}; ohjaa asks sykli to validate it: request {}, recorded in \
                             this session before sykli is asked",
                            request.validation.request
                        ));
                        view.request = Some(*request);
                        view.lock = Some(lock);
                    }
                }
            }
            (Mode::Launch | Mode::DryRun, false) => line.push_str(
                "; this sykli cannot run it detached (no `sykli run --detach`), so no run is asked for",
            ),
            (Mode::DryRun | Mode::Brief, _) => {
                match stands(&requested, &tip.commit, now_ms()) {
                    Stands::Asked(r) | Stands::Failed(r) => {
                        line.push_str(&asked_already(&r.validation))
                    }
                    Stands::Unconfirmed(r) => line.push_str(&unconfirmed(r)),
                    Stands::Nothing if mode == Mode::DryRun => line
                        .push_str("; would ask sykli to validate it (a dry run asks for nothing)"),
                    Stands::Nothing => {}
                }
            }
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

    /// One rolling checkout per clone, whatever the commit: its folder
    /// holds the checkout, cargo's config and target dir beside it (never
    /// in it), the receipts kept, and the lock.
    #[test]
    fn a_clone_has_one_folder_and_one_rolling_checkout() {
        let state = Path::new("/s");
        let a = CloneDir::of(state, Path::new("/nowhere/widget/.git"));
        let again = CloneDir::of(state, Path::new("/nowhere/widget/.git"));
        let other = CloneDir::of(state, Path::new("/elsewhere/widget/.git"));
        assert_eq!(a, again);
        assert_ne!(a, other);
        assert!(
            a.dir.to_string_lossy().starts_with("/s/integrated/widget-"),
            "{}",
            a.dir.display()
        );
        assert_eq!(a.checkout(), a.dir.join("checkout"));
        assert_eq!(a.cargo_config(), a.dir.join(".cargo/config.toml"));
        assert_eq!(a.target(), a.dir.join("target"));
        assert!(!a.cargo_config().starts_with(a.checkout()));
        let gate = Gate {
            slug: "acme/widget".to_owned(),
            branch: "main".to_owned(),
            contract: PathBuf::from("/h/sykli.json"),
            top: PathBuf::from("/nowhere/widget"),
            common: PathBuf::from("/nowhere/widget/.git"),
        };
        let one = plan(&gate, "c1", "t1", state);
        let two = plan(&gate, "c2", "t2", state);
        assert_eq!(one.checkout, two.checkout, "one checkout, rolled");
        assert_eq!(one.validation.repo, a.checkout().to_string_lossy());
    }

    fn requested(commit: &str, at: i64, run: Option<&str>, not: Option<&str>) -> Requested {
        let mut validation = candidate(commit, "b", at);
        validation.kind = ValidationKind::Integrated;
        validation.run = run.map(str::to_owned);
        validation.not_dispatched = not.map(str::to_owned);
        Requested {
            session: "oh-1-1".to_owned(),
            started_ms: 0,
            ended: true,
            validation,
        }
    }

    /// A run whose tasks have not all ended, or whose process groups live
    /// on, keeps its checkout busy, whatever sykli calls the run.
    #[test]
    fn a_run_that_may_still_be_going_keeps_its_checkout_busy() {
        let gone = |_: u32| Some(false);
        let done = serde_json::json!({"state": "completed", "tasks": [{"name": "t",
            "state": "passed", "attempts": [{"outcome": "passed", "process_group": 4242}]}]});
        assert_eq!(still_going(&done, &gone), None);
        let lost = serde_json::json!({"state": "lost", "tasks": [{"name": "ok",
            "state": "running", "attempts": [{"outcome": null, "process_group": null}]}]});
        assert!(still_going(&lost, &gone)
            .unwrap()
            .contains("its task ok still running"));
        let unfinished = serde_json::json!({"state": "lost", "tasks": [{"name": "ok",
            "state": "lost", "attempts": [{"outcome": null}]}]});
        assert!(still_going(&unfinished, &gone)
            .unwrap()
            .contains("not finished"));
        let ended = serde_json::json!({"state": "lost", "tasks": [{"name": "ok",
            "state": "lost", "attempts": [{"outcome": "lost", "process_group": 4242}]}]});
        assert_eq!(still_going(&ended, &gone), None);
        let alive = |group: u32| Some(group == 4242);
        assert!(still_going(&ended, &alive)
            .unwrap()
            .contains("process group 4242 of its task ok still runs"));
        assert!(still_going(&done, &|_| None)
            .unwrap()
            .contains("cannot be told"));
        let running = serde_json::json!({"state": "running", "tasks": []});
        assert!(still_going(&running, &gone).is_some());
    }

    /// The newest request of a commit decides: handed to sykli, or failed
    /// to be, stands ten minutes; recorded and never confirmed handed over
    /// stands never (under the lock it was never dispatched).
    #[test]
    fn a_request_never_dispatched_is_asked_again_at_once() {
        let now = 100 * 60_000;
        let asked = [requested("c1", now - 60_000, Some("run-1"), None)];
        assert!(matches!(stands(&asked, "c1", now), Stands::Asked(_)));
        assert!(matches!(stands(&asked, "c2", now), Stands::Nothing));
        let old = [requested("c1", now - 11 * 60_000, Some("run-1"), None)];
        assert!(matches!(stands(&old, "c1", now), Stands::Nothing));
        let failed = [requested("c1", now - 60_000, None, Some("no checkout"))];
        assert!(matches!(stands(&failed, "c1", now), Stands::Failed(_)));
        let lost = [
            requested("c1", now - 120_000, Some("run-0"), None),
            requested("c1", now - 60_000, None, None),
        ];
        assert!(matches!(stands(&lost, "c1", now), Stands::Unconfirmed(_)));
        let ancient = [requested("c1", now - 99 * 60_000, None, None)];
        assert!(matches!(
            stands(&ancient, "c1", now),
            Stands::Unconfirmed(_)
        ));
        // Seen without the lock, from a session that ended: never dispatched.
        let said = unconfirmed(&ancient[0]);
        assert!(said.contains(NEVER_DISPATCHED), "{said}");
    }
}
