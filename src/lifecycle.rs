//! Running a session from entering to leaving.
//!
//! `ohjaa run` stays the parent of `toimija run` so that it is still there when
//! the session ends: leaving must always write a record, crash or not. So
//! while it waits it must not die of a signal, and it must never swallow one
//! that was meant to stop the session.
//!
//! The rule, per signal, while the session runs (unix):
//!
//! | signal | ohjaa | why |
//! |---|---|---|
//! | SIGINT | survives, never forwards | Ctrl-C already reaches the whole foreground group; a second copy makes agents like `claude` quit instead of interrupt |
//! | SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2 | survives, forwards the first of each to `toimija run` | a supervisor may aim them at ohjaa alone; when the whole group got them too, the session receives them twice |
//! | SIGALRM, SIGVTALRM, SIGPROF, SIGXCPU, SIGXFSZ | survives | they concern ohjaa's own timers and limits, which it never sets |
//! | SIGKILL, SIGSTOP | cannot be caught | SIGKILL loses the record |
//!
//! To stop a session from outside, send SIGTERM to ohjaa, or SIGINT to the
//! whole process group.
//!
//! The handlers go in just before the session is recorded and spawned, so a
//! signal in that window is kept and passed on once the child exists (SIGINT
//! included, since the group could not have delivered it to the child).
//!
//! Once the session has ended, leaving begins: any of these signals now cuts
//! leaving short (the child tools still running are killed, nothing new is
//! started) and the record is written with what is known so far. The default
//! dispositions come back once the record is written.
//!
//! Handlers, not `SIG_IGN`: an ignored disposition is inherited across exec,
//! and the agent must still stop on Ctrl-C.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;

use crate::process;
use crate::sessions::{Check, Evidence, Exit, Finished, Phase};

#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    /// Every signal ohjaa handles while a session runs, and whether it is
    /// forwarded to the session. See the table in the module docs.
    const HANDLED: [(libc::c_int, bool); 11] = [
        (libc::SIGINT, false),
        (libc::SIGTERM, true),
        (libc::SIGHUP, true),
        (libc::SIGQUIT, true),
        (libc::SIGUSR1, true),
        (libc::SIGUSR2, true),
        (libc::SIGALRM, false),
        (libc::SIGVTALRM, false),
        (libc::SIGPROF, false),
        (libc::SIGXCPU, false),
        (libc::SIGXFSZ, false),
    ];

    /// One bit per signal received (all handled signals are below 32).
    static PENDING: AtomicU32 = AtomicU32::new(0);
    static LEAVING: AtomicBool = AtomicBool::new(false);

    fn bit(signal: libc::c_int) -> u32 {
        if (0..32).contains(&signal) {
            1 << signal
        } else {
            0
        }
    }

    extern "C" fn on_signal(signal: libc::c_int) {
        // Only atomic operations: the one thing that is safe inside a handler.
        PENDING.fetch_or(bit(signal), Ordering::SeqCst);
        if LEAVING.load(Ordering::SeqCst) {
            crate::process::cancel();
        }
    }

    fn set_all(disposition: libc::sighandler_t) {
        for (signal, _) in HANDLED {
            // SAFETY: installing a disposition for a catchable signal; the
            // handler only performs atomic operations and lives for the
            // whole process.
            unsafe {
                libc::signal(signal, disposition);
            }
        }
    }

    pub fn supervise() {
        let handler = on_signal as extern "C" fn(libc::c_int);
        set_all(handler as libc::sighandler_t);
    }

    /// Forward the first of each forwardable signal received so far. With
    /// `before_spawn`, SIGINT too: one that arrived before the child existed
    /// never reached it through the process group.
    pub fn forward_pending(child: u32, forwarded: &mut u32, before_spawn: bool) {
        let pending = PENDING.load(Ordering::SeqCst);
        for (signal, forward) in HANDLED {
            let b = bit(signal);
            let forward = forward || (before_spawn && signal == libc::SIGINT);
            if forward && pending & b != 0 && *forwarded & b == 0 {
                *forwarded |= b;
                // SAFETY: plain kill(2) on the pid of a child not yet reaped.
                unsafe {
                    libc::kill(child as libc::pid_t, signal);
                }
            }
        }
    }

    /// From now on a signal cuts leaving short. What stopped the session
    /// itself (the Ctrl-C that ended it) must not count against leaving.
    pub fn leaving() {
        PENDING.store(0, Ordering::SeqCst);
        LEAVING.store(true, Ordering::SeqCst);
    }

    pub fn restore() {
        LEAVING.store(false, Ordering::SeqCst);
        set_all(libc::SIG_DFL);
    }
}

/// Install the waiting parent's handlers, just before the session is recorded
/// and spawned: a signal from here on is kept, and passed to the session once
/// it exists, instead of killing ohjaa half way through entering. The child
/// starts with default dispositions (caught signals reset across exec).
pub fn supervise() {
    #[cfg(unix)]
    signals::supervise();
}

/// Wait for the session to end, forwarding stop signals aimed at ohjaa.
pub fn wait(child: &mut Child) -> std::io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        let mut forwarded = 0;
        // Whatever arrived while the session was being started.
        signals::forward_pending(child.id(), &mut forwarded, true);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            signals::forward_pending(child.id(), &mut forwarded, false);
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(not(unix))]
    {
        child.wait()
    }
}

/// The session has ended; a signal from here on cuts leaving short.
pub fn leaving() {
    #[cfg(unix)]
    signals::leaving();
}

/// The record is written: signals mean what they normally mean again.
pub fn restore_signals() {
    #[cfg(unix)]
    signals::restore();
}

pub fn exit_of(status: ExitStatus) -> Exit {
    if let Some(code) = status.code() {
        return Exit::Code { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return Exit::Signal { signal };
        }
    }
    Exit::Unknown
}

/// The exit code `ohjaa run` returns for the session's end.
pub fn exit_code(exit: Exit) -> i32 {
    match exit {
        Exit::Code { code } => code,
        Exit::Signal { signal } => 128 + signal,
        Exit::Unknown => 1,
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = process::run(
        Command::new("git").arg("-C").arg(repo).args(args),
        None,
        process::LOCAL,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| output.stdout_text().trim().to_owned())
}

pub fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "--verify", "-q", "HEAD"])
}

/// The Git repository `path` lies in, canonical. A path not created yet (a
/// file the agent will write) is looked up from its nearest existing
/// directory. `None` when that directory is not inside a repository.
pub fn repo_of(path: &Path) -> Option<std::path::PathBuf> {
    let dir = path.ancestors().find(|dir| dir.is_dir())?;
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(root).ok()
}

pub fn branch(repo: &Path) -> Option<String> {
    git(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
}

/// Whether `sykli run` has a contract to run here: it reads `sykli.rs`, or
/// `sykli.json` when only that exists, from the directory it runs in.
pub fn has_sykli_contract(repo: &Path) -> bool {
    ["sykli.rs", "sykli.json"]
        .iter()
        .any(|name| repo.join(name).is_file())
}

/// What a repository's checkouts and branches pointed at when a session
/// entered, read then and kept in memory until leaving (`ohjaa run` is one
/// process): nothing reachable from these was made during the session, and
/// nothing reachable from `remote` reached a remote during it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Start {
    /// Every checkout's HEAD, and every branch.
    pub local: Vec<String>,
    /// Every remote branch.
    pub remote: Vec<String>,
}

/// Read `repo`'s [`Start`] now. Empty where git cannot say.
pub fn start_of(repo: &Path) -> Start {
    let mut start = Start {
        local: worktrees(repo).into_iter().filter_map(|w| w.head).collect(),
        remote: Vec::new(),
    };
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    for line in refs.lines() {
        match line.split_once(' ') {
            Some((commit, name)) if name.starts_with("refs/remotes/") => {
                start.remote.push(commit.to_owned())
            }
            Some((commit, _)) => start.local.push(commit.to_owned()),
            None => {}
        }
    }
    for list in [&mut start.local, &mut start.remote] {
        list.sort_unstable();
        list.dedup();
    }
    start
}

/// One checkout of a repository: its main worktree or a linked one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Canonical.
    pub path: PathBuf,
    pub head: Option<String>,
    /// `refs/heads/<name>`, when a branch is checked out there.
    pub branch: Option<String>,
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every checkout of the repository `repo` lies in, the main one first, as
/// `git worktree list --porcelain` names them. A bare main repository and
/// checkouts whose folder is gone are left out. Only `repo` itself when git
/// cannot list them.
pub fn worktrees(repo: &Path) -> Vec<Worktree> {
    let listed = git(repo, &["worktree", "list", "--porcelain"])
        .map(|porcelain| parse_worktrees(&porcelain))
        .unwrap_or_default();
    if listed.is_empty() {
        return vec![Worktree {
            path: canonical(repo),
            head: head(repo),
            branch: git(repo, &["symbolic-ref", "-q", "HEAD"]),
        }];
    }
    listed
}

fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    porcelain
        .split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut head = None;
            let mut branch = None;
            for line in block.lines() {
                match line.split_once(' ') {
                    Some(("worktree", value)) => path = Some(PathBuf::from(value)),
                    Some(("HEAD", value)) => head = Some(value.to_owned()),
                    Some(("branch", value)) => branch = Some(value.to_owned()),
                    _ if line == "bare" => return None,
                    _ => {}
                }
            }
            let path = path.filter(|path| path.is_dir())?;
            Some(Worktree {
                path: canonical(&path),
                head,
                branch,
            })
        })
        .collect()
}

/// A sykli receipt: what it says, and where it lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// When the run started, in milliseconds.
    pub at: i64,
    pub evidence: Evidence,
    pub path: PathBuf,
    /// The checkout sykli ran in.
    pub checkout: PathBuf,
}

/// Sykli receipts keyed by the tree that ran: every one of each tree, best
/// first (a passing run, then a clean one, then the newest). A newer run of
/// the same tree, under another contract or failing, never hides a pass.
#[derive(Debug, Clone, Default)]
pub struct Receipts(std::collections::HashMap<String, Vec<Receipt>>);

impl Receipts {
    /// The best receipt of `tree`: the receipt's word a phase shows.
    pub fn get(&self, tree: &str) -> Option<&Receipt> {
        self.0.get(tree).and_then(|all| all.first())
    }

    /// Every receipt of `tree`, best first.
    pub fn of(&self, tree: &str) -> &[Receipt] {
        self.0.get(tree).map_or(&[], Vec::as_slice)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every receipt of `other` too.
    pub fn extend(&mut self, other: Receipts) {
        for (tree, all) in other.0 {
            for receipt in all {
                self.add(&tree, receipt);
            }
        }
    }

    fn add(&mut self, tree: &str, receipt: Receipt) {
        let rank = |r: &Receipt| (r.evidence.outcome == "passed", !r.evidence.dirty, r.at);
        let all = self.0.entry(tree.to_owned()).or_default();
        let at = all
            .iter()
            .position(|seen| rank(seen) < rank(&receipt))
            .unwrap_or(all.len());
        all.insert(at, receipt);
    }
}

#[cfg(test)]
impl std::ops::Index<&str> for Receipts {
    type Output = Receipt;

    fn index(&self, tree: &str) -> &Receipt {
        self.get(tree).expect("a receipt of that tree")
    }
}

/// The sykli receipts in these checkouts' `.sykli/receipts` (see
/// [`receipts_from`]).
#[cfg(test)]
pub fn receipts_in<'a>(checkouts: impl IntoIterator<Item = &'a Path>) -> Receipts {
    receipts_from(
        checkouts
            .into_iter()
            .map(|checkout| (checkout.join(".sykli").join("receipts"), checkout)),
    )
}

/// The receipts in every checkout of `repo`'s clone, and those sykli keeps
/// under the git common dir (`<common>/sykli/receipts`: a copy of the receipt
/// that verified each push it accepted), which outlive a worktree removed
/// with its `.sykli/`. Those are run from `repo`.
pub fn receipts_of_clone(repo: &Path, checkouts: &[Worktree]) -> Receipts {
    let kept = git(repo, &["rev-parse", "--git-common-dir"])
        .map(|common| repo.join(common).join("sykli").join("receipts"));
    receipts_from(
        checkouts
            .iter()
            .flat_map(|w| {
                [
                    (w.path.join(".sykli").join("receipts"), w.path.as_path()),
                    (w.path.join(".sykli").join("receipt.json"), w.path.as_path()),
                ]
            })
            .chain(kept.map(|dir| (dir, repo))),
    )
}

