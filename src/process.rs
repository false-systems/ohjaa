//! Running other programs without ever hanging on them.
//!
//! Every tool ohjaa asks something of (toimija, git) can hang.
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

#[cfg(test)]
mod tests {
    use super::*;

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
