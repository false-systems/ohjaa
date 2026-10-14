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

/// Async-signal-safe: a single atomic store.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

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
    let program = command.get_program().to_string_lossy().into_owned();
    if cancelled() {
        anyhow::bail!("{program} was not started: interrupted");
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

/// One process as the kernel says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    /// Its session (getsid(2)); 0 when the kernel would not say.
    pub sid: u32,
    /// When it was forked, ms since the epoch (an exec keeps it); `None`
    /// when the kernel tells only the process's owner (another user's).
    pub started_ms: Option<i64>,
    /// It has exited and is not reaped yet.
    pub zombie: bool,
}

#[cfg(any(not(target_os = "macos"), test))]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The kernel's word on process `pid`, never a program's on PATH:
/// `proc_pidinfo(2)` on macOS, `/proc/<pid>/stat` on Linux, `/bin/ps` by its
/// path elsewhere. `Ok(None)` when there is no such process.
pub fn proc_of(pid: u32) -> Result<Option<Proc>, String> {
    #[cfg(target_os = "macos")]
    {
        let id = libc::c_int::try_from(pid).map_err(|_| format!("{pid} is not a pid"))?;
        // SAFETY: getsid(2) only reads.
        let sid = u32::try_from(unsafe { libc::getsid(id) }).unwrap_or(0);
        // SAFETY: proc_bsdinfo is plain data; proc_pidinfo(2) writes at
        // most `size` bytes into it.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                id,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
        if got == size {
            return Ok(Some(Proc {
                pid,
                ppid: info.pbi_ppid,
                pgid: info.pbi_pgid,
                sid,
                started_ms: Some(
                    i64::try_from(info.pbi_start_tvsec)
                        .unwrap_or(0)
                        .saturating_mul(1000)
                        + i64::try_from(info.pbi_start_tvusec / 1000).unwrap_or(0),
                ),
                zombie: info.pbi_status == libc::SZOMB,
            }));
        }
        // Another user's process (`login`, root's): the kernel gives its
        // parent and group to anyone, its start time only to its owner.
        // SAFETY: as above, for proc_bsdshortinfo.
        let mut short: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                id,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                std::ptr::addr_of_mut!(short).cast(),
                size,
            )
        };
        if got == size {
            return Ok(Some(Proc {
                pid,
                ppid: short.pbsi_ppid,
                pgid: short.pbsi_pgid,
                sid,
                started_ms: None,
                zombie: short.pbsi_status == libc::SZOMB,
            }));
        }
        let error = std::io::Error::last_os_error();
        // SAFETY: signal 0 sends nothing; kill(2) says whether it exists.
        let gone = unsafe { libc::kill(id, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            Ok(None)
        } else {
            Err(format!("the kernel says nothing of process {pid}: {error}"))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("/proc/{pid}/stat cannot be read: {error}")),
        };
        linux_stat(
            pid,
            &stat,
            now_ms(),
            &std::fs::read_to_string("/proc/uptime").unwrap_or_default(),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let output = run(
            Command::new(PS).args([
                "-o",
                "pid=",
                "-o",
                "ppid=",
                "-o",
                "pgid=",
                "-o",
                "etime=",
                "-o",
                "stat=",
                "-p",
                &pid.to_string(),
            ]),
            None,
            LOCAL,
        )
        .map_err(|error| format!("{PS} could not be run: {error:#}"))?;
        let text = output.stdout_text();
        let Some(line) = text.lines().find(|l| !l.trim().is_empty()) else {
            return Ok(None);
        };
        let fields: Vec<&str> = line.split_whitespace().collect();
        let parse = |n: usize| fields.get(n).and_then(|f| f.parse::<u32>().ok());
        let (Some(ppid), Some(pgid), Some(elapsed)) = (
            parse(1),
            parse(2),
            fields.get(3).and_then(|f| elapsed_seconds(f)),
        ) else {
            return Err(format!("{PS} said {line:?} of process {pid}"));
        };
        // SAFETY: getsid(2) only reads.
        let sid = unsafe { libc::getsid(pid as libc::pid_t) };
        Ok(Some(Proc {
            pid,
            ppid,
            pgid,
            sid: u32::try_from(sid).unwrap_or(0),
            started_ms: Some(now_ms() - elapsed * 1000),
            zombie: fields.get(4).is_some_and(|s| s.starts_with('Z')),
        }))
    }
}