/// The receipts of a clone found from its layout alone, without asking git:
/// those sykli keeps under the common dir (`<common>/sykli/receipts`, the
/// receipt that verified each push its hook accepted) and, in every worktree
/// (the main one, and each `<common>/worktrees/*/gitdir` names), the ones
/// `sykli run` writes (`.sykli/receipts/`) and the one `ohjaa gate` saves
/// (`.sykli/receipt.json`).
pub fn receipts_of_layout(common: &Path) -> Receipts {
    let mut checkouts: Vec<PathBuf> = Vec::new();
    if common.file_name().is_some_and(|name| name == ".git") {
        if let Some(main) = common.parent() {
            checkouts.push(canonical(main));
        }
    }
    if let Ok(linked) = std::fs::read_dir(common.join("worktrees")) {
        for entry in linked.flatten() {
            let Ok(gitdir) = std::fs::read_to_string(entry.path().join("gitdir")) else {
                continue;
            };
            let gitdir = entry.path().join(gitdir.trim());
            if let Some(worktree) = gitdir.parent() {
                checkouts.push(canonical(worktree));
            }
        }
    }
    let kept_in = checkouts
        .first()
        .cloned()
        .unwrap_or_else(|| common.to_path_buf());
    let mut dirs: Vec<(PathBuf, PathBuf)> = checkouts
        .iter()
        .flat_map(|checkout| {
            [
                (checkout.join(".sykli").join("receipts"), checkout.clone()),
                (
                    checkout.join(".sykli").join("receipt.json"),
                    checkout.clone(),
                ),
            ]
        })
        .collect();
    dirs.push((common.join("sykli").join("receipts"), kept_in));
    receipts_from(
        dirs.iter()
            .map(|(dir, checkout)| (dir.clone(), checkout.as_path())),
    )
}

/// The sykli receipts in these folders, each with the checkout it is run
/// from, keyed by the tree that ran. Reads `sykli-receipt.v1`:
/// `subject.tree_oid`, `subject.head_tree_oid`, `subject.dirty`, `outcome`,
/// `started_at_ms`. A tree is its content, so a receipt is about that tree in
/// whichever checkout sykli ran. Every receipt of a tree is kept, best first: a
/// passing run, then a clean one, then the newest. Read once per repository at leaving,
/// and shared by its phases and its pull request.
fn receipts_from<'a>(dirs: impl IntoIterator<Item = (PathBuf, &'a Path)>) -> Receipts {
    let mut found = Receipts::default();
    for (dir, checkout) in dirs {
        // A folder of receipts, or one receipt file (`.sykli/receipt.json`).
        let paths: Vec<PathBuf> = if dir.is_file() {
            vec![dir]
        } else {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            entries.flatten().map(|entry| entry.path()).collect()
        };
        for path in paths {
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(receipt) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            else {
                continue;
            };
            if receipt["schema"] != "sykli-receipt.v1" {
                continue;
            }
            // Filed under the tree that ran, never HEAD's: a run with changes
            // beside HEAD ran another tree, and must not shadow HEAD's own run.
            let (Some(tree), Some(outcome)) = (
                receipt["subject"]["tree_oid"].as_str(),
                receipt["outcome"].as_str(),
            ) else {
                continue;
            };
            let at = receipt["started_at_ms"].as_i64().unwrap_or(0);
            let head = receipt["subject"]["head_tree_oid"].as_str();
            // sykli calls a run dirty when the tree that ran is not HEAD's, or
            // something beside it (a modified submodule) is. A receipt is filed
            // under the tree that ran, so a run made before the commit (sykli's
            // pre-commit hook, HEAD still at the parent) is that tree's run all
            // the same: only something beside the tree is dirty here.
            let dirty = match receipt["subject"]["dirty"].as_bool() {
                Some(false) => false,
                Some(true) => head.is_none_or(|head| head == tree),
                None => true,
            };
            let evidence = Evidence {
                outcome: outcome.to_owned(),
                dirty,
            };
            found.add(
                tree,
                Receipt {
                    at,
                    evidence,
                    path,
                    checkout: checkout.to_path_buf(),
                },
            );
        }
    }
    found
}

/// The most phases a record keeps: the newest ones.
const MAX_PHASES: usize = 50;

/// One reflog entry: `reference` moved to `commit`, at `at` (unix seconds).
struct Move {
    reference: String,
    at: i64,
    commit: String,
    subject: String,
}

const REFLOG_FORMAT: &str = "--format=%gD%x1f%H%x1f%gs";

/// Reflog entries, newest first per reference, from `git <args>` run with
/// `--date=unix` and [`REFLOG_FORMAT`].
fn reflog(dir: &Path, args: &[&str]) -> Option<Vec<Move>> {
    let log = git(dir, args)?;
    Some(
        log.lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\u{1f}');
                let (selector, commit, subject) = (fields.next()?, fields.next()?, fields.next()?);
                // `refs/heads/main@{1790380435}`, with --date=unix.
                let (reference, rest) = selector.rsplit_once("@{")?;
                let at = rest.strip_suffix('}')?.parse::<i64>().ok()?;
                Some(Move {
                    reference: reference.to_owned(),
                    at,
                    commit: commit.to_owned(),
                    subject: subject.to_owned(),
                })
            })
            .collect(),
    )
}

/// What leaving observed of one repository's commits, in all its checkouts.
#[derive(Debug, Clone, Default)]
pub struct Work {
    /// This session's own commits. `None` when they could not be observed
    /// (see [`observe`]).
    pub phases: Option<Vec<Phase>>,
    /// Commits made during the session that cannot be shown to be its own:
    /// in a repository it only was found working in, or while another session
    /// shared the place. Observed, never credited.
    pub observed: Vec<Phase>,
    /// The other sessions that shared this session's places while it ran.
    pub shared: Vec<Window>,
    /// Commits made here during the session that no reference or reflog
    /// names any more, each with the session that shared the clone then:
    /// work only once a pull request is found holding the newest of them.
    pub lost: Vec<(Phase, Option<Window>)>,
    /// The branch this session's newest commit on a branch was made on, and
    /// that commit: where its pull request is looked up. With no branch, the
    /// newest of its commits no reference names any more, to search by.
    pub pr_branch: Option<String>,
    pub pr_commit: Option<String>,
    /// Whether any commit was made, or pushed from here, during the session.
    pub made: bool,
    /// The receipts in every checkout read.
    pub receipts: Receipts,
    /// The commits that reached the default branch during the session, oldest
    /// first (see [`observe`]); sykli is asked about each tip by the caller.
    pub landed: Vec<Arrived>,
    /// Every checkout of the repository.
    pub checkouts: Vec<Worktree>,
}

/// Another session's time in a checkout of this clone (`from_ms` to `to_ms`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub id: String,
    pub checkout: PathBuf,
    pub from_ms: i64,
    pub to_ms: i64,
}

impl Window {
    /// Whether the session was there during the second `at` (unix seconds).
    fn live_at(&self, at: i64) -> bool {
        self.from_ms <= at * 1000 + 999 && self.to_ms >= at * 1000
    }
}

/// A commit that reached the default branch, before sykli is asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrived {
    pub commit: String,
    pub tree: String,
    pub subject: String,
    /// Its commit time, in unix seconds.
    pub committed: i64,
    /// `origin/main`, or the local `main`.
    pub onto: String,
    /// The tip of the push (or move) that brought it: what sykli is asked about.
    pub tip: bool,
    /// The branch still sits at it.
    pub at_tip: bool,
    /// It can be shown to be this session's landing.
    pub attributed: bool,
    /// A checkout that still exists records making it (its HEAD's reflog):
    /// where sykli's receipt of it would be.
    pub alive: bool,
}

/// The most landed commits a record keeps: the newest ones.
const MAX_LANDED: usize = 20;

/// Where a commit was made, as its reflog says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    /// In this checkout (its HEAD's reflog).
    Checkout(PathBuf),
    /// On this branch, in a checkout since removed (only the branch's reflog).
    Branch(String),
    /// Known only from a push from here (local branch and checkout gone).
    Push,
}

#[derive(Debug, Clone)]
struct Made {
    place: Place,
    /// When, in unix seconds.
    at: i64,
}

