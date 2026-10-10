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

/// Sykli receipts keyed by the HEAD tree they ran on.
pub type Receipts = std::collections::HashMap<String, Receipt>;

/// The sykli receipts in these checkouts, keyed by the tree that ran.
/// Reads `sykli-receipt.v1`: `subject.tree_oid`, `subject.head_tree_oid`,
/// `subject.dirty`, `outcome`, `started_at_ms`. A tree is its content, so a
/// receipt is about that tree in whichever checkout sykli ran. Of several for
/// one tree, a clean run (not dirty, and HEAD's own tree) wins, then a
/// passing one, then the newest. Read once per repository at leaving, and shared by its phases and
/// its pull request.
pub fn receipts_in<'a>(checkouts: impl IntoIterator<Item = &'a Path>) -> Receipts {
    let mut found = Receipts::new();
    for checkout in checkouts {
        let Ok(entries) = std::fs::read_dir(checkout.join(".sykli").join("receipts")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
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
            let evidence = Evidence {
                outcome: outcome.to_owned(),
                dirty: receipt["subject"]["dirty"].as_bool().unwrap_or(true) || head != Some(tree),
            };
            // A clean run first, then a passing one, then the newest.
            let rank = |e: &Evidence, at: i64| (!e.dirty, e.outcome == "passed", at);
            if found
                .get(tree)
                .is_some_and(|seen| rank(&seen.evidence, seen.at) >= rank(&evidence, at))
            {
                continue;
            }
            found.insert(
                tree.to_owned(),
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
    /// `None` when they could not be observed (see [`observe`]).
    pub phases: Option<Vec<Phase>>,
    /// The branch this session's newest commit on a branch was made on, and
    /// that commit: where its pull request is looked up.
    pub pr_branch: Option<String>,
    pub pr_commit: Option<String>,
    /// Whether any of its checkouts or branches moved during the session.
    pub moved: bool,
    /// The receipts in every checkout read.
    pub receipts: Receipts,
    /// The commits that reached the default branch during the session, oldest
    /// first (see [`observe`]); sykli is asked about each by the caller.
    pub landed: Vec<Arrived>,
    /// Every checkout of the repository.
    pub checkouts: Vec<Worktree>,
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
}

/// The most landed commits a record keeps: the newest ones.
const MAX_LANDED: usize = 20;

/// State 3, observed: every commit this session made in `repo`, in any of its
/// checkouts (`git worktree list`), each with whatever sykli saw on its tree.
///
/// A commit is this session's when a reflog — a checkout's HEAD, or a branch
/// — shows it created since the session started, and it is still reachable
/// from a branch, a remote branch or a checkout. Commits a pull, merge or
/// reset brought in were made elsewhere, and are not phases; nor is one that
/// an amend or a rebase replaced. Checkouts in `skip` (where another session
/// was launched) are someone else's: neither their HEAD nor the branch checked
/// out there is read as this session's. That may include `repo` itself, when
/// it was only found at leaving; a caller that was launched in or named
/// `repo` leaves it out.
///
/// Also observed: the commits that reached the default branch (`origin/HEAD`,
/// else `main` or `master`) during the session — every commit a push from
/// here took to the remote one, and every commit this session made that
/// arrived on the local one. A push is not told apart by checkout, so the
/// commits made in a skipped checkout are left out of it.
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
    skip: &[PathBuf],
    start: &Start,
) -> Work {
    let repo = canonical(repo);
    let since = started_ms.div_euclid(1000);
    let all = worktrees(&repo);
    let skipped = |path: &Path| skip.iter().any(|s| s == path);
    let mut work = Work {
        receipts: receipts_in(
            all.iter()
                .filter(|w| !skipped(&w.path))
                .map(|w| w.path.as_path()),
        ),
        ..Work::default()
    };
    // Each commit created since the start, with the checkout it was made in
    // when known; and those another session made in a skipped checkout.
    let mut created: HashMap<String, Option<PathBuf>> = HashMap::new();
    // The branch each was made on, or the remote branch a push from here named.
    let mut branch_of: HashMap<String, String> = HashMap::new();
    // Each remote branch's pushes since the start, newest first, and where it
    // pointed at the start.
    let mut pushes: HashMap<String, (Vec<Move>, Option<String>)> = HashMap::new();
    let mut theirs: HashSet<String> = HashSet::new();
    // What each reference pointed at when the session started: nothing
    // reachable from those was made by this session.
    let mut boundaries: Vec<String> = head_at_start
        .into_iter()
        .map(str::to_owned)
        .chain(start.local.iter().cloned())
        .chain(start.remote.iter().cloned())
        .collect();
    let mut repo_moves = None;
    for checkout in &all {
        let ours = !skipped(&checkout.path);
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
            if entry.at < since {
                boundary.get_or_insert(entry.commit);
                continue;
            }
            in_window += 1;
            if !created_here(&entry.subject) {
                continue;
            }
            if ours {
                created
                    .entry(entry.commit)
                    .or_insert_with(|| Some(checkout.path.clone()));
            } else {
                theirs.insert(entry.commit);
            }
        }
        boundaries.extend(boundary);
        if ours {
            work.moved |= in_window > 0;
            if checkout.path == repo {
                repo_moves = Some(in_window);
            }
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
            if entry.reference.starts_with("refs/heads/") {
                let there = all
                    .iter()
                    .find(|w| w.branch.as_deref() == Some(entry.reference.as_str()));
                if there.is_some_and(|w| skipped(&w.path)) {
                    if created_here(&entry.subject) {
                        theirs.insert(entry.commit);
                    }
                    continue;
                }
                work.moved = true;
                if created_here(&entry.subject) {
                    created
                        .entry(entry.commit.clone())
                        .or_insert_with(|| there.map(|w| w.path.clone()));
                    let name = entry.reference.trim_start_matches("refs/heads/");
                    branch_of
                        .entry(entry.commit.clone())
                        .or_insert_with(|| name.to_owned());
                }
            } else if entry.subject == "update by push" {
                work.moved = true;
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
    // A commit a push from here took to a remote branch during the session is
    // this session's work even once its local branch and checkout are gone (a
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
                if pushed.committed < since || theirs.contains(&pushed.commit) {
                    continue;
                }
                branch_of
                    .entry(pushed.commit.clone())
                    .or_insert_with(|| name.to_owned());
                created.entry(pushed.commit).or_insert(None);
            }
        }
    }
    let head_now = all
        .iter()
        .find(|w| w.path == repo)
        .and_then(|w| w.head.clone());
    let unkept = head_at_start.is_some() && head_now.as_deref() != head_at_start;
    work.phases = match repo_moves {
        None if skipped(&repo) => surviving(&repo, &all, &created, &boundaries),
        None => None,
        Some(0) if unkept && created.is_empty() => None,
        Some(_) => surviving(&repo, &all, &created, &boundaries),
    };
    if let Some(phases) = &mut work.phases {
        for phase in phases.iter_mut() {
            phase.evidence = work
                .receipts
                .get(&phase.tree)
                .map(|receipt| receipt.evidence.clone());
        }
        for phase in phases.iter_mut() {
            phase.branch = branch_of.get(&phase.commit).cloned();
        }
        // Its pull request is the one for the branch its newest commit on a
        // branch was made on: never whatever a checkout holds now.
        if let Some(newest) = phases.iter().rev().find(|p| p.branch.is_some()) {
            work.pr_branch = newest.branch.clone();
            work.pr_commit = Some(newest.commit.clone());
        }
        {
            for phase in phases.iter_mut() {
                phase.worktree = created
                    .get(&phase.commit)
                    .cloned()
                    .flatten()
                    .filter(|path| *path != repo)
                    .map(|path| path.to_string_lossy().into_owned());
            }
        }
    }
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
            let already = if remote { &on_remotes } else { &start.local };
            for commit in range(&repo, &entry.commit, old.as_deref(), already) {
                let ours = if remote {
                    !theirs.contains(&commit.commit)
                } else {
                    created.contains_key(&commit.commit)
                };
                if ours && seen.insert(commit.commit.clone()) {
                    work.landed.push(Arrived {
                        onto: onto.clone(),
                        tip: commit.commit == entry.commit,
                        at_tip: now_at.as_deref() == Some(commit.commit.as_str()),
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
    created: &HashMap<String, Option<PathBuf>>,
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
/// request by that name any more, the one that holds `commit`; and what sykli
/// says about its head. Asked of `gh` in `repo`.
pub fn finished(
    repo: &Path,
    branch: &str,
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
    let output = match gh(&["pr", "view", branch, "--json", "url,state,headRefOid"]) {
        Ok(output) => output,
        Err(error) => {
            return Finished::Unknown {
                why: format!("{error:#}"),
            }
        }
    };
    let pr = if output.status.success() {
        serde_json::from_slice::<Value>(&output.stdout).ok()
    } else {
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
        // A branch deleted after a squash merge: the pull request still holds
        // the commit.
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
        Some(tree) => sykli.check(repo, head, tree, receipts, checkouts),
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

/// This machine's sykli, asked at most once per leaving whether it can bind
/// a receipt to a commit (`sykli verify --commit`).
#[derive(Debug, Default)]
pub struct Sykli {
    binds_commits: std::cell::OnceCell<Result<(), String>>,
}

impl Sykli {
    fn binds_commits(&self) -> Result<(), String> {
        self.binds_commits
            .get_or_init(|| {
                let output = process::run(
                    Command::new("sykli").args(["verify", "--help"]),
                    None,
                    process::LOCAL,
                )
                .map_err(|error| format!("sykli could not be asked: {error:#}"))?;
                let help = format!(
                    "{}{}",
                    output.stdout_text(),
                    String::from_utf8_lossy(&output.stderr)
                );
                if help.contains("--commit") {
                    Ok(())
                } else {
                    Err("sykli has no --commit".to_owned())
                }
            })
            .clone()
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
    pub fn check(
        &self,
        repo: &Path,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        checkouts: &[Worktree],
    ) -> Check {
        let Some(receipt) = receipts.get(tree) else {
            let has_contract = ["sykli.json", "sykli.rs"]
                .iter()
                .any(|name| git(repo, &["cat-file", "-e", &format!("{commit}:{name}")]).is_some());
            return if has_contract {
                Check::Unchecked
            } else {
                Check::Unverified {
                    why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned(),
                }
            };
        };
        if let Err(why) = self.binds_commits() {
            return Check::Unverified { why };
        }
        let at_commit = checkouts
            .iter()
            .filter(|w| w.head.as_deref() == Some(commit))
            .map(|w| w.path.as_path())
            .find(|path| clean(path));
        let (checkout, is_the_commit) = match at_commit {
            Some(path) => (path, true),
            None => (receipt.checkout.as_path(), false),
        };
        let Some(contract) = ["sykli.json", "sykli.rs"]
            .into_iter()
            .find(|name| checkout.join(name).is_file())
        else {
            return Check::Unverified {
                why: format!(
                    "no sykli contract (sykli.json or sykli.rs) in {}",
                    checkout.display()
                ),
            };
        };
        let output = process::run(
            Command::new("sykli")
                .arg("verify")
                .arg(&receipt.path)
                .args(["--contract", contract, "--commit", commit])
                .current_dir(checkout),
            None,
            process::LOCAL,
        );
        let output = match output {
            Err(error) => {
                return Check::Unverified {
                    why: format!("sykli verify could not run: {error:#}"),
                }
            }
            Ok(output) if output.status.success() => return Check::Passed,
            Ok(output) => output,
        };
        let why = output.failure_detail();
        // sykli names the first check that failed: `refused: <check> expected …`.
        let refused = why
            .strip_prefix("refused: ")
            .and_then(|rest| rest.split_whitespace().next());
        match refused {
            Some("commit" | "clean" | "outcome" | "records" | "schema") => Check::Failed { why },
            Some("tree" | "inputs" | "contract") if is_the_commit => Check::Failed { why },
            Some("tree" | "inputs" | "contract") => Check::Stale { why },
            _ => Check::Unverified {
                why: format!("sykli verify could not say: {why}"),
            },
        }
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
        observe(dir, start, started_ms, &[], &Start::default()).phases
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
        let work = observe(&dir, Some(&start), started_ms, &[], &Start::default());
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
        assert!(work.moved);
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
    fn a_checkout_another_session_was_launched_in_is_not_this_sessions() {
        let dir = repo("wt-skip");
        let start = head(&dir).unwrap();
        let started_ms = now_ms();
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
        commit(&theirs, "t.txt", "another session's work");
        let theirs = std::fs::canonicalize(&theirs).unwrap();
        assert_eq!(phases(&dir, Some(&start), started_ms).unwrap().len(), 1);
        let skipped = observe(
            &dir,
            Some(&start),
            started_ms,
            std::slice::from_ref(&theirs),
            &Start::default(),
        );
        assert_eq!(skipped.phases, Some(Vec::new()));
        let own = std::fs::canonicalize(&dir).unwrap();
        commit(&dir, "o.txt", "made where another session was launched");
        let mine = worktree_path("mine");
        git_ok(
            &dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "mine",
                mine.to_str().unwrap(),
            ],
        );
        commit(&mine, "m.txt", "mine");
        assert_eq!(
            subjects(
                &observe(
                    &dir,
                    None,
                    started_ms,
                    &[own, theirs.clone()],
                    &Start::default(),
                )
                .phases
                .unwrap()
            ),
            ["mine"],
            "a repository only found at leaving can be another session's launch checkout too"
        );
        let _ = std::fs::remove_dir_all(mine);
        let _ = std::fs::remove_dir_all(theirs);
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
        let work = observe(&dir, Some(&start), started_ms, &[], &Start::default());
        assert!(work.landed.is_empty(), "a fetch is not a landing");
        commit(&dir, "m.txt", "mine, on the default branch");
        let work = observe(&dir, Some(&start), started_ms, &[], &Start::default());
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
                &work.checkouts
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
        let work = observe(&dir, None, started_ms, &[], &start);
        assert_eq!(subjects(&work.phases.unwrap()), ["after entering"]);
        let _ = std::fs::remove_dir_all(wt);
        let _ = std::fs::remove_dir_all(dir);
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