/// `/proc/<pid>/stat` at `now` (ms since the epoch), with `/proc/uptime`:
/// the fields after the command (which may hold spaces and parentheses)
/// are the state, ppid, pgrp, session, …, and starttime (ticks since boot).
#[cfg(any(target_os = "linux", test))]
fn linux_stat(pid: u32, stat: &str, now: i64, uptime: &str) -> Result<Option<Proc>, String> {
    let unreadable = || format!("/proc/{pid}/stat does not read: {stat:?}");
    let (_, after) = stat.rsplit_once(')').ok_or_else(unreadable)?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let number = |n: usize| fields.get(n).and_then(|f| f.parse::<u64>().ok());
    let (Some(ppid), Some(pgid), Some(sid), Some(start)) =
        (number(1), number(2), number(3), number(19))
    else {
        return Err(unreadable());
    };
    let up: f64 = uptime
        .split_whitespace()
        .next()
        .and_then(|up| up.parse().ok())
        .ok_or_else(|| format!("/proc/uptime does not read: {uptime:?}"))?;
    #[cfg(unix)]
    // SAFETY: sysconf(3) only reads.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    #[cfg(not(unix))]
    let ticks = 100.0;
    let started_ago_ms = ((up - start as f64 / ticks) * 1000.0).max(0.0) as i64;
    Ok(Some(Proc {
        pid,
        ppid: u32::try_from(ppid).map_err(|_| unreadable())?,
        pgid: u32::try_from(pgid).map_err(|_| unreadable())?,
        sid: u32::try_from(sid).map_err(|_| unreadable())?,
        started_ms: Some(now - started_ago_ms),
        zombie: fields.first().is_some_and(|state| *state == "Z"),
    }))
}

/// This process and its ancestors, nearest first, from the kernel: this
/// process's own pid, ppid, pgid and sid from its own calls (always there),
/// then each parent's, up to pid 1. With the chain, why it stopped short
/// when a link could not be read (a parent gone between two reads, three
/// times over; a kernel that will not say): whoever asks must then fail
/// closed.
#[cfg(unix)]
pub fn ancestry() -> (Vec<Proc>, Option<String>) {
    let me = std::process::id();
    let mut tries = 0;
    'walk: loop {
        tries += 1;
        // SAFETY: these calls cannot fail for the calling process.
        let (ppid, pgid, sid) = unsafe { (libc::getppid(), libc::getpgrp(), libc::getsid(0)) };
        let own = proc_of(me);
        let mut chain = vec![Proc {
            pid: me,
            ppid: u32::try_from(ppid).unwrap_or(0),
            pgid: u32::try_from(pgid).unwrap_or(0),
            sid: u32::try_from(sid).unwrap_or(0),
            started_ms: own
                .as_ref()
                .ok()
                .and_then(|own| own.and_then(|own| own.started_ms)),
            zombie: false,
        }];
        if let Err(why) = own {
            return (chain, Some(why));
        }
        loop {
            let last = chain[chain.len() - 1];
            if last.ppid <= 1 {
                return (chain, None);
            }
            if chain.len() > 256 {
                return (
                    chain,
                    Some("its ancestry is deeper than 256 processes".to_owned()),
                );
            }
            match proc_of(last.ppid) {
                Ok(Some(parent)) if chain.iter().any(|p| p.pid == parent.pid) => {
                    let why = format!("process {} is its own ancestor", parent.pid);
                    return (chain, Some(why));
                }
                Ok(Some(parent)) => chain.push(parent),
                Ok(None) if tries < 3 => continue 'walk,
                Ok(None) => {
                    let why = format!(
                        "process {} (the parent of {}) went while its ancestry was read",
                        last.ppid, last.pid
                    );
                    return (chain, Some(why));
                }
                Err(why) => return (chain, Some(why)),
            }
        }
    }
}