/// State 3, observed: every commit made in `repo` during the session, in any
/// of its checkouts (`git worktree list`), each with whatever sykli saw on
/// its tree; and which of them are this session's own.
///
/// A commit was made during the session when a reflog — a checkout's HEAD, a
/// branch, or a push from here — shows it created since the session started,
/// and it is still reachable from a branch, a remote branch or a checkout.
/// Commits a pull, merge or reset brought in were made elsewhere; one an
/// amend or a rebase replaced is gone. A rebase's replayed commits are made
/// anew, so they count, whenever their originals were written.
///
/// Law 3, observed not asserted: in a repository the session was launched in
/// or named (`named`, the checkout it was launched in or named with
/// `--scope`), a commit made during the session is its own unless another
/// session (`others`) was there at that second: in `named`, one launched in
/// `named`; in any other checkout or branch of the clone, old or new, or a
/// push, any session in the clone. In a repository only found at leaving
/// (`named` is `None`) nothing is its own. Every other commit made during the
/// session is observed, not attributed.
///
/// Also observed: the commits that reached the default branch (`origin/HEAD`,
/// else `main` or `master`) — every commit a push from here took to the
/// remote one, and every commit made here that arrived on the local one. A
/// push is this session's when no other session was in the clone then.
///
/// Reflog times are whole seconds, so a move in the second the session started
/// may be from just before it: what `start` says the repository held at
/// entering is never this session's.
///
/// Phases are `None` when they cannot be observed: git failed in `repo`, or
/// its HEAD moved from `head_at_start` while its reflog recorded nothing.
pub fn observe(
    repo: &Path,
    head_at_start: Option<&str>,
    started_ms: i64,
    start: &Start,
    named: Option<&Path>,
    others: &[Window],
) -> Work {
    let repo = canonical(repo);
    let named = named.map(canonical);
    let since = started_ms.div_euclid(1000);
    let all = worktrees(&repo);
    // Only sessions in a checkout of this clone can share its places.
    let others: Vec<&Window> = others
        .iter()
        .filter(|w| all.iter().any(|c| c.path == w.checkout))
        .collect();
    let mut work = Work {
        receipts: receipts_of_clone(&repo, &all),
        ..Work::default()
    };
    let mut made: HashMap<String, Made> = HashMap::new();
    // The branch each was made on, or the remote branch a push from here named.
    let mut branch_of: HashMap<String, String> = HashMap::new();
    // Each remote branch's pushes since the start, newest first, and where it
    // pointed at the start.
    let mut pushes: HashMap<String, (Vec<Move>, Option<String>)> = HashMap::new();
    // What each reference pointed at when the session started: nothing
    // reachable from those was made during it.
    let mut boundaries: Vec<String> = head_at_start
        .into_iter()
        .map(str::to_owned)
        .chain(start.local.iter().cloned())
        .chain(start.remote.iter().cloned())
        .collect();
    let mut repo_moves = None;
    // Every commit a checkout that still exists records making, at any time:
    // its sykli receipts would be there.
    let mut made_alive: HashSet<String> = HashSet::new();
    for checkout in &all {
        let Some(moves) = reflog(
            &checkout.path,
            &[
                "log",
                "--walk-reflogs",
                "--date=unix",
                REFLOG_FORMAT,
                "HEAD",
            ],
        ) else {
            continue;
        };
        let mut in_window = 0;
        let mut boundary = None;
        for entry in moves {
            if created_here(&entry.subject) {
                made_alive.insert(entry.commit.clone());
            }
            if entry.at < since {
                boundary.get_or_insert(entry.commit);
                continue;
            }
            in_window += 1;
            if created_here(&entry.subject) {
                made.entry(entry.commit).or_insert_with(|| Made {
                    place: Place::Checkout(checkout.path.clone()),
                    at: entry.at,
                });
            }
        }
        boundaries.extend(boundary);
        if checkout.path == repo {
            repo_moves = Some(in_window);
        }
    }
    let default = default_branch(&repo);
    // What every remote branch pointed at when the session started: none of
    // that reached a remote during it.
    let mut on_remotes: Vec<String> = start.remote.clone();
    // Each default-branch reference's moves since the start, newest first,
    // and where it pointed at the start.
    let mut arrivals: Vec<(String, Vec<Move>, Option<String>)> = default
        .iter()
        .map(|reference| (reference.clone(), Vec::new(), None))
        .collect();
    // Branches and remote branches, shared by every checkout: a commit made in
    // a checkout removed since is still on its branch's reflog.
    if let Some(moves) = reflog(
        &repo,
        &["reflog", "show", "--all", "--date=unix", REFLOG_FORMAT],
    ) {
        let mut bounded = HashSet::new();
        for entry in moves {
            if !entry.reference.starts_with("refs/") {
                continue; // checkouts' HEADs, read above
            }
            let arrival = arrivals.iter_mut().find(|(r, _, _)| *r == entry.reference);
            if entry.at < since {
                if bounded.insert(entry.reference.clone()) {
                    if let Some((_, _, start)) = arrival {
                        *start = Some(entry.commit.clone());
                    }
                    if let Some((_, start)) = pushes.get_mut(&entry.reference) {
                        start.get_or_insert(entry.commit.clone());
                    }
                    if entry.reference.starts_with("refs/remotes/") {
                        on_remotes.push(entry.commit.clone());
                    }
                    boundaries.push(entry.commit);
                }
                continue;
            }
            if let Some(name) = entry.reference.strip_prefix("refs/heads/") {
                if created_here(&entry.subject) {
                    let there = all
                        .iter()
                        .find(|w| w.branch.as_deref() == Some(entry.reference.as_str()));
                    made.entry(entry.commit.clone()).or_insert_with(|| Made {
                        place: there.map_or_else(
                            || Place::Branch(name.to_owned()),
                            |w| Place::Checkout(w.path.clone()),
                        ),
                        at: entry.at,
                    });
                    branch_of
                        .entry(entry.commit.clone())
                        .or_insert_with(|| name.to_owned());
                }
            } else if entry.subject == "update by push" {
                pushes
                    .entry(entry.reference.clone())
                    .or_default()
                    .0
                    .push(Move {
                        reference: entry.reference.clone(),
                        at: entry.at,
                        commit: entry.commit.clone(),
                        subject: entry.subject.clone(),
                    });
            }
            if let Some((_, moves, _)) = arrival {
                moves.push(entry);
            }
        }
    }
    // A commit a push from here took to a remote branch during the session was
    // made during it even once its local branch and checkout are gone (a
    // squash-merged, cleaned-up pull request): its commit time says when.
    for (reference, (moves, held)) in &pushes {
        let name = reference
            .trim_start_matches("refs/remotes/")
            .split_once('/')
            .map_or(reference.as_str(), |(_, branch)| branch);
        let olds = moves
            .iter()
            .skip(1)
            .map(|older| Some(older.commit.clone()))
            .chain(std::iter::once(held.clone()));
        for (entry, old) in moves.iter().zip(olds) {
            for pushed in range(&repo, &entry.commit, old.as_deref(), &on_remotes) {
                if pushed.committed < since {
                    continue;
                }
                branch_of
                    .entry(pushed.commit.clone())
                    .or_insert_with(|| name.to_owned());
                made.entry(pushed.commit).or_insert(Made {
                    place: Place::Push,
                    at: entry.at,
                });
            }
        }
    }
    work.made = !made.is_empty() || !pushes.is_empty();

    // Whose each commit is: `Ok` this session's own; `Err(Some(w))` made in
    // its place while `w` shared it; `Err(None)` not in a place of its own.
    // One rule: in a repository it was launched in or named, a commit is its
    // own unless another session was there at that second: in the checkout it
    // was launched in, only a session launched in that checkout; in any other
    // checkout or branch of the clone, old or new, any session in the clone.
    let in_clone = |at: i64| others.iter().copied().find(|w| w.live_at(at));
    let attribute = |made: &Made| -> Result<(), Option<&Window>> {
        let Some(named) = &named else {
            return Err(None);
        };
        let contested = match &made.place {
            Place::Checkout(path) if path == named => others
                .iter()
                .copied()
                .find(|w| w.checkout == *named && w.live_at(made.at)),
            Place::Checkout(_) | Place::Branch(_) | Place::Push => in_clone(made.at),
        };
        contested.map_or(Ok(()), |w| Err(Some(w)))
    };

    let head_now = all
        .iter()
        .find(|w| w.path == repo)
        .and_then(|w| w.head.clone());
    let unkept = head_at_start.is_some() && head_now.as_deref() != head_at_start;
    let made_since = match repo_moves {
        None => None,
        Some(0) if unkept && made.is_empty() => None,
        Some(_) => surviving(&repo, &all, &made, &boundaries),
    };
    let mut shared: Vec<Window> = Vec::new();
    if let Some(made_since) = made_since {
        let mut phases = Vec::new();
        for mut phase in made_since {
            phase.evidence = work
                .receipts
                .get(&phase.tree)
                .map(|receipt| receipt.evidence.clone());
            phase.branch = branch_of.get(&phase.commit).cloned();
            let how = &made[&phase.commit];
            phase.worktree = match &how.place {
                Place::Checkout(path) if *path != repo => Some(path.to_string_lossy().into_owned()),
                _ => None,
            };
            match attribute(how) {
                Ok(()) => phases.push(phase),
                Err(by) => {
                    if let Some(by) = by {
                        if !shared.contains(by) {
                            shared.push(by.clone());
                        }
                    }
                    work.observed.push(phase);
                }
            }
        }
        // Its pull request is the one for the branch its newest commit on a
        // branch was made on: never whatever a checkout holds now.
        if let Some(newest) = phases.iter().rev().find(|p| p.branch.is_some()) {
            work.pr_branch = newest.branch.clone();
            work.pr_commit = Some(newest.commit.clone());
        } else if named.is_some() {
            // No branch names its work. A squash-merged pull request whose
            // branch, worktree and remote-tracking branch (`git fetch --prune`)
            // were all deleted leaves its commits only in the object store.
            // Those made here during the session are candidates, its own by
            // the same rule, for the clone; its pull request is searched for by
            // the newest of its own, and they count only if one holds it (a
            // stash's commits are lost too, and are no one's work).
            for lost in lost_commits(&repo, since) {
                let phase = Phase {
                    evidence: work.receipts.get(&lost.tree).map(|r| r.evidence.clone()),
                    commit: lost.commit,
                    subject: lost.subject,
                    tree: lost.tree,
                    unreachable: true,
                    ..Phase::default()
                };
                let by = in_clone(lost.committed).cloned();
                if by.is_none() {
                    work.pr_commit = Some(phase.commit.clone());
                }
                work.lost.push((phase, by));
            }
        }
        work.phases = Some(phases);
    }
    // A session launched in the same checkout while this one ran shares it,
    // whether or not either made a commit.
    if let Some(named) = &named {
        for w in &others {
            let overlaps = w.from_ms <= intake_now_ms() && w.to_ms >= started_ms;
            if w.checkout == *named && overlaps && !shared.contains(w) {
                shared.push((*w).clone());
            }
        }
    }
    work.shared = shared;

    let mut seen = HashSet::new();
    for (reference, moves, held) in arrivals {
        let remote = reference.starts_with("refs/remotes/");
        let now_at = git(&repo, &["rev-parse", "--verify", "-q", &reference]);
        let onto = reference
            .strip_prefix("refs/remotes/")
            .or_else(|| reference.strip_prefix("refs/heads/"))
            .unwrap_or(&reference)
            .to_owned();
        // Oldest first: each move took the reference from the one before it.
        let olds: Vec<Option<String>> = moves
            .iter()
            .skip(1)
            .map(|older| Some(older.commit.clone()))
            .chain(std::iter::once(held))
            .collect();
        for (entry, old) in moves.iter().zip(olds).rev() {
            if remote && entry.subject != "update by push" {
                continue; // a fetch brings in other people's commits
            }
            // A push is this session's when it was in a place of its own and
            // no other session was in the clone then.
            let pushed_by_us = named.is_some() && in_clone(entry.at).is_none();
            let already = if remote { &on_remotes } else { &start.local };
            for commit in range(&repo, &entry.commit, old.as_deref(), already) {
                let attributed = if remote {
                    pushed_by_us
                } else {
                    match made.get(&commit.commit) {
                        None => continue, // brought in, not made here
                        Some(how) => attribute(how).is_ok(),
                    }
                };
                if seen.insert(commit.commit.clone()) {
                    work.landed.push(Arrived {
                        onto: onto.clone(),
                        tip: commit.commit == entry.commit,
                        at_tip: now_at.as_deref() == Some(commit.commit.as_str()),
                        attributed,
                        alive: made_alive.contains(&commit.commit),
                        ..commit
                    });
                }
            }
        }
    }
    work.landed
        .drain(..work.landed.len().saturating_sub(MAX_LANDED));
    work.checkouts = all;
    work
}

fn intake_now_ms() -> i64 {
    crate::intake::now_seconds() * 1000
}

/// A commit on a repository's default branch, and what sykli says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDefault {
    /// `origin/main`, else the local `main`.
    pub onto: String,
    pub commit: String,
    pub subject: String,
    /// Its commit time, in unix seconds.
    pub committed: i64,
    /// It has more than one parent: a merge (a forge's "Merge pull request").
    pub merge: bool,
    pub check: Check,
}

/// The most default-branch commits a report reads in one repository.
pub const MAX_REPORTED: usize = 100;

/// The commits on `repo`'s default branch (the remote one, else the local;
/// for a repository with an entry, origin's copy of the entry's branch)
/// committed after `since` (unix seconds), first parent only, newest first,
/// at most [`MAX_REPORTED`] (`true` beside them when the window held more),
/// each with what sykli says of it, against the receipts in every checkout
/// and those kept under the git common dir. `None` when git cannot read it.
pub fn default_branch_since(
    repo: &Path,
    since: i64,
    sykli: &Sykli,
) -> Option<(Vec<OnDefault>, bool)> {
    let repo = canonical(repo);
    // An entry names the branch it observes; else the default branch.
    let reference = match sykli.gate_for(&repo) {
        Some(gate) => format!("refs/remotes/origin/{}", gate.branch),
        None => default_branch(&repo).into_iter().next()?,
    };
    let onto = reference
        .strip_prefix("refs/remotes/")
        .or_else(|| reference.strip_prefix("refs/heads/"))
        .unwrap_or(&reference)
        .to_owned();
    let log = git(
        &repo,
        &[
            "log",
            "--first-parent",
            &format!("--since=@{since}"),
            &format!("-{}", MAX_REPORTED + 1),
            "--format=%H%x1f%T%x1f%ct%x1f%P%x1f%s",
            &reference,
        ],
    )?;
    let all = worktrees(&repo);
    let receipts = receipts_of_clone(&repo, &all);
    // What a checkout that still exists records making: its receipts would
    // be there.
    let mut alive = HashSet::new();
    for checkout in &all {
        let moves = reflog(
            &checkout.path,
            &[
                "log",
                "--walk-reflogs",
                "--date=unix",
                REFLOG_FORMAT,
                "HEAD",
            ],
        );
        for entry in moves.into_iter().flatten() {
            if created_here(&entry.subject) {
                alive.insert(entry.commit);
            }
        }
    }
    let mut listed: Vec<(&str, &str, i64, bool, &str)> = log
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(5, '\u{1f}');
            let (commit, tree, committed, parents, subject) = (
                fields.next()?,
                fields.next()?,
                fields.next()?.parse::<i64>().ok()?,
                fields.next()?,
                fields.next()?,
            );
            // `--since` keeps the second it names; the window does not.
            (committed > since).then_some((
                commit,
                tree,
                committed,
                parents.split_whitespace().count() > 1,
                subject,
            ))
        })
        .collect();
    let capped = listed.len() > MAX_REPORTED;
    listed.truncate(MAX_REPORTED);
    let judged = listed
        .into_iter()
        .map(|(commit, tree, committed, merge, subject)| OnDefault {
            onto: onto.clone(),
            commit: commit.to_owned(),
            subject: subject.to_owned(),
            committed,
            merge,
            check: sykli.check(&repo, commit, tree, &receipts, &all, alive.contains(commit)),
        })
        .collect();
    Some((judged, capped))
}

