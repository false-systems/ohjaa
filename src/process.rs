//! Running other programs without ever hanging on them.
//!
//! Every tool ohjaa asks something of (toimija, git, gh, the drafter) can hang.
//! A hang must end as a reported failure, never as a silent wait, so every
//! child runs under a deadline. The child leads its own process group, so a
//! deadline kills everything it started, including helpers that inherited its
//! output pipes and would otherwise keep them open forever.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Long enough for a cold `toimija` or `git` on a large repository.
pub const LOCAL: Duration = Duration::from_secs(60);

/// Set when the person asked ohjaa to stop what it is doing now (a signal while
/// a session is leaving). Every child still running is killed and reported as
/// interrupted, and nothing new is started, so the caller can finish quickly
/// with what it already knows.
static CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The last non-empty stderr line, or the exit status when there is none.
    pub fn failure_detail(&self) -> String {
        let stderr = String::from_utf8_lossy(&self.stderr);
        match stderr.trim().lines().last().map(str::trim) {
            Some(line) if !line.is_empty() => line.to_owned(),
            _ => format!("exited {}", self.status),
        }
    }
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_: &mut Command) {}

/// Kill the child's whole process group (the child leads it).
fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `command` with `input` on stdin, killing it and everything it started
/// once `timeout` has passed.
pub fn run(command: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    run_as_given(command, input, timeout)
}

/// [`run`], with `stdin` (a lock, [`FileLock::as_stdin`]) for its stdin.
pub fn run_with_stdin(command: &mut Command, stdin: Stdio, timeout: Duration) -> Result<Output> {
    command.stdin(stdin);
    run_as_given(command, None, timeout)
}

fn run_as_given(command: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    if cancelled() {
        anyhow::bail!("{program} was not started: interrupted");
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    own_group(command);
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;

    // Feed stdin and drain both pipes on their own threads, so a child that
    // fills a pipe buffer can never deadlock against us.
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let bytes = bytes.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let drain = |mut pipe: Box<dyn Read + Send>| {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            let _ = sender.send(buffer);
        });
        receiver
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));

    let deadline = Instant::now() + timeout;
    // Polled from 1 ms, doubling to 20 ms: most children (a `git rev-parse`
    // in a hook that runs on every commit) are done within a few
    // milliseconds, and a fixed 20 ms poll would be most of their cost.
    let mut poll = Duration::from_millis(1);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancelled() {
            kill_group(&mut child);
            anyhow::bail!("{program} was stopped: interrupted");
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            anyhow::bail!("{program} did not finish within {}s", timeout.as_secs());
        }
        std::thread::sleep(poll);
        poll = (poll * 2).min(Duration::from_millis(20));
    };

    // The child has exited, but something it started may still hold the
    // pipes open. Wait for them only until the same deadline.
    // Polled in short steps, so an interruption is noticed here too.
    let collect = |receiver: &mpsc::Receiver<Vec<u8>>| loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left.min(Duration::from_millis(50))) {
            Err(mpsc::RecvTimeoutError::Timeout) if !left.is_zero() && !cancelled() => continue,
            other => break other,
        }
    };
    match (collect(&stdout), collect(&stderr)) {
        (Ok(stdout), Ok(stderr)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        _ => {
            kill_group(&mut child);
            if cancelled() {
                anyhow::bail!("{program} was stopped: interrupted");
            }
            anyhow::bail!(
                "{program} exited, but a process it started kept its output open past {}s",
                timeout.as_secs()
            )
        }
    }
}

/// A child asked one question at a time on its stdin, answering each on its
/// stdout before the next is asked (`git cat-file --batch`). Each answer is
/// under the same deadline as [`run`]: once a question has waited `timeout`
/// for it, or ohjaa is interrupted, the child is killed with everything it
/// started, and a read from it ends; between questions it waits idle. Its
/// stderr is not read. Dropped, its stdin is closed and it is waited for.
pub struct Conversation {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    /// When the question not yet answered was asked: the deadline's start.
    asked: std::sync::Arc<std::sync::Mutex<Option<Instant>>>,
    /// Kills the child's group at the deadline; told to stand down before
    /// the child is reaped, so it never signals a process id reused since.
    watchdog: Option<(mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Conversation({})", self.child.id())
    }
}

