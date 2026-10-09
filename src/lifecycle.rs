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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;

use crate::process;
use crate::sessions::{Evidence, Exit, Finished, Phase};

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

/// The sykli receipts in these checkouts, keyed by the HEAD tree they ran on.
/// Reads `sykli-receipt.v1`: `subject.head_tree_oid`, `subject.dirty`,
/// `outcome`, `started_at_ms`. A tree is its content, so a receipt is about
/// that tree in whichever checkout sykli ran; the newest receipt for a tree
/// wins. Read once per repository at leaving, and shared by its phases and
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
            let (Some(tree), Some(outcome)) = (
                receipt["subject"]["head_tree_oid"].as_str(),
                receipt["outcome"].as_str(),
            ) else {
                continue;
            };
            let at = receipt["started_at_ms"].as_i64().unwrap_or(0);
            if found.get(tree).is_some_and(|seen| seen.at >= at) {
                continue;
            }
            let evidence = Evidence {
                outcome: outcome.to_owned(),
                dirty: receipt["subject"]["dirty"].as_bool().unwrap_or(true),
            };
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
    /// The checkout this session's newest commit was made in, where its
    /// branch's pull request is looked up; the repository itself otherwise.
    pub checkout: PathBuf,
    /// Whether any of its checkouts or branches moved during the session.
    pub moved: bool,
    /// The receipts in every checkout read.
    pub receipts: Receipts,
}

/// State 3, observed: every commit this session made in `repo`, in any of its
/// checkouts (`git worktree list`), each with whatever sykli saw on its tree.
///
/// A commit is this session's when a reflog — a checkout's HEAD, or a branch
/// — shows it created since the session started, and it is still reachable
/// from a branch, a remote branch or a checkout. Commits a pull, merge or
/// reset brought in were made elsewhere, and are not phases; nor is one that
/// an amend or a rebase replaced. Checkouts in `skip` (where another session
/// was launched) are someone else's: neither their HEAD nor the branch checked
/// out there is read, though `repo` itself is never skipped.
///
/// Phases are `None` when they cannot be observed: git failed in `repo`, or
/// its HEAD moved from `head_at_start` while its reflog recorded nothing.
pub fn observe(
    repo: &Path,
    head_at_start: Option<&str>,
    started_ms: i64,
    skip: &[PathBuf],
) -> Work {
    let repo = canonical(repo);
    let since = started_ms.div_euclid(1000);
    let all = worktrees(&repo);
    let skipped = |path: &Path| path != repo && skip.iter().any(|s| s == path);
    let checkouts: Vec<&Worktree> = all.iter().filter(|w| !skipped(&w.path)).collect();
    let mut work = Work {
        checkout: repo.clone(),
        receipts: receipts_in(checkouts.iter().map(|w| w.path.as_path())),
        ..Work::default()
    };
    // Each commit created since the start, with the checkout it was made in
    // when known.
    let mut created: std::collections::HashMap<String, Option<PathBuf>> = Default::default();
    // What each reference pointed at when the session started: nothing
    // reachable from those was made by this session.
    let mut boundaries: Vec<String> = head_at_start.into_iter().map(str::to_owned).collect();
    let mut repo_moves = None;
    for checkout in &checkouts {
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
            if created_here(&entry.subject) {
                created
                    .entry(entry.commit)
                    .or_insert_with(|| Some(checkout.path.clone()));
            }
        }
        boundaries.extend(boundary);
        work.moved |= in_window > 0;
        if checkout.path == repo {
            repo_moves = Some(in_window);
        }
    }
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
            if entry.at < since {
                if bounded.insert(entry.reference.clone()) {
                    boundaries.push(entry.commit);
                }
                continue;
            }
            if entry.reference.starts_with("refs/heads/") {
                let there = all
                    .iter()
                    .find(|w| w.branch.as_deref() == Some(entry.reference.as_str()));
                if there.is_some_and(|w| skipped(&w.path)) {
                    continue;
                }
                work.moved = true;
                if created_here(&entry.subject) {
                    created
                        .entry(entry.commit)
                        .or_insert_with(|| there.map(|w| w.path.clone()));
                }
            } else if entry.subject == "update by push" {
                work.moved = true;
            }
        }
    }
    let head_now = all
        .iter()
        .find(|w| w.path == repo)
        .and_then(|w| w.head.clone());
    let unkept = head_at_start.is_some() && head_now.as_deref() != head_at_start;
    work.phases = match repo_moves {
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
        if let Some(newest) = phases.last() {
            if let Some(Some(checkout)) = created.get(&newest.commit) {
                work.checkout = checkout.clone();
            }
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
    work
}

/// The created commits still reachable from a branch, a remote branch or a
/// checkout's HEAD, oldest first. Walks only what became reachable since the
/// start (everything the `boundaries` reach is older).
fn surviving(
    repo: &Path,
    checkouts: &[Worktree],
    created: &std::collections::HashMap<String, Option<PathBuf>>,
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
                evidence: None,
                tree: tree.to_owned(),
                worktree: None,
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

/// State 4, observed: the pull request of the branch checked out in
/// `checkout`, if `gh` can say.
pub fn finished(checkout: &Path, receipts: &Receipts) -> Finished {
    let output = process::run(
        Command::new("gh")
            .args(["pr", "view", "--json", "url,state,headRefOid"])
            .current_dir(checkout),
        None,
        std::time::Duration::from_secs(20),
    );
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return Finished::Unknown {
                why: format!("{error:#}"),
            }
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return if stderr.contains("no pull requests found") {
            Finished::NoPullRequest
        } else if stderr.contains("gh auth login") || stderr.contains("GH_TOKEN") {
            Finished::Unknown {
                why: "gh is not signed in".to_owned(),
            }
        } else {
            Finished::Unknown {
                why: output.failure_detail(),
            }
        };
    }
    let Ok(pr) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Finished::Unknown {
            why: "unreadable gh output".to_owned(),
        };
    };
    let head_evidence = pr["headRefOid"]
        .as_str()
        .and_then(|head| git(checkout, &["rev-parse", &format!("{head}^{{tree}}")]))
        .and_then(|tree| receipts.get(&tree).map(|r| r.evidence.clone()));
    Finished::PullRequest {
        url: pr["url"].as_str().unwrap_or("").to_owned(),
        state: pr["state"].as_str().unwrap_or("unknown").to_owned(),
        head_evidence,
    }
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
        observe(dir, start, started_ms, &[]).phases
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
        let work = observe(&dir, Some(&start), started_ms, &[]);
        let found = work.phases.unwrap();
        assert_eq!(subjects(&found), ["made in the worktree"]);
        assert_eq!(found[0].evidence.as_ref().unwrap().outcome, "passed");
        assert_eq!(found[0].tree, tree);
        let wt = std::fs::canonicalize(&wt).unwrap();
        assert_eq!(found[0].worktree.as_deref(), Some(wt.to_str().unwrap()));
        assert_eq!(work.checkout, wt, "its pull request is looked up there");
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
        );
        assert_eq!(skipped.phases, Some(Vec::new()));
        let own = std::fs::canonicalize(&dir).unwrap();
        commit(&dir, "o.txt", "mine");
        assert_eq!(
            subjects(
                &observe(&dir, Some(&start), started_ms, &[own, theirs.clone()])
                    .phases
                    .unwrap()
            ),
            ["mine"],
            "the checkout being observed is never skipped"
        );
        let _ = std::fs::remove_dir_all(theirs);
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