/// A sykli that judges a repository whose origin names an entry in the
/// workflow home by the entry's gate (every other one by its own contract).
pub fn sykli_with_entries() -> Sykli {
    match crate::home::ohjaa_home() {
        Ok(home) => Sykli::with_home(crate::home::absolute(home)),
        Err(_) => Sykli::default(),
    }
}

/// Whether this sykli can bind a receipt to a commit, and why not when it
/// cannot: for a report to say what it could not check.
pub fn sykli_cannot_bind(sykli: &Sykli) -> Option<String> {
    sykli.binds_commits().err()
}

/// The default branch's references: the remote one first, then the local
/// one, each when it exists. The default branch is what `origin/HEAD` names,
/// else `main`, else `master`.
fn default_branch(repo: &Path) -> Vec<String> {
    let exists = |reference: &str| git(repo, &["rev-parse", "--verify", "-q", reference]).is_some();
    let name = git(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        .and_then(|target| {
            target
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_owned)
        })
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|name| {
                    exists(&format!("refs/remotes/origin/{name}"))
                        || exists(&format!("refs/heads/{name}"))
                })
                .map(str::to_owned)
        });
    let Some(name) = name else {
        return Vec::new();
    };
    [
        format!("refs/remotes/origin/{name}"),
        format!("refs/heads/{name}"),
    ]
    .into_iter()
    .filter(|reference| exists(reference))
    .collect()
}

/// The commits reachable from `new` but from neither `old` nor what was
/// `already` on a remote, oldest first; at most the newest [`MAX_LANDED`].
fn range(repo: &Path, new: &str, old: Option<&str>, already: &[String]) -> Vec<Arrived> {
    // Fed on stdin: a repository with many branches has many starting points.
    let mut input = format!("{new}\n");
    for excluded in old.into_iter().chain(already.iter().map(String::as_str)) {
        input.push_str(&format!("^{excluded}\n"));
    }
    let limit = format!("-{MAX_LANDED}");
    let output = process::run(
        Command::new("git").arg("-C").arg(repo).args([
            "log",
            "--stdin",
            "--format=%H%x1f%T%x1f%ct%x1f%s",
            &limit,
        ]),
        Some(input.as_bytes()),
        process::LOCAL,
    );
    let Ok(output) = output.map(|output| (output.status.success(), output.stdout_text())) else {
        return Vec::new();
    };
    let (true, log) = output else {
        return Vec::new();
    };
    let mut found: Vec<Arrived> = log
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\u{1f}');
            Some(Arrived {
                commit: fields.next()?.to_owned(),
                tree: fields.next()?.to_owned(),
                committed: fields.next()?.parse().ok()?,
                subject: fields.next()?.to_owned(),
                onto: String::new(),
                tip: false,
                at_tip: false,
                attributed: false,
                alive: false,
            })
        })
        .collect();
    found.reverse();
    found
}

/// The created commits still reachable from a branch, a remote branch or a
/// checkout's HEAD, oldest first. Walks only what became reachable since the
/// start (everything the `boundaries` reach is older).
fn surviving(
    repo: &Path,
    checkouts: &[Worktree],
    created: &HashMap<String, Made>,
    boundaries: &[String],
) -> Option<Vec<Phase>> {
    if created.is_empty() {
        return Some(Vec::new());
    }
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let tips: Vec<&str> = refs
        .lines()
        .chain(checkouts.iter().filter_map(|w| w.head.as_deref()))
        .collect();
    // A reflog may name an object gc has since removed: `^<missing>` would
    // fail the whole walk, so only boundaries that exist are used.
    let boundaries = existing(repo, boundaries)?;
    let mut input = tips.join("\n");
    for boundary in &boundaries {
        input.push_str(&format!("\n^{boundary}"));
    }
    input.push('\n');
    let output = process::run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["log", "--stdin", "--format=%H%x1f%T%x1f%s"]),
        Some(input.as_bytes()),
        process::LOCAL,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut phases: Vec<Phase> = output
        .stdout_text()
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\u{1f}');
            let (commit, tree, subject) = (fields.next()?, fields.next()?, fields.next()?);
            created.contains_key(commit).then(|| Phase {
                commit: commit.to_owned(),
                subject: subject.to_owned(),
                tree: tree.to_owned(),
                ..Phase::default()
            })
        })
        .collect();
    phases.reverse();
    phases.drain(..phases.len().saturating_sub(MAX_PHASES));
    Some(phases)
}

/// A commit no reference and no reflog names any more.
struct Lost {
    commit: String,
    tree: String,
    /// Its commit time, in unix seconds.
    committed: i64,
    subject: String,
}

/// The most loose objects written during a session read for lost commits:
/// more were written by a fetch or an unpack, not by commits, and none is.
const MAX_LOOSE: usize = 5_000;

/// Commits written in `repo`'s clone since `since` (unix seconds) that no
/// reference and no reflog names any more, oldest first: the newest one and
/// those of its ancestors that are lost too (a sibling an amend or a rebase
/// replaced is not). Git writes a new commit as a loose object, so only loose
/// objects written since then are read, and only commits this clone's
/// committer (`user.email`) committed since then count; one fetched and
/// pruned again was someone else's.
fn lost_commits(repo: &Path, since: i64) -> Vec<Lost> {
    let run = |args: &[&str], input: String| {
        process::run(
            Command::new("git").arg("-C").arg(repo).args(args),
            Some(input.as_bytes()),
            process::LOCAL,
        )
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout_text())
    };
    let read = || -> Option<Vec<Lost>> {
        let common = repo.join(git(repo, &["rev-parse", "--git-common-dir"])?);
        let mut loose = Vec::new();
        for dir in std::fs::read_dir(common.join("objects")).ok()?.flatten() {
            let prefix = dir.file_name().to_string_lossy().into_owned();
            if prefix.len() != 2 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            for file in std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let written = file
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|at| at.as_secs() as i64);
                if written.is_some_and(|at| at >= since) {
                    loose.push(format!("{prefix}{}", file.file_name().to_string_lossy()));
                }
            }
            if loose.len() > MAX_LOOSE {
                return None;
            }
        }
        if loose.is_empty() {
            return Some(Vec::new());
        }
        let kinds = run(
            &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
            loose.join("\n") + "\n",
        )?;
        let commits: Vec<&str> = kinds
            .lines()
            .filter_map(|line| line.strip_suffix(" commit"))
            .collect();
        if commits.is_empty() {
            return Some(Vec::new());
        }
        let me = git(repo, &["config", "user.email"]);
        let log = run(
            &[
                "log",
                "--no-walk=unsorted",
                "--stdin",
                "--format=%H%x1f%T%x1f%ct%x1f%ce%x1f%s",
            ],
            commits.join("\n") + "\n",
        )?;
        let mut lost: Vec<Lost> = log
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(5, '\u{1f}');
                let (commit, tree, committed, email, subject) = (
                    fields.next()?,
                    fields.next()?,
                    fields.next()?.parse::<i64>().ok()?,
                    fields.next()?,
                    fields.next()?,
                );
                (committed >= since && me.as_deref().is_none_or(|me| me == email)).then(|| Lost {
                    commit: commit.to_owned(),
                    tree: tree.to_owned(),
                    committed,
                    subject: subject.to_owned(),
                })
            })
            .collect();
        if lost.is_empty() {
            return Some(Vec::new());
        }
        // Named by no reference and no reflog, in any checkout; parents first.
        let unnamed = |tips: String| {
            run(
                &[
                    "rev-list",
                    "--reverse",
                    "--topo-order",
                    "--not",
                    "--all",
                    "--reflog",
                    "--stdin",
                ],
                tips,
            )
        };
        let gone = unnamed(lost.iter().map(|l| format!("{}\n", l.commit)).collect())?;
        let gone: HashSet<&str> = gone.lines().collect();
        lost.retain(|l| gone.contains(l.commit.as_str()));
        let newest = lost.iter().max_by_key(|l| l.committed)?.commit.clone();
        let line = unnamed(format!("{newest}\n"))?;
        let mut by_commit: HashMap<String, Lost> =
            lost.into_iter().map(|l| (l.commit.clone(), l)).collect();
        let mut lost: Vec<Lost> = line
            .lines()
            .filter_map(|commit| by_commit.remove(commit))
            .collect();
        lost.drain(..lost.len().saturating_sub(MAX_PHASES));
        Some(lost)
    };
    read().unwrap_or_default()
}

/// The commits among `commits` the repository still has.
fn existing(repo: &Path, commits: &[String]) -> Option<Vec<String>> {
    if commits.is_empty() {
        return Some(Vec::new());
    }
    let input = format!("{}\n", commits.join("\n"));
    let output = process::run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
        Some(input.as_bytes()),
        process::LOCAL,
    )
    .ok()?;
    output.status.success().then(|| {
        output
            .stdout_text()
            .lines()
            .filter_map(|line| line.strip_suffix(" commit"))
            .map(str::to_owned)
            .collect()
    })
}

/// Whether a reflog entry records HEAD arriving at a commit this checkout
/// created, rather than moving to one that already existed (a checkout, a
/// reset, a fast-forward, a rebase's start or finish). Judged by git's own
/// wording, `<action>: <message>`. The action carries the command's
/// arguments (`pull -q --rebase (pick)`), so replays match by their suffix.
fn created_here(subject: &str) -> bool {
    const REPLAYED: [&str; 7] = [
        "(pick)",
        "(reword)",
        "(edit)",
        "(squash)",
        "(fixup)",
        "(merge)",
        "(continue)",
    ];
    let (action, message) = subject.split_once(": ").unwrap_or((subject, ""));
    action.starts_with("commit")
        || ["cherry-pick", "revert", "am"].contains(&action)
        || REPLAYED.iter().any(|step| action.ends_with(step))
        || message.starts_with("Merge made by")
}

/// The Git repositories directly in `folder` (each one's main checkout),
/// canonical, whose reflogs may have been written since `started_ms`. Only a
/// filter before git is asked (see [`observe`]): it reads file times in the
/// repository's `logs`, and passes a repository it cannot judge that way.
pub fn repos_touched_in(folder: &Path, started_ms: i64) -> Vec<PathBuf> {
    let since = started_ms.div_euclid(1000);
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join(".git").exists())
        .filter(|path| reflogs_touched_since(path, since))
        .map(|path| canonical(&path))
        .collect();
    found.sort();
    found
}

/// Whether a reflog of the repository whose main checkout is `checkout` may
/// have been written at or after `since` (unix seconds): its own, or one of
/// its linked worktrees'. A `.git` file (a submodule, a worktree) or a
/// reftable store cannot be judged from file times, so it passes.
fn reflogs_touched_since(checkout: &Path, since: i64) -> bool {
    let git_dir = checkout.join(".git");
    if !git_dir.is_dir() || git_dir.join("reftable").exists() {
        return true;
    }
    let mut pending = vec![git_dir.join("logs")];
    if let Ok(linked) = std::fs::read_dir(git_dir.join("worktrees")) {
        pending.extend(linked.flatten().map(|entry| entry.path().join("logs")));
    }
    while let Some(path) = pending.pop() {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(children) = std::fs::read_dir(&path) {
                pending.extend(children.flatten().map(|entry| entry.path()));
            }
            continue;
        }
        let written = meta
            .modified()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|at| at.as_secs() as i64);
        if written.is_some_and(|at| at >= since) {
            return true;
        }
    }
    false
}