#[cfg(not(unix))]
pub fn ancestry() -> (Vec<Proc>, Option<String>) {
    (Vec::new(), Some("no process groups here".to_owned()))
}

/// Whether process group `pgid` still has a member (kill(2) with signal 0
/// to the group): `None` when that cannot be told.
pub fn group_exists(pgid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let group = libc::pid_t::try_from(pgid).ok().filter(|g| *g > 1)?;
        // SAFETY: signal 0 sends nothing.
        if unsafe { libc::kill(-group, 0) } == 0 {
            return Some(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            Some(libc::EPERM) => Some(true),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        None
    }
}

/// Whether this process has a controlling terminal: `/dev/tty` opens only
/// for one that does. A person at a terminal has one; a process started in
/// the background, by a daemon, or after setsid(2) has none.
pub fn controlling_terminal() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .open("/dev/tty")
        .is_ok()
}

/// An exclusive lock on a file, held until it is dropped: flock(2), so it
/// goes with the process however that ends, and a crash never leaves one
/// behind.
#[derive(Debug)]
pub struct FileLock {
    _file: std::fs::File,
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

    /// The kernel says who this process and its ancestors are: no `ps` is
    /// asked, so none on PATH can lie about them.
    #[cfg(unix)]
    #[test]
    fn this_process_and_its_ancestors_are_the_kernels_word() {
        let (chain, why) = ancestry();
        assert_eq!(why, None);
        assert_eq!(chain[0].pid, std::process::id());
        // SAFETY: getpgrp(2) cannot fail.
        assert_eq!(chain[0].pgid, unsafe { libc::getpgrp() } as u32);
        assert!(chain.len() > 1, "{chain:?}");
        for pair in chain.windows(2) {
            assert_eq!(pair[0].ppid, pair[1].pid);
            if let (Some(child), Some(parent)) = (pair[0].started_ms, pair[1].started_ms) {
                assert!(parent <= child + 1000, "{chain:?}");
            }
        }
        let now = now_ms();
        let own = chain[0].started_ms.expect("its own start time");
        assert!(own <= now && own > now - 3_600_000);
        let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let gone = child.id();
        let _ = child.wait_with_output();
        assert_eq!(proc_of(gone).unwrap(), None);
    }

    #[test]
    fn linux_stat_reads_its_fields_after_the_command() {
        let stat = "4242 (sh (odd) name) S 4241 4200 4100 34817 4200 4194304 1 0 0 0 0 0 0 0 \
                    20 0 1 0 1500 0 0";
        let proc = linux_stat(4242, stat, 1_000_000, "100.00 50.00")
            .unwrap()
            .unwrap();
        assert_eq!(
            (proc.ppid, proc.pgid, proc.sid, proc.zombie),
            (4241, 4200, 4100, false)
        );
        // Ticks since boot 1500 at CLK_TCK (100 here): 15 s after boot,
        // the uptime 100 s: 85 s ago.
        #[cfg(unix)]
        // SAFETY: sysconf(3) only reads.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as i64;
        #[cfg(not(unix))]
        let ticks = 100;
        assert_eq!(
            proc.started_ms,
            Some(1_000_000 - (100_000 - 1500 * 1000 / ticks))
        );
        assert!(linux_stat(1, "garbage", 0, "1 1").is_err());
        let zombie = "7 (z) Z 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 10 0 0";
        assert!(linux_stat(7, zombie, 1000, "1 1").unwrap().unwrap().zombie);
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
