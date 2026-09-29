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
use std::path::Path;
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

/// Sykli receipts keyed by the HEAD tree they ran on, with when they started.
pub type Receipts = std::collections::HashMap<String, (i64, Evidence)>;

/// The sykli receipts in a repository, keyed by the HEAD tree they ran on.
/// Reads `sykli-receipt.v1`: `subject.head_tree_oid`, `subject.dirty`,
/// `outcome`, `started_at_ms`. The newest receipt for a tree wins. Read once
/// per leaving and shared by `phases` and `finished`.
pub fn receipts_by_tree(repo: &Path) -> Receipts {
    let mut found = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(repo.join(".sykli").join("receipts")) else {
        return found;
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
        let evidence = Evidence {
            outcome: outcome.to_owned(),
            dirty: receipt["subject"]["dirty"].as_bool().unwrap_or(true),
        };
        match found.get(tree) {
            Some((seen, _)) if *seen >= at => {}
            _ => {
                found.insert(tree.to_owned(), (at, evidence));
            }
        }
    }
    found
}

/// The most phases a record keeps: the newest ones.
const MAX_PHASES: usize = 50;

/// State 3, observed: every commit this session made, each with whatever
/// sykli saw on its tree. A commit is this session's when it is on HEAD but
/// not on the starting commit, and HEAD's reflog shows it created in this
/// checkout since the session started. Commits a pull, merge or reset brought
/// in were made elsewhere, and are not phases. `None` when that cannot be
/// observed: no starting commit, git failed, or HEAD moved while its reflog
/// recorded nothing.
pub fn phases(
    repo: &Path,
    head_at_start: Option<&str>,
    started_ms: i64,
    receipts: &Receipts,
) -> Option<Vec<Phase>> {
    let start = head_at_start?;
    let range = format!("{start}..HEAD");
    let log = git(
        repo,
        &["log", "--reverse", "--format=%H%x1f%T%x1f%s", &range],
    )?;
    if log.is_empty() {
        return Some(Vec::new());
    }
    let created = created_since(repo, started_ms)?;
    let mut phases: Vec<Phase> = log
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\u{1f}');
            let (commit, tree, subject) = (fields.next()?, fields.next()?, fields.next()?);
            created.contains(commit).then(|| Phase {
                commit: commit.to_owned(),
                subject: subject.to_owned(),
                evidence: receipts.get(tree).map(|(_, evidence)| evidence.clone()),
            })
        })
        .collect();
    phases.drain(..phases.len().saturating_sub(MAX_PHASES));
    Some(phases)
}

/// The commits HEAD's reflog shows created in this checkout since
/// `started_ms`. `None` when the reflog cannot be read, or has no entry since
/// then at all: the caller has seen HEAD move, so an empty reflog means it is
/// not being kept, never that nothing was made.
fn created_since(repo: &Path, started_ms: i64) -> Option<HashSet<String>> {
    let since = started_ms.div_euclid(1000);
    let log = git(
        repo,
        &[
            "log",
            "--walk-reflogs",
            "--date=unix",
            "--format=%gd%x1f%H%x1f%gs",
            "HEAD",
        ],
    )?;
    let mut moves = 0;
    let mut created = HashSet::new();
    for line in log.lines() {
        let mut fields = line.splitn(3, '\u{1f}');
        let (Some(selector), Some(commit), Some(subject)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        // `HEAD@{1790380435}`, with --date=unix.
        let at = selector
            .rsplit_once("@{")
            .and_then(|(_, rest)| rest.strip_suffix('}'))
            .and_then(|secs| secs.parse::<i64>().ok());
        if at.is_none_or(|at| at < since) {
            continue;
        }
        moves += 1;
        if created_here(subject) {
            created.insert(commit.to_owned());
        }
    }
    (moves > 0).then_some(created)
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

/// State 4, observed: the branch's pull request, if `gh` can say.
pub fn finished(repo: &Path, receipts: &Receipts) -> Finished {
    let output = process::run(
        Command::new("gh")
            .args(["pr", "view", "--json", "url,state,headRefOid"])
            .current_dir(repo),
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
        .and_then(|head| git(repo, &["rev-parse", &format!("{head}^{{tree}}")]))
        .and_then(|tree| receipts.get(&tree).map(|(_, e)| e.clone()));
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
        let found = phases(&dir, Some(&start), started_ms, &receipts_by_tree(&dir)).unwrap();
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
        let receipts = receipts_by_tree(&dir);
        assert!(
            phases(&dir, Some(&start), started_ms, &receipts).unwrap()[2]
                .evidence
                .is_none(),
            "no run is said, not guessed"
        );
        assert_eq!(
            phases(&dir, None, started_ms, &receipts),
            None,
            "without a start, phases are not observed, not claimed empty"
        );
        assert_eq!(
            phases(&dir, Some(&head(&dir).unwrap()), started_ms, &receipts),
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
        let seen = |dir: &Path| phases(dir, Some(&start), started_ms, &Receipts::new()).unwrap();

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
            phases(&dir, Some(&start), started_ms, &Receipts::new()),
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
            phases(&dir, Some(&start), started_ms, &Receipts::new()),
            None,
            "HEAD moved and the reflog saw nothing: not observed, never \"no commits\""
        );
        let _ = std::fs::remove_dir_all(dir);
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
        assert!(receipts_by_tree(&dir).is_empty());
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