/// State 4, observed: the pull request for `branch` (the one this session's
/// newest commit on a branch was made on), else, when gh knows no pull
/// request by that name any more, or no branch names the session's work any
/// more (`branch` is `None`), the one that holds `commit`; and what sykli says
/// about its head. Asked of `gh` in `repo`.
pub fn finished(
    repo: &Path,
    branch: Option<&str>,
    commit: Option<&str>,
    receipts: &Receipts,
    checkouts: &[Worktree],
    sykli: &Sykli,
) -> Finished {
    let gh = |args: &[&str]| {
        process::run(
            Command::new("gh").args(args).current_dir(repo),
            None,
            std::time::Duration::from_secs(20),
        )
    };
    let viewed =
        match branch.map(|branch| gh(&["pr", "view", branch, "--json", "url,state,headRefOid"])) {
            None => None,
            Some(Ok(output)) => Some(output),
            Some(Err(error)) => {
                return Finished::Unknown {
                    why: format!("{error:#}"),
                }
            }
        };
    let pr = if let Some(output) = viewed.as_ref().filter(|o| o.status.success()) {
        serde_json::from_slice::<Value>(&output.stdout).ok()
    } else {
        if let Some(output) = &viewed {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("no pull requests found") {
                return Finished::Unknown {
                    why: if stderr.contains("gh auth login") || stderr.contains("GH_TOKEN") {
                        "gh is not signed in".to_owned()
                    } else {
                        output.failure_detail()
                    },
                };
            }
        }
        // A branch deleted after a squash merge, or no branch left at all: the
        // pull request still holds the commit.
        let Some(commit) = commit else {
            return Finished::NoPullRequest;
        };
        let search = [
            "pr",
            "list",
            "--state",
            "all",
            "--search",
            commit,
            "--json",
            "url,state,headRefOid",
            "--limit",
            "1",
        ];
        match gh(&search) {
            Ok(found) if found.status.success() => {
                match serde_json::from_slice::<Value>(&found.stdout) {
                    Ok(Value::Array(list)) if list.is_empty() => return Finished::NoPullRequest,
                    Ok(Value::Array(mut list)) => Some(list.swap_remove(0)),
                    _ => None,
                }
            }
            Ok(found) => {
                return Finished::Unknown {
                    why: found.failure_detail(),
                }
            }
            Err(error) => {
                return Finished::Unknown {
                    why: format!("{error:#}"),
                }
            }
        }
    };
    let Some(pr) = pr else {
        return Finished::Unknown {
            why: "unreadable gh output".to_owned(),
        };
    };
    let head = pr["headRefOid"].as_str().map(str::to_owned);
    let tree = head
        .as_deref()
        .and_then(|head| git(repo, &["rev-parse", &format!("{head}^{{tree}}")]));
    let head_evidence = tree
        .as_ref()
        .and_then(|tree| receipts.get(tree).map(|r| r.evidence.clone()));
    let head_check = head.as_deref().map(|head| match &tree {
        Some(tree) => sykli.check(repo, head, tree, receipts, checkouts, true),
        None => Check::Unverified {
            why: "its head commit is not in this repository".to_owned(),
        },
    });
    Finished::PullRequest {
        url: pr["url"].as_str().unwrap_or("").to_owned(),
        state: pr["state"].as_str().unwrap_or("unknown").to_owned(),
        head_evidence,
        head,
        head_check,
    }
}

/// Whether `branch` (`origin/main`, or a local `main`) in `repo` still sits at
/// `commit`. False when git cannot say.
pub fn still_at(repo: &Path, branch: &str, commit: &str) -> bool {
    [
        format!("refs/remotes/{branch}"),
        format!("refs/heads/{branch}"),
    ]
    .iter()
    .filter_map(|reference| git(repo, &["rev-parse", "--verify", "-q", reference]))
    .any(|at| at == commit)
}

/// The most receipts of one tree sykli is asked about, best first.
const MAX_ASKED: usize = 5;

/// An entry's gate for one clone: the contract the workflow home keeps for
/// the repository its origin names, the branch it observes, and the clone
/// (a checkout's top level and the common git dir), which sykli is handed
/// instead of asking git for it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// `owner/name`.
    pub slug: String,
    pub branch: String,
    /// Outside the repository: `sykli verify` reads it, and the lock beside
    /// it, from their files.
    pub contract: PathBuf,
    pub top: PathBuf,
    pub common: PathBuf,
}

/// This machine's sykli, asked at most once per leaving whether it can bind
/// a receipt to a commit (`sykli verify --commit`).
///
/// Made [`Sykli::with_home`], a repository whose origin names an entry in
/// that workflow home is judged by the entry's gate, not by a contract in the
/// commit; a commit judged once against a gate is not asked about again.
#[derive(Debug, Default)]
pub struct Sykli {
    /// `Ok(true)`: `--commit` reads everything from the commit (sykli#67 as
    /// merged); `Ok(false)`: it also checks the checkout on disk.
    binds_commits: std::cell::OnceCell<Result<bool, String>>,
    /// `sykli verify --help` names `--repo-root` and `--git-common-dir`.
    takes_facts: std::cell::OnceCell<bool>,
    /// Where entries are looked up; `None` judges every repository by its
    /// own contract.
    home: Option<PathBuf>,
    /// Each repository's gate, by its canonical path.
    gates: std::cell::RefCell<HashMap<PathBuf, Option<Gate>>>,
    /// What a gate said of a commit: (contract, commit) to the verdict.
    judged: std::cell::RefCell<HashMap<(PathBuf, String), Check>>,
    /// How many times `sykli verify` ran.
    verified: std::cell::Cell<usize>,
    /// How many times it may run in all; `None`: as often as it is needed.
    allowance: std::cell::Cell<Option<usize>>,
    /// The sykli to run, when not the one on PATH: the one `ohjaa apply`
    /// recorded in a dispatcher.
    program: Option<PathBuf>,
    /// How long one sykli call may take; `None`: [`process::LOCAL`].
    limit: Option<std::time::Duration>,
}

impl Sykli {
    /// A sykli that judges a repository with an entry in `home` by its gate.
    pub fn with_home(home: PathBuf) -> Sykli {
        Sykli {
            home: Some(home),
            ..Sykli::default()
        }
    }

    /// The same, running `program` for sykli, and each call for at most
    /// `limit`: a git hook's, which a person waits on.
    pub fn with_program(home: PathBuf, program: PathBuf, limit: std::time::Duration) -> Sykli {
        Sykli {
            home: Some(home),
            program: Some(program),
            limit: Some(limit),
            ..Sykli::default()
        }
    }

    fn command(&self) -> Command {
        Command::new(self.program.as_deref().unwrap_or(Path::new("sykli")))
    }

    fn timeout(&self) -> std::time::Duration {
        self.limit.unwrap_or(process::LOCAL)
    }

    /// Let `sykli verify` run at most `times` more times.
    pub fn allow_verifies(&self, times: usize) {
        self.allowance.set(Some(self.verified.get() + times));
    }

    /// Whether `sykli verify` may still run.
    pub fn may_verify(&self) -> bool {
        self.allowance
            .get()
            .is_none_or(|allowed| self.verified.get() < allowed)
    }

    /// The gate an entry gives `repo`: one git process (its config) the
    /// first time it is asked, none when the home has no entries.
    pub fn gate_for(&self, repo: &Path) -> Option<Gate> {
        let home = self.home.as_deref()?;
        let key = canonical(repo);
        if let Some(known) = self.gates.borrow().get(&key) {
            return known.clone();
        }
        let gate = crate::mark::gate_here(home, &key);
        self.gates.borrow_mut().insert(key, gate.clone());
        gate
    }

    /// Ask sykli once whether it can bind a receipt to a commit, and
    /// whether it takes the repository's facts.
    fn binds_commits(&self) -> Result<bool, String> {
        self.binds_commits
            .get_or_init(|| {
                let output = process::run(
                    self.command().args(["verify", "--help"]),
                    None,
                    self.timeout(),
                )
                .map_err(|error| format!("sykli could not be asked: {error:#}"))?;
                let help = format!(
                    "{}{}",
                    output.stdout_text(),
                    String::from_utf8_lossy(&output.stderr)
                );
                let _ = self
                    .takes_facts
                    .set(help.contains("--repo-root") && help.contains("--git-common-dir"));
                if help.contains("--commit") {
                    Ok(help.contains("nothing is read from the"))
                } else {
                    Err("sykli has no --commit".to_owned())
                }
            })
            .clone()
    }

    /// What the entry's gate says of `commit` (whose tree is `tree`):
    /// `sykli verify <receipt> --contract <gate> --commit <commit>`, handed
    /// the clone's facts when sykli takes them. Only receipts that say they
    /// passed are asked about, best first, until one verifies: a receipt
    /// that says the run failed is not a pass, and needs no sykli to say so.
    /// With no receipt of the tree it was never run here (`alive`: and the
    /// checkout it was made in still exists).
    pub fn check_gated(
        &self,
        gate: &Gate,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        alive: bool,
    ) -> Check {
        let key = (gate.contract.clone(), commit.to_owned());
        if let Some(known) = self.judged.borrow().get(&key) {
            return known.clone();
        }
        let check = self.ask_gate(gate, commit, tree, receipts, alive);
        // Only what sykli said is kept: what it could not say may be said
        // when it is asked again, and with no receipt it was never asked.
        if !matches!(
            check,
            Check::Unverified { .. } | Check::Unchecked | Check::UncheckedGone
        ) {
            self.judged.borrow_mut().insert(key, check.clone());
        }
        check
    }