impl Conversation {
    pub fn start(command: &mut Command, timeout: Duration) -> Result<Conversation> {
        let program = command.get_program().to_string_lossy().into_owned();
        if cancelled() {
            anyhow::bail!("{program} was not started: interrupted");
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        own_group(command);
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot run {program}"))?;
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        let (stand_down, told) = mpsc::channel::<()>();
        let group = child.id();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(None::<Instant>));
        let waiting = std::sync::Arc::clone(&asked);
        let overdue = move || {
            waiting
                .lock()
                .map(|asked| asked.is_some_and(|at| at.elapsed() >= timeout))
                .unwrap_or(true)
        };
        let watchdog = std::thread::spawn(move || loop {
            match told.recv_timeout(Duration::from_millis(50)) {
                Err(mpsc::RecvTimeoutError::Timeout) if !overdue() && !cancelled() => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    #[cfg(unix)]
                    if let Ok(group) = libc::pid_t::try_from(group) {
                        // SAFETY: the group the child leads; the child is not
                        // reaped before this thread has ended.
                        unsafe { libc::kill(-group, libc::SIGKILL) };
                    }
                    #[cfg(not(unix))]
                    let _ = group;
                    return;
                }
                _ => return,
            }
        });
        Ok(Conversation {
            child,
            stdin,
            stdout,
            asked,
            watchdog: Some((stand_down, watchdog)),
        })
    }

    /// Where the next question goes; its answer's deadline starts now.
    pub fn ask(&mut self) -> Option<&mut std::process::ChildStdin> {
        if let Ok(mut asked) = self.asked.lock() {
            *asked = Some(Instant::now());
        }
        self.stdin.as_mut()
    }

    /// Where the answers come from.
    pub fn output(&mut self) -> &mut std::io::BufReader<std::process::ChildStdout> {
        &mut self.stdout
    }

    /// The answer is read: no deadline runs until the next question.
    pub fn answered(&mut self) {
        if let Ok(mut asked) = self.asked.lock() {
            *asked = None;
        }
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        // No more questions: the child ends on its own. The watchdog stands
        // down first: once the child is reaped its id may be reused.
        drop(self.stdin.take());
        if let Some((stand_down, watchdog)) = self.watchdog.take() {
            let _ = stand_down.send(());
            let _ = watchdog.join();
        }
        let ended = Instant::now() + Duration::from_secs(2);
        let mut poll = Duration::from_millis(1);
        while Instant::now() < ended {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(poll),
                Err(_) => break,
            }
            poll = (poll * 2).min(Duration::from_millis(20));
        }
        kill_group(&mut self.child);
    }
}

/// Whether no process runs as `pid` (kill(2) with signal 0 says ESRCH).
/// Only what the kernel says: anything else, a pid it cannot be asked about
/// included, is not gone.
pub fn gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Some(pid) = libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 0) else {
            return false;
        };
        // SAFETY: signal 0 sends nothing.
        let refused = unsafe { libc::kill(pid, 0) } != 0;
        refused && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Whether the ohjaa that started a session at `started_ms` still runs as