    fn ask_gate(
        &self,
        gate: &Gate,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        alive: bool,
    ) -> Check {
        let candidates = receipts.of(tree);
        if candidates.is_empty() {
            return if alive {
                Check::Unchecked
            } else {
                Check::UncheckedGone
            };
        }
        let passing: Vec<&Receipt> = candidates
            .iter()
            .filter(|receipt| matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"))
            .collect();
        if passing.is_empty() {
            let newest = &candidates[0];
            return Check::Failed {
                why: format!(
                    "its receipt says the run {} ({})",
                    newest.evidence.outcome,
                    newest.path.display()
                ),
            };
        }
        match self.binds_commits() {
            Ok(true) => {}
            Ok(false) => {
                return Check::Unverified {
                    why: "this sykli's verify --commit reads the checkout too; update sykli"
                        .to_owned(),
                }
            }
            Err(why) => return Check::Unverified { why },
        }
        let facts: Vec<&std::ffi::OsStr> = if self.takes_facts.get().copied().unwrap_or(false) {
            vec![
                "--repo-root".as_ref(),
                gate.top.as_os_str(),
                "--git-common-dir".as_ref(),
                gate.common.as_os_str(),
            ]
        } else {
            Vec::new()
        };
        let mut verdicts = Vec::new();
        for receipt in passing.iter().take(MAX_ASKED) {
            if !self.may_verify() {
                if verdicts.is_empty() {
                    verdicts.push((
                        receipt.at,
                        Check::Unverified {
                            why: "sykli was not asked again".to_owned(),
                        },
                    ));
                }
                break;
            }
            self.verified.set(self.verified.get() + 1);
            let output = process::run(
                self.command()
                    .arg("verify")
                    .arg(&receipt.path)
                    .arg("--contract")
                    .arg(&gate.contract)
                    .args(&facts)
                    .args(["--commit", commit])
                    .current_dir(&gate.top),
                None,
                self.timeout(),
            );
            match output {
                Err(error) => verdicts.push((
                    receipt.at,
                    Check::Unverified {
                        why: format!("sykli verify could not run: {error:#}"),
                    },
                )),
                Ok(output) if output.status.success() => return Check::Passed,
                Ok(output) => verdicts.push((
                    receipt.at,
                    verdict(true, true, output.status.code(), output.failure_detail()),
                )),
            }
        }
        // The runs that failed are their receipts' word, unasked.
        for receipt in candidates
            .iter()
            .filter(|receipt| !matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"))
        {
            verdicts.push((
                receipt.at,
                Check::Failed {
                    why: format!(
                        "its receipt says the run {} ({})",
                        receipt.evidence.outcome,
                        receipt.path.display()
                    ),
                },
            ));
        }
        let mut check = settle(verdicts);
        if passing.len() > MAX_ASKED {
            if let Check::Failed { why }
            | Check::Unverified { why }
            | Check::OtherContract { why } = &mut check
            {
                why.push_str(&format!(
                    " (sykli was asked about the best {MAX_ASKED} of {} receipts of its tree)",
                    passing.len()
                ));
            }
        }
        check
    }

    /// What sykli says about `commit` (whose tree is `tree`) in `repo`:
    /// `sykli verify <receipt> --contract <contract> --commit <commit>`. It
    /// runs in a checkout at the commit with nothing beside it when there is
    /// one (`checkouts`), else in the checkout the receipt is in.
    ///
    /// Exit 0 passes it. A refusal of the commit's own run (another tree, a
    /// dirty run, a failed outcome, contradictory records) fails it, in
    /// sykli's words. A refusal on the tree, inputs or contract on disk fails
    /// it only where the checkout is the commit; elsewhere it is stale: a
    /// sykli that checks the checkout, not the commit. A receipt's own
    /// `outcome` is never read as a pass, a sykli that cannot bind a commit
    /// never passes anything, and a commit with no contract cannot be checked.
    /// With no receipt, `alive` says whether the checkout it was made in, and
    /// so its receipts, still exists: when not, it cannot be observed.
    ///
    /// A repository whose origin names an entry (made [`Sykli::with_home`])
    /// is judged by the entry's gate instead: [`Sykli::check_gated`].
    pub fn check(
        &self,
        repo: &Path,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        checkouts: &[Worktree],
        alive: bool,
    ) -> Check {
        if let Some(gate) = self.gate_for(repo) {
            return self.check_gated(&gate, commit, tree, receipts, alive);
        }
        let candidates = receipts.of(tree);
        if candidates.is_empty() {
            let has_contract = ["sykli.json", "sykli.rs"]
                .iter()
                .any(|name| git(repo, &["cat-file", "-e", &format!("{commit}:{name}")]).is_some());
            return if has_contract && alive {
                Check::Unchecked
            } else if has_contract {
                Check::UncheckedGone
            } else {
                Check::Unverified {
                    why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned(),
                }
            };
        }
        let from_the_commit = match self.binds_commits() {
            Ok(from_the_commit) => from_the_commit,
            Err(why) => return Check::Unverified { why },
        };
        let at_commit = checkouts
            .iter()
            .filter(|w| w.head.as_deref() == Some(commit))
            .map(|w| w.path.as_path())
            .find(|path| clean(path));
        // Every receipt of the tree, best first, until one passes: a newer run
        // of it under another contract, or a failing one, never hides a pass.
        let mut verdicts = Vec::new();
        for receipt in candidates.iter().take(MAX_ASKED) {
            let (checkout, is_the_commit) = match at_commit {
                Some(path) => (path, true),
                None => (receipt.checkout.as_path(), false),
            };
            // A sykli that reads everything from the commit takes the contract
            // from it too; an older one from the checkout on disk.
            let Some(contract) = ["sykli.json", "sykli.rs"].into_iter().find(|name| {
                if from_the_commit {
                    git(repo, &["cat-file", "-e", &format!("{commit}:{name}")]).is_some()
                } else {
                    checkout.join(name).is_file()
                }
            }) else {
                verdicts.push((
                    receipt.at,
                    Check::Unverified {
                        why: format!(
                            "no sykli contract (sykli.json or sykli.rs) in {}",
                            checkout.display()
                        ),
                    },
                ));
                continue;
            };
            self.verified.set(self.verified.get() + 1);
            let output = process::run(
                self.command()
                    .arg("verify")
                    .arg(&receipt.path)
                    .args(["--contract", contract, "--commit", commit])
                    .current_dir(checkout),
                None,
                self.timeout(),
            );
            verdicts.push((
                receipt.at,
                match output {
                    Err(error) => Check::Unverified {
                        why: format!("sykli verify could not run: {error:#}"),
                    },
                    Ok(output) if output.status.success() => return Check::Passed,
                    Ok(output) => verdict(
                        from_the_commit,
                        is_the_commit,
                        output.status.code(),
                        output.failure_detail(),
                    ),
                },
            ));
        }
        let mut check = settle(verdicts);
        // Said when the cap left receipts unasked: one of them might pass.
        if candidates.len() > MAX_ASKED {
            if let Check::Failed { why }
            | Check::Unverified { why }
            | Check::Stale { why }
            | Check::OtherContract { why } = &mut check
            {
                why.push_str(&format!(
                    " (sykli was asked about the best {MAX_ASKED} of {} receipts of its tree)",
                    candidates.len()
                ));
            }
        }
        check
    }
}

/// What the receipts of one tree say when none of them proved it, from
/// (when the run started, what was said of it): what sykli could not say of
/// one, since that one might have passed; else the newest run's word, a run
/// that failed or one that passed under another contract.
fn settle(mut verdicts: Vec<(i64, Check)>) -> Check {
    let unsaid = |check: &Check| matches!(check, Check::Unverified { .. } | Check::Stale { .. });
    if let Some(at) = verdicts.iter().position(|(_, check)| unsaid(check)) {
        return verdicts.swap_remove(at).1;
    }
    verdicts
        .into_iter()
        .max_by_key(|(at, _)| *at)
        .map_or(Check::Unchecked, |(_, check)| check)
}

/// What a non-zero `sykli verify --commit` means. A sykli that reads
/// everything from the commit (`from_the_commit`) refuses only the commit
/// itself (exit 1 or 3, the first failing check on stderr), and exit 4 says
/// the receipt passed under another contract or lock than the one it is
/// judged by now: not proven, never failed. Exit 2 is that it could not
/// verify (a bad revision, arguments, not a receipt), and any other end (a
/// crash, a kill) says nothing about the commit either. One that
/// still compares the checkout on disk refuses the commit on its own run
/// (`commit`, `clean`, `submodules`, `outcome`, `records`), but on the tree,
/// inputs or contract on disk only where the checkout is the commit
/// (`is_the_commit`): elsewhere that is stale, not the commit's failure.
fn verdict(from_the_commit: bool, is_the_commit: bool, exit: Option<i32>, why: String) -> Check {
    if exit == Some(2) {
        return Check::Unverified {
            why: format!("sykli verify could not verify: {why}"),
        };
    }
    if from_the_commit {
        return match exit {
            Some(1 | 3) => Check::Failed { why },
            Some(4) => Check::OtherContract { why },
            // A crash, a kill or an exit sykli does not document says nothing
            // about the commit.
            _ => Check::Unverified {
                why: format!("sykli verify could not say: {why}"),
            },
        };
    }
    // sykli names the first check that failed: `refused: <check> expected …`.
    let refused = why
        .strip_prefix("refused: ")
        .and_then(|rest| rest.split_whitespace().next());
    match refused {
        Some("commit" | "clean" | "submodules" | "outcome" | "records") => Check::Failed { why },
        Some("tree" | "inputs" | "contract") if is_the_commit => Check::Failed { why },
        Some("tree" | "inputs" | "contract") => Check::Stale { why },
        _ => Check::Unverified {
            why: format!("sykli verify could not say: {why}"),
        },
    }
}

/// Whether `checkout` has nothing beside its HEAD, sykli's own `.sykli/` aside.
fn clean(checkout: &Path) -> bool {
    git(
        checkout,
        &["status", "--porcelain", "--", ".", ":(exclude).sykli"],
    )
    .is_some_and(|status| status.is_empty())
}

/// The toimija session the child ran: toimija names it `<secs>-<pid>` after
/// its own process, which is ohjaa's child. Asked of toimija, never read from
/// its files; with `outside`, among the sessions it keeps outside every repo.
pub fn toimija_session(
    repo: &Path,
    outside: bool,
    child_pid: u32,
    started_ms: i64,
) -> Option<String> {
    let mut command = Command::new("toimija");
    command.args(["sessions", "--json"]).current_dir(repo);
    if outside {
        command.arg("--outside-repos");
    }
    let output = process::run(&mut command, None, process::LOCAL).ok()?;
    let payload: Value = serde_json::from_slice(&output.stdout).ok()?;
    pick_toimija_session(&payload, child_pid, started_ms)
}

/// Pids are reused, so the pid alone is not enough: the session must also
/// have started no earlier than ohjaa did (a second of slack for the clock
/// tick between the two), and the closest such session wins.
pub fn pick_toimija_session(payload: &Value, child_pid: u32, started_ms: i64) -> Option<String> {
    let earliest = started_ms.div_euclid(1000) - 1;
    payload["sessions"]
        .as_array()?
        .iter()
        .filter_map(|session| session["id"].as_str())
        .filter_map(|id| {
            let (secs, pid) = id.split_once('-')?;
            let secs: i64 = secs.parse().ok()?;
            (pid.parse::<u32>().ok()? == child_pid && secs >= earliest).then_some((secs, id))
        })
        .min_by_key(|(secs, _)| *secs)
        .map(|(_, id)| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The receipts of a clone, from its files alone: those sykli keeps
    /// under the common dir, and in each worktree (git names the linked ones
    /// in `<common>/worktrees/*/gitdir`, absolute or relative) those `sykli
    /// run` writes and the one `ohjaa gate` saves.
    #[test]
    fn receipts_are_found_from_the_layout_alone() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-r-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: PathBuf, tree: &str, outcome: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    r#"{{"schema":"sykli-receipt.v1","outcome":"{outcome}","started_at_ms":1,"subject":{{"tree_oid":"{tree}","dirty":false}}}}"#
                ),
            )
            .unwrap();
        };
        let (main, near, far) = (root.join("main"), root.join("near"), root.join("far"));
        let common = main.join(".git");
        write(common.join("sykli/receipts/rcpt_a.json"), "kept", "passed");
        write(main.join(".sykli/receipts/rcpt_b.json"), "ran", "failed");
        write(near.join(".sykli/receipt.json"), "gate", "passed");
        write(far.join(".sykli/receipts/rcpt_c.json"), "far", "passed");
        for (id, gitdir) in [
            ("near", "../../../../near/.git".to_owned()),
            ("far", format!("{}\n", far.join(".git").display())),
        ] {
            let dir = common.join("worktrees").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("gitdir"), gitdir).unwrap();
        }
        let found = receipts_of_layout(&common);
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        assert_eq!(
            found["kept"].checkout,
            canonical(&main),
            "run from the main checkout"
        );
        assert_eq!(found["ran"].evidence.outcome, "failed");
        assert_eq!(found["gate"].checkout, canonical(&near));
        assert_eq!(
            found["gate"].path,
            canonical(&near).join(".sykli/receipt.json")
        );
        assert_eq!(found["far"].checkout, canonical(&far));
        // A bare clone has no main checkout; what sykli keeps is still read.
        let bare = root.join("bare.git");
        write(bare.join("sykli/receipts/rcpt_d.json"), "bare", "passed");
        assert_eq!(receipts_of_layout(&bare)["bare"].checkout, bare);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ohjaa-life-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "T"],
            vec!["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(&dir)
                .status()
                .unwrap()
                .success());
        }
        dir
    }

    fn commit(dir: &Path, file: &str, message: &str) -> String {
        std::fs::write(dir.join(file), message).unwrap();
        assert!(Command::new("git")
            .args(["add", file])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        git(dir, &["rev-parse", "HEAD^{tree}"]).unwrap()
    }

    fn receipt(dir: &Path, name: &str, tree: &str, outcome: &str, at: i64, dirty: bool) {
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join(format!("{name}.json")),
            format!(
                r#"{{"schema":"sykli-receipt.v1","contract_hash":"x","started_at_ms":{at},
                   "subject":{{"repository":"/r","tree_oid":"{tree}","inputs_digest":"d",
                   "head_tree_oid":"{tree}","dirty":{dirty}}},"tasks":[],"outcome":"{outcome}"}}"#
            ),
        )
        .unwrap();
    }

    /// A receipt for a run of `ran` (the tree that ran) with HEAD at `head`.
    fn receipt_ran(
        dir: &Path,
        name: &str,
        ran: &str,
        head: &str,
        outcome: &str,
        at: i64,
        dirty: bool,
    ) {
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join(format!("{name}.json")),
            format!(
                r#"{{"schema":"sykli-receipt.v1","contract_hash":"x","started_at_ms":{at},
                   "subject":{{"repository":"/r","tree_oid":"{ran}","inputs_digest":"d",
                   "head_tree_oid":"{head}","dirty":{dirty}}},"tasks":[],"outcome":"{outcome}"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_receipt_is_for_the_tree_that_ran_and_a_clean_pass_outranks_a_newer_dirty_run() {
        let dir = repo("receipt-choice");
        let tree = commit(&dir, "a.txt", "checked");
        receipt_ran(&dir, "clean", &tree, &tree, "passed", 1, false);
        // An experiment run on top of HEAD, then reverted: it ran another tree.
        receipt_ran(&dir, "experiment", "e0e0", &tree, "failed", 2, true);
        // A newer run of the same tree, with something uncommitted beside it.
        receipt_ran(&dir, "dirty", &tree, &tree, "passed", 3, true);
        let receipts = receipts_in([dir.as_path()]);
        let chosen = &receipts[&tree];
        assert!(chosen.path.ends_with("clean.json"), "{:?}", chosen.path);
        assert!(!chosen.evidence.dirty);
        assert_eq!(
            receipts["e0e0"].evidence.outcome, "failed",
            "filed under what ran"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pass_made_before_the_commit_outranks_a_newer_failing_run_of_its_tree() {
        let dir = repo("receipt-precommit");
        let parent = commit(&dir, "a.txt", "parent");
        let tree = commit(&dir, "a.txt", "the phase");
        // sykli's pre-commit hook runs the tree being committed while HEAD is
        // still the parent: sykli calls that dirty, yet it ran exactly this tree.
        receipt_ran(&dir, "pre-commit", &tree, &parent, "passed", 1, true);
        // A newer run of the same tree failed (a flaky task).
        receipt_ran(&dir, "flake", &tree, &tree, "failed", 2, false);
        // A newer pass with something beside the tree (a modified submodule).
        receipt_ran(&dir, "beside", &tree, &tree, "passed", 3, true);
        let receipts = receipts_in([dir.as_path()]);
        let chosen = &receipts[&tree];
        assert!(
            chosen.path.ends_with("pre-commit.json"),
            "{:?}",
            chosen.path
        );
        assert_eq!(chosen.evidence.outcome, "passed");
        assert!(
            !chosen.evidence.dirty,
            "a run of exactly the commit's tree, made before the commit, is not dirty"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    fn now_ms() -> i64 {
        crate::intake::now_seconds() * 1000
    }

    fn subjects(found: &[Phase]) -> Vec<&str> {
        found.iter().map(|p| p.subject.as_str()).collect()
    }

    /// The phases leaving observes in `dir`, skipping no checkout.
    fn phases(dir: &Path, start: Option<&str>, started_ms: i64) -> Option<Vec<Phase>> {
        observe(dir, start, started_ms, &Start::default(), Some(dir), &[]).phases
    }

    #[test]
    fn phases_are_this_sessions_commits_with_what_sykli_saw() {
        let dir = repo("phases");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        let first = commit(&dir, "a.txt", "first phase");
        let second = commit(&dir, "b.txt", "second phase");
        receipt(&dir, "old", &first, "failed", 1, false);
        receipt(&dir, "new", &first, "passed", 2, false);
        receipt(&dir, "dirty", &second, "passed", 3, true);
        let found = phases(&dir, Some(&start), started_ms).unwrap();
        assert_eq!(
            found.len(),
            2,
            "the commit before the session is not a phase"
        );
        assert_eq!(found[0].subject, "first phase");
        assert_eq!(
            found[0].evidence.as_ref().unwrap().outcome,
            "passed",
            "the newest receipt wins"
        );
        assert!(found[1].evidence.as_ref().unwrap().dirty);

        commit(&dir, "c.txt", "third phase");
        assert!(
            phases(&dir, Some(&start), started_ms).unwrap()[2]
                .evidence
                .is_none(),
            "no run is said, not guessed"
        );
        let without_start = phases(&dir, None, started_ms).expect("still observed");
        assert!(
            subjects(&without_start).ends_with(&["first phase", "second phase", "third phase"]),
            "without a starting commit, the reflogs still say what was made: {:?}",
            subjects(&without_start)
        );
        assert_eq!(
            phases(&dir, Some(&head(&dir).unwrap()), started_ms),
            Some(Vec::new()),
            "no commits since the start is an observation"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn commits_a_pull_brought_in_are_not_this_sessions_phases() {
        let upstream = repo("pull-upstream");
        let dir = std::env::temp_dir().join(format!("ohjaa-life-pull-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let status = Command::new("git")
            .args(["clone", "-q"])
            .arg(&upstream)
            .arg(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        git_ok(&dir, &["config", "user.email", "t@example.invalid"]);
        git_ok(&dir, &["config", "user.name", "T"]);
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        let seen = |dir: &Path| phases(dir, Some(&start), started_ms).unwrap();

        commit(&upstream, "u1.txt", "theirs 1");
        commit(&upstream, "u2.txt", "theirs 2");
        git_ok(&dir, &["pull", "-q", "--ff-only"]);
        assert!(
            seen(&dir).is_empty(),
            "a fast-forward brings in commits made elsewhere"
        );

        commit(&dir, "m.txt", "mine");
        assert_eq!(subjects(&seen(&dir)), ["mine"]);

        commit(&upstream, "u3.txt", "theirs 3");
        git_ok(&dir, &["pull", "-q", "--rebase"]);
        assert_eq!(
            subjects(&seen(&dir)),
            ["mine"],
            "a rebased commit is still this session's; what it was rebased onto is not"
        );

        commit(&upstream, "u4.txt", "theirs 4");
        git_ok(&dir, &["pull", "-q", "--no-rebase", "--no-edit"]);
        let found = seen(&dir);
        assert_eq!(found.len(), 2, "{:?}", subjects(&found));
        assert_eq!(found[0].subject, "mine");
        assert!(
            found[1].subject.starts_with("Merge"),
            "a merge made here is this session's commit; the merged one is not"
        );
        let _ = std::fs::remove_dir_all(upstream);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_commit_made_before_the_session_is_not_a_phase() {
        let dir = repo("before");
        git_ok(&dir, &["switch", "-q", "-c", "side"]);
        std::fs::write(dir.join("s.txt"), "s").unwrap();
        git_ok(&dir, &["add", "s.txt"]);
        let status = Command::new("git")
            .args(["commit", "-q", "-m", "made before the session"])
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        git_ok(&dir, &["switch", "-q", "-"]);
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        git_ok(&dir, &["merge", "-q", "--ff-only", "side"]);
        assert_eq!(
            phases(&dir, Some(&start), started_ms),
            Some(Vec::new()),
            "created here, but before the session: its reflog entry is older than the start"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_a_reflog_phases_are_not_observed() {
        let dir = repo("no-reflog");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        std::fs::remove_dir_all(dir.join(".git/logs")).unwrap();
        git_ok(
            &dir,
            &[
                "-c",
                "core.logAllRefUpdates=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "unlogged",
            ],
        );
        assert_eq!(
            phases(&dir, Some(&start), started_ms),
            None,
            "HEAD moved and the reflog saw nothing: not observed, never \"no commits\""
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn worktree_path(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ohjaa-checkout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn a_commit_made_in_a_linked_worktree_is_a_phase_with_the_receipt_made_there() {
        let dir = repo("wt-linked");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        let wt = worktree_path("linked");
        git_ok(
            &dir,
            &["worktree", "add", "-q", "-b", "feat2", wt.to_str().unwrap()],
        );
        let tree = commit(&wt, "w.txt", "made in the worktree");
        receipt(&wt, "w", &tree, "passed", 5, false);
        let work = observe(
            &dir,
            Some(&start),
            started_ms,
            &Start::default(),
            Some(&dir),
            &[],
        );
        let found = work.phases.unwrap();
        assert_eq!(subjects(&found), ["made in the worktree"]);
        assert_eq!(found[0].evidence.as_ref().unwrap().outcome, "passed");
        assert_eq!(found[0].tree, tree);
        let wt = std::fs::canonicalize(&wt).unwrap();
        assert_eq!(found[0].worktree.as_deref(), Some(wt.to_str().unwrap()));
        assert_eq!(
            work.pr_branch.as_deref(),
            Some("feat2"),
            "its pull request is looked up for the branch it was made on"
        );
        assert!(work.made);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_commit_in_a_worktree_removed_since_is_still_on_its_branch() {
        let dir = repo("wt-removed");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        let wt = worktree_path("removed");
        git_ok(
            &dir,
            &["worktree", "add", "-q", "-b", "gone", wt.to_str().unwrap()],
        );
        commit(&wt, "g.txt", "made, then the worktree went");
        git_ok(&dir, &["worktree", "remove", wt.to_str().unwrap()]);
        let found = phases(&dir, Some(&start), started_ms).unwrap();
        assert_eq!(subjects(&found), ["made, then the worktree went"]);
        assert_eq!(found[0].worktree, None, "where is no longer known");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_detached_worktree_commit_and_an_amended_one_are_seen_once_each() {
        let dir = repo("wt-detached");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        let wt = worktree_path("detached");
        git_ok(
            &dir,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        commit(&wt, "d.txt", "detached work");
        commit(&dir, "m.txt", "first try");
        git_ok(&dir, &["commit", "-q", "--amend", "-m", "amended"]);
        let found = phases(&dir, Some(&start), started_ms).unwrap();
        let mut seen = subjects(&found);
        seen.sort_unstable();
        assert_eq!(
            seen,
            ["amended", "detached work"],
            "the commit an amend replaced is no longer anyone's work"
        );
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_was_made_where_another_session_was_is_observed_not_attributed() {
        let dir = repo("shared");
        let start = head(&dir).unwrap();
        let started_ms = now_ms() - 1000;
        let theirs = worktree_path("theirs");
        git_ok(
            &dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "theirs",
                theirs.to_str().unwrap(),
            ],
        );
        commit(&theirs, "t.txt", "in a worktree made during the session");
        commit(&dir, "o.txt", "in the launch checkout");
        let theirs = std::fs::canonicalize(&theirs).unwrap();
        let own = std::fs::canonicalize(&dir).unwrap();
        let watch = |others: &[Window]| {
            let work = observe(
                &dir,
                Some(&start),
                started_ms,
                &Start::default(),
                Some(&dir),
                others,
            );
            let mut mine = subjects(&work.phases.unwrap())
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            mine.sort();
            (mine, work.observed.len(), work.shared.len())
        };
        // Alone in the clone: both are this session's.
        assert_eq!(
            watch(&[]),
            (
                vec![
                    "in a worktree made during the session".to_owned(),
                    "in the launch checkout".to_owned()
                ],
                0,
                0
            )
        );
        let live = |checkout: &Path| Window {
            id: "oh-other".to_owned(),
            checkout: checkout.to_path_buf(),
            from_ms: started_ms,
            to_ms: now_ms() + 60_000,
        };
        // Another session in the clone while the worktree was made: it could
        // have made it; and one in the launch checkout shares that too.
        assert_eq!(watch(&[live(&theirs)]).0, ["in the launch checkout"]);
        assert_eq!(watch(&[live(&own)]), (Vec::new(), 2, 1));
        // One that had left before anything was made shares nothing.
        let left = Window {
            to_ms: started_ms - 5_000,
            from_ms: started_ms - 10_000,
            ..live(&own)
        };
        assert_eq!(watch(&[left]).0.len(), 2);
        // One that left after this session started but before anything was
        // made (the session it took the checkout over from, or an orphan
        // closed at this launch): what was made after it left is this one's.
        let handed_over = Window {
            from_ms: started_ms - 10_000,
            to_ms: started_ms,
            ..live(&own)
        };
        assert_eq!(watch(&[handed_over]).0.len(), 2);
        // A repository only found at leaving: nothing in it is attributed.
        let found = observe(&dir, Some(&start), started_ms, &Start::default(), None, &[]);
        assert!(found.phases.unwrap().is_empty());
        assert_eq!(found.observed.len(), 2);
        assert!(found.made);
        let _ = std::fs::remove_dir_all(theirs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_worktree_from_before_the_session_is_its_own_while_no_other_session_is_in_the_clone() {
        let dir = repo("resume");
        let wt = worktree_path("resume");
        // Made long before the session, as far as reflog times say.
        let added = Command::new("git")
            .args(["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()])
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(added.status.success(), "{added:?}");
        let start = head(&dir).unwrap();
        let started_ms = now_ms() - 1000;
        commit(&wt, "r.txt", "resumed");
        let watch = |others: &[Window]| {
            observe(
                &dir,
                Some(&start),
                started_ms,
                &Start::default(),
                Some(&dir),
                others,
            )
        };
        // Alone in the clone: its commit there is its phase, and its PR is
        // looked up on that worktree's branch.
        let work = watch(&[]);
        assert_eq!(subjects(&work.phases.unwrap()), ["resumed"]);
        assert_eq!(work.pr_branch.as_deref(), Some("feat"));
        // Another session in the clone then could have made it.
        let other = Window {
            id: "oh-other".to_owned(),
            checkout: std::fs::canonicalize(&dir).unwrap(),
            from_ms: started_ms,
            to_ms: now_ms() + 60_000,
        };
        let work = watch(&[other]);
        assert!(work.phases.unwrap().is_empty());
        assert_eq!(subjects(&work.observed), ["resumed"]);
        assert_eq!(work.pr_branch, None);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_commit_no_reference_names_any_more_is_found_and_what_replaced_it_wins() {
        let dir = repo("lost");
        let since = now_ms() / 1000 - 1;
        let wt = worktree_path("lost");
        git_ok(
            &dir,
            &["worktree", "add", "-q", "-b", "gone", wt.to_str().unwrap()],
        );
        // Commit times a second apart and more, as an amend's would be.
        let at = |seconds: i64, args: &[&str]| {
            let done = Command::new("git")
                .args(args)
                .env("GIT_COMMITTER_DATE", format!("@{} +0000", since + seconds))
                .current_dir(&wt)
                .output()
                .unwrap();
            assert!(done.status.success(), "{done:?}");
        };
        at(10, &["commit", "-q", "--allow-empty", "-m", "first try"]);
        at(
            20,
            &["commit", "-q", "--amend", "--allow-empty", "-m", "the work"],
        );
        at(30, &["commit", "-q", "--allow-empty", "-m", "more work"]);
        // Someone else's commit, fetched and pruned: not this clone's.
        at(
            40,
            &[
                "-c",
                "user.email=someone@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "theirs",
            ],
        );
        git_ok(
            &dir,
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
        git_ok(&dir, &["branch", "-q", "-D", "gone"]);
        let found: Vec<String> = lost_commits(&dir, since)
            .into_iter()
            .map(|l| l.subject)
            .collect();
        assert_eq!(found, ["the work", "more work"]);
        // While a reference still names them, nothing is lost.
        assert!(lost_commits(&dir, now_ms() / 1000 + 5).is_empty());
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_repositories_whose_reflogs_were_written_since_are_candidates() {
        let folder = worktree_path("folder");
        std::fs::create_dir_all(folder.join("not-a-repo")).unwrap();
        let old = folder.join("old");
        let fresh = folder.join("fresh");
        for repo in [&old, &fresh] {
            std::fs::create_dir_all(repo).unwrap();
            git_ok(repo, &["init", "-q"]);
            git_ok(repo, &["config", "user.email", "t@example.invalid"]);
            git_ok(repo, &["config", "user.name", "T"]);
            git_ok(repo, &["commit", "-q", "--allow-empty", "-m", "start"]);
        }
        // Long after both were written: nothing is a candidate.
        let later = now_ms() + 120_000;
        assert!(repos_touched_in(&folder, later).is_empty());
        // Written long ago, as far as file times say.
        let mut logs = vec![old.join(".git/logs")];
        while let Some(path) = logs.pop() {
            if path.is_dir() {
                logs.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .flatten()
                        .map(|e| e.path()),
                );
            } else {
                let long_ago =
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(long_ago)
                    .unwrap();
            }
        }
        // A worktree's commit writes its own reflog, and the branch's.
        let wt = worktree_path("fresh-wt");
        git_ok(
            &fresh,
            &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
        );
        let started_ms = now_ms();
        commit(&wt, "w.txt", "in the worktree");
        let found = repos_touched_in(&folder, started_ms);
        assert_eq!(found, [std::fs::canonicalize(&fresh).unwrap()], "{found:?}");
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn what_reached_the_default_branch_was_made_or_pushed_here_never_fetched() {
        let upstream = repo("landed-upstream");
        let dir = worktree_path("landed-clone");
        let status = Command::new("git")
            .args(["clone", "-q"])
            .arg(&upstream)
            .arg(&dir)
            .status()
            .unwrap();
        assert!(status.success());
        git_ok(&dir, &["config", "user.email", "t@example.invalid"]);
        git_ok(&dir, &["config", "user.name", "T"]);
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
        commit(&upstream, "u.txt", "theirs");
        git_ok(&dir, &["fetch", "-q"]);
        let work = observe(
            &dir,
            Some(&start),
            started_ms,
            &Start::default(),
            Some(&dir),
            &[],
        );
        assert!(work.landed.is_empty(), "a fetch is not a landing");
        commit(&dir, "m.txt", "mine, on the default branch");
        let work = observe(
            &dir,
            Some(&start),
            started_ms,
            &Start::default(),
            Some(&dir),
            &[],
        );
        let default = git(&dir, &["branch", "--show-current"]).unwrap();
        assert_eq!(
            work.landed
                .iter()
                .map(|a| (a.subject.as_str(), a.onto.as_str()))
                .collect::<Vec<_>>(),
            [("mine, on the default branch", default.as_str())]
        );
        let landed = &work.landed[0];
        assert!(landed.tip && landed.at_tip);
        assert_eq!(
            Sykli::default().check(
                &dir,
                &landed.commit,
                &landed.tree,
                &work.receipts,
                &work.checkouts,
                landed.alive
            ),
            Check::Unverified {
                why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned()
            },
            "no receipt and no contract: sykli is not even asked"
        );
        let _ = std::fs::remove_dir_all(upstream);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_a_repository_held_at_entering_is_never_this_sessions() {
        let dir = repo("held");
        let wt = worktree_path("held");
        git_ok(
            &dir,
            &["worktree", "add", "-q", "-b", "held", wt.to_str().unwrap()],
        );
        // Made by someone else in the second this session enters: reflog
        // times cannot tell it from this session's own.
        commit(&wt, "b.txt", "just before entering");
        let started_ms = now_ms() - 1000;
        let start = start_of(&dir);
        commit(&wt, "a.txt", "after entering");
        let work = observe(&dir, None, started_ms, &start, Some(&dir), &[]);
        assert_eq!(subjects(&work.phases.unwrap()), ["after entering"]);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refusal_fails_the_commit_only_when_it_is_about_the_commit() {
        let why = |check: &str| format!("refused: {check} expected a but got b");
        // sykli#67 as merged reads everything from the commit.
        for (exit, check) in [(1, "outcome"), (3, "commit"), (3, "submodules")] {
            assert!(
                matches!(
                    verdict(true, false, Some(exit), why(check)),
                    Check::Failed { .. }
                ),
                "{check}"
            );
        }
        // A run that passed under another contract or lock: not proven
        // against this one, and never failed.
        for check in ["contract", "lock"] {
            assert_eq!(
                verdict(true, true, Some(4), why(check)),
                Check::OtherContract { why: why(check) },
                "{check}"
            );
        }
        assert!(matches!(
            verdict(true, true, Some(2), "fatal: bad revision".to_owned()),
            Check::Unverified { .. }
        ));
        // A sykli that crashed or was killed said nothing about the commit.
        for exit in [None, Some(101), Some(127)] {
            assert!(
                matches!(
                    verdict(true, true, exit, "thread 'main' panicked".to_owned()),
                    Check::Unverified { .. }
                ),
                "{exit:?}"
            );
        }
        // One that still checks the checkout on disk.
        assert!(matches!(
            verdict(false, false, Some(3), why("clean")),
            Check::Failed { .. }
        ));
        assert!(matches!(
            verdict(false, false, Some(3), why("tree")),
            Check::Stale { .. }
        ));
        assert!(matches!(
            verdict(false, true, Some(3), why("tree")),
            Check::Failed { .. }
        ));
        assert!(matches!(
            verdict(false, false, Some(4), why("contract")),
            Check::Stale { .. }
        ));
    }

    #[test]
    fn worktrees_are_read_from_gits_porcelain() {
        let dir = std::env::temp_dir();
        let porcelain = format!(
            "worktree /nonexistent/bare\nbare\n\nworktree {d}\nHEAD abc\nbranch refs/heads/main\n\n\
             worktree /nonexistent/gone\nHEAD def\ndetached\nprunable gitdir file points to non-existent location",
            d = dir.display()
        );
        let found = parse_worktrees(&porcelain);
        assert_eq!(
            found,
            [Worktree {
                path: std::fs::canonicalize(&dir).unwrap(),
                head: Some("abc".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
            }],
            "bare entries and checkouts whose folder is gone are left out"
        );
    }

    #[test]
    fn created_here_reads_gits_own_reflog_wording() {
        // Written by git 2.54 for each kind of move.
        for made in [
            "commit (initial): start",
            "commit: mine",
            "commit (amend): mine, amended",
            "commit (merge): Merge branch 'side'",
            "cherry-pick: side-a",
            "revert: Revert \"side-a\"",
            "pull -q --rebase (pick): mine",
            "rebase (reword): mine",
            "rebase -i (squash): # This is a combination of 2 commits.",
            "pull -q --no-rebase --no-edit: Merge made by the 'ort' strategy.",
            "merge side: Merge made by the 'ort' strategy.",
        ] {
            assert!(created_here(made), "{made}");
        }
        for moved in [
            "clone: from /somewhere",
            "pull -q: Fast-forward",
            "merge side: Fast-forward",
            "checkout: moving from side to main",
            "reset: moving to HEAD~1",
            "pull -q --rebase (start): checkout 2ecb28b",
            "pull -q --rebase (finish): returning to refs/heads/main",
            "rebase (abort): returning to refs/heads/main",
        ] {
            assert!(!created_here(moved), "{moved}");
        }
    }

    #[test]
    fn a_receipt_of_another_schema_is_not_evidence() {
        let dir = repo("schema");
        let tree = commit(&dir, "a.txt", "x");
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join("r.json"),
            format!(r#"{{"schema":"other.v1","subject":{{"head_tree_oid":"{tree}"}},"outcome":"passed"}}"#),
        )
        .unwrap();
        assert!(receipts_in([dir.as_path()]).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_toimija_session_is_the_closest_one_started_after_ohjaa_for_its_pid() {
        let payload: Value = serde_json::from_str(
            r#"{"sessions":[{"id":"100-42"},{"id":"2003-42"},{"id":"2000-42"},{"id":"2000-7"}]}"#,
        )
        .unwrap();
        // pid 42 was reused: the session from 100 is not ours; 2000 is closer than 2003.
        assert_eq!(
            pick_toimija_session(&payload, 42, 1_999_600).as_deref(),
            Some("2000-42")
        );
        // A second of slack for the clock tick between ohjaa and toimija.
        assert_eq!(
            pick_toimija_session(&payload, 42, 2_000_900).as_deref(),
            Some("2000-42")
        );
        // Started more than a second before ohjaa did: not this session.
        assert_eq!(pick_toimija_session(&payload, 42, 2_005_000), None);
        assert_eq!(pick_toimija_session(&payload, 9, 0), None);
    }

    #[test]
    fn signals_map_to_the_shells_exit_codes() {
        assert_eq!(exit_code(Exit::Code { code: 7 }), 7);
        assert_eq!(exit_code(Exit::Signal { signal: 15 }), 143);
        let status = Command::new("sh")
            .args(["-c", "kill -TERM $$"])
            .status()
            .unwrap();
        assert_eq!(exit_of(status), Exit::Signal { signal: 15 });
    }
}