/// `pid`. A pid is reused, so the process there must also be an `ohjaa` that
/// was already running then (a couple of seconds of slack for the clock
/// tick). `None` when that cannot be told: a process is never declared gone
/// on a guess.
pub fn ohjaa_alive(pid: u32, started_ms: i64) -> Option<bool> {
    #[cfg(unix)]
    {
        let pid_t = libc::pid_t::try_from(pid).ok().filter(|pid| *pid > 0)?;
        // SAFETY: signal 0 sends nothing; kill(2) only says whether the pid
        // exists (EPERM: it does, as another user's).
        let exists = unsafe { libc::kill(pid_t, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !exists {
            return Some(false);
        }
        let output = run(
            Command::new(PS).args(["-o", "etime=", "-o", "comm=", "-p", &pid.to_string()]),
            None,
            LOCAL,
        )
        .ok()?;
        let text = output.stdout_text();
        let line = text.trim();
        if !output.status.success() || line.is_empty() {
            // ps ran and found no such process: gone between kill(2) and ps.
            return Some(false);
        }
        let (elapsed, command) = line.split_once(char::is_whitespace)?;
        let elapsed = elapsed_seconds(elapsed)?;
        let is_ohjaa = std::path::Path::new(command.trim())
            .file_name()
            .is_some_and(|name| name == "ohjaa");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(is_ohjaa && now - elapsed <= started_ms.div_euclid(1000) + 2)
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, started_ms);
        None
    }
}

/// Whether the session's process group `pgid`, made at `started_ms`, still
/// runs: its leader (`toimija run`, whose pid is the group's id) does. The
/// agent stays in toimija's group and toimija waits for it, so the leader
/// outlives the agent; a process the agent left behind in the group (a
/// background shell, a server) does not keep the session running. A leader
/// that has exited and not been reaped yet (a zombie) is gone, and a process
/// with that pid in another group, or one that started after `started_ms`
/// (a couple of seconds of slack for the clock tick), is another process,
/// the id reused. `None` when that cannot be told: a group is never
/// declared gone on a guess.
pub fn group_alive(pgid: u32, started_ms: i64) -> Option<bool> {
    #[cfg(unix)]
    {
        let leader = libc::pid_t::try_from(pgid).ok().filter(|pgid| *pgid > 1)?;
        // SAFETY: signal 0 sends nothing; kill(2) only says whether the pid
        // exists (EPERM: it does, as another user's).
        let exists = unsafe { libc::kill(leader, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !exists {
            return Some(false);
        }
        let output = run(
            Command::new(PS).args([
                "-o",
                "pid=",
                "-o",
                "pgid=",
                "-o",
                "etime=",
                "-o",
                "stat=",
                "-p",
                &pgid.to_string(),
            ]),
            None,
            LOCAL,
        )
        .ok()?;
        if !output.status.success() && output.stdout.is_empty() {
            // ps ran and found no such process: gone between kill(2) and ps.
            return Some(false);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(leads(&output.stdout_text(), pgid, started_ms, now))
    }
    #[cfg(not(unix))]
    {
        let _ = (pgid, started_ms);
        None
    }
}

/// From `ps -o pid= -o pgid= -o etime= -o stat= -p <pgid>` at `now` (unix
/// seconds): whether the process `pgid` still leads its group, made at
/// `started_ms`: it runs (not a zombie), in that group, and started no later
/// than the group was made.
#[cfg(unix)]
fn leads(listing: &str, pgid: u32, started_ms: i64, now: i64) -> bool {
    listing.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(group), Some(elapsed), Some(stat)) = (
            fields.next().and_then(|pid| pid.parse::<u32>().ok()),
            fields.next().and_then(|group| group.parse::<u32>().ok()),
            fields.next().and_then(elapsed_seconds),
            fields.next(),
        ) else {
            return false;
        };
        pid == pgid
            && group == pgid
            && !stat.starts_with('Z')
            && now - elapsed <= started_ms.div_euclid(1000) + 2
    })
}

/// `ps`, by its path: never whichever `ps` PATH finds first, which anything
/// in a session may put there.
pub const PS: &str = "/bin/ps";

/// An exclusive lock on a file, held until it is dropped: flock(2), so it
/// goes with the process however that ends, and a crash never leaves one
/// behind.
#[derive(Debug)]
pub struct FileLock {
    _file: std::fs::File,
}

impl FileLock {
    /// This lock as the stdin of a process ohjaa starts ([`run_with_stdin`]):
    /// a copy of the same open file, so that process holds the lock too, for
    /// as long as it runs, and it is let go only once both have ended. What
    /// that process does under the lock is then never cut loose by ohjaa being
    /// killed. Only its stdin holds it: what it starts with a stdin of its
    /// own (git gives its hooks /dev/null, a daemon closes it) does not. The
    /// file is empty and open to read, so a read from it is at its end.
    pub fn as_stdin(&self) -> std::io::Result<Stdio> {
        Ok(Stdio::from(self._file.try_clone()?))
    }
}

/// Take the lock on `path` (made, with its folder, when missing), waiting
/// at most `wait` while another process holds it.
pub fn lock_file(path: &std::path::Path, wait: Duration) -> Result<FileLock, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd as _;
        let deadline = Instant::now() + wait;
        loop {
            // SAFETY: flock(2) on a descriptor this function owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(FileLock { _file: file });
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(format!("cannot lock {}: {error}", path.display()));
            }
            if Instant::now() >= deadline || cancelled() {
                return Err(format!(
                    "{} is held by another ohjaa (waited {}s)",
                    path.display(),
                    wait.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = wait;
        Ok(FileLock { _file: file })
    }
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`, in seconds.
fn elapsed_seconds(text: &str) -> Option<i64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<i64>().ok()?, clock),
        None => (0, text),
    };
    let mut seconds = 0;
    for part in clock.split(':') {
        seconds = seconds * 60 + part.parse::<i64>().ok()?;
    }
    Some(days * 86_400 + seconds)
}

/// Whether `help` (a program's `--help`) lists the option `flag`: a line
/// that begins with it, as clap lists one (`--commit <REV>`, `-c, --commit`).
/// What its sentences say is never read: they change with any rewording
/// (sykli#79 reworded the one ohjaa matched), and a sentence that names a
/// flag is not the flag.
pub fn names_flag(help: &str, flag: &str) -> bool {
    help.lines().any(|line| {
        let line = line.trim_start();
        let line = match line.as_bytes() {
            [b'-', short, b',', b' ', ..] if *short != b'-' => line[4..].trim_start(),
            _ => line,
        };
        line.strip_prefix(flag).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|next| next.is_whitespace() || matches!(next, '=' | '<' | '['))
        })
    })
}

/// For tests: write an executable that no process holds open for writing
/// when it runs. Linux refuses to exec a file any process has open for
/// writing (ETXTBSY), and a child another test thread forks holds a copy of
/// every descriptor this process has open until that child execs: a file
/// written here, then closed, even renamed, can still be open for writing in
/// such a child at the moment it is run. So this process writes only the
/// text, beside it, and `cp`, a process of its own that forks nothing,
/// writes the executable.
#[cfg(all(test, unix))]
pub fn write_executable(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let name = path.file_name().unwrap().to_string_lossy();
    let source = path.with_file_name(format!(".{name}.text"));
    std::fs::write(&source, text).unwrap();
    let copied = Command::new("cp").arg(&source).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    std::fs::remove_file(&source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_is_named_by_the_line_that_lists_it_never_by_a_sentence() {
        let help = "Usage: sykli verify [OPTIONS] <RECEIPT>\n\nOptions:\n      \
                    --commit <REV>  Judge the receipt as evidence for this commit\n      \
                    --json          Print the verdict as one JSON document\n  \
                    -h, --help      Print help\n      \
                    --repo-root=<DIR>\n\n\
                    With --commit the subject is the commit; --branch is no flag here.\n";
        for flag in ["--commit", "--json", "--help", "--repo-root"] {
            assert!(names_flag(help, flag), "{flag}");
        }
        // Named in a sentence only, the start of a longer flag, or not at all.
        for flag in ["--branch", "--comm", "--run"] {
            assert!(!names_flag(help, flag), "{flag}");
        }
        assert!(names_flag("  --run <ID>", "--run"));
        assert!(!names_flag("  --run-id <ID>", "--run"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_lock_is_one_holder_at_a_time_and_goes_with_its_holder() {
        let dir = std::env::temp_dir().join(format!("ohjaa-lock-{}", std::process::id()));
        let path = dir.join("sub").join("lock");
        let held = lock_file(&path, Duration::ZERO).unwrap();
        // flock(2) locks an open file, not a process: a second open of it
        // waits like another process would, and gives up when told.
        let started = Instant::now();
        let refused = lock_file(&path, Duration::from_millis(150)).unwrap_err();
        assert!(refused.contains("held by another ohjaa"), "{refused}");
        assert!(started.elapsed() >= Duration::from_millis(150));
        drop(held);
        let again = lock_file(&path, Duration::from_millis(200)).unwrap();
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn elapsed_times_read_as_ps_writes_them() {
        assert_eq!(elapsed_seconds("00:07"), Some(7));
        assert_eq!(elapsed_seconds("01:02:03"), Some(3723));
        assert_eq!(elapsed_seconds("2-00:00:01"), Some(172_801));
        assert_eq!(elapsed_seconds("x"), None);
    }

    #[test]
    fn this_ohjaa_is_alive_and_a_gone_pid_is_not() {
        // The test binary is not named ohjaa: alive, but not an ohjaa.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert_eq!(ohjaa_alive(std::process::id(), now), Some(false));
        let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let gone = child.id();
        child.wait().unwrap();
        assert_eq!(ohjaa_alive(gone, now), Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn a_group_runs_while_its_leader_does_and_is_never_another_group_of_its_id() {
        let now = 10_000;
        let leader = "  501   501    01:00 Ss\n";
        // Its leader started 60 s ago, as the group did.
        assert!(leads(leader, 501, (now - 60) * 1000, now));
        // Its leader started 55 s after the group was made: the id was reused.
        assert!(!leads(leader, 501, (now - 115) * 1000, now));
        // An exited leader not reaped yet is gone.
        assert!(!leads(
            "  501   501    01:00 Z\n",
            501,
            (now - 60) * 1000,
            now
        ));
        // That pid, in another group now: another process.
        assert!(!leads(
            "  501   777    01:00 S\n",
            501,
            (now - 60) * 1000,
            now
        ));
        // No such process listed.
        assert!(!leads("", 501, 0, now));
    }

    #[cfg(unix)]
    #[test]
    fn a_running_group_is_alive_and_an_ended_one_is_not() {
        use std::os::unix::process::CommandExt as _;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id();
        assert_eq!(group_alive(pgid, now), Some(true));
        // A group recorded long before its leader started is another one.
        assert_eq!(group_alive(pgid, now - 3_600_000), Some(false));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(group_alive(pgid, now), Some(false));
    }

    #[test]
    fn output_and_input_pass_through() {
        let out = run(
            Command::new("sh").args(["-c", "cat; echo err >&2"]),
            Some(b"hello"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout_text(), "hello");
        assert_eq!(out.failure_detail(), "err");
    }

    #[test]
    fn a_hang_is_killed_and_reported() {
        let started = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", "sleep 30"]),
            None,
            Duration::from_millis(200),
        )
        .err()
        .expect("a hang must fail");
        assert!(error.to_string().contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_helper_holding_the_pipes_cannot_outlive_the_deadline() {
        // The shell exits at once, but its background child keeps stdout open.
        let started = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", "sleep 30 & echo started"]),
            None,
            Duration::from_millis(500),
        )
        .err()
        .expect("a held pipe must fail, not hang");
        assert!(
            error.to_string().contains("kept its output open"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
