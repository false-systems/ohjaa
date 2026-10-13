//! `ohjaa run` at a real terminal: a job-control shell (zsh, else bash) on a
//! pseudo-terminal of its own, driven as a person would, with a stub
//! toimija that execs the agent (a shell script that logs what reaches it).
//! The session runs in a process group of its own; these hold ohjaa to what
//! a shell does with a job's terminal, at every moment, not only at spawn.
//!
//! Sandboxed like the CLI tests: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME,
//! OHJAA_OURS, GIT_CONFIG_GLOBAL and PATH; no real toimija, agent or ohjaa
//! state. Skipped, and said so, where there is no shell or no pty.

#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `run` execs the agent; with STUB_TOIMIJA_CHILD it runs it as a child in
/// its own group and passes on TERM, HUP and INT to it as TERM, the way the
/// real `toimija run` does.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions) echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}';;
  context) echo '{}';;
  run)
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do shift; done
    shift
    [ -n "${STUB_TOIMIJA_CHILD:-}" ] || exec "$@"
    exec 3<&0
    "$@" <&3 3<&- &
    child=$!
    trap 'kill -TERM $child 2>/dev/null' TERM HUP INT
    while kill -0 $child 2>/dev/null; do wait $child; status=$?; done
    exit $status;;
esac
"#;

/// Logs what reaches it to $AGENT_LOG (and says it on its terminal): its
/// start (pid, parent), each line it reads, INT (then exits 7) and TERM
/// (exits 143). With AGENT_DEVTTY it reads and writes /dev/tty instead of
/// its stdin and stdout, as an agent with redirected output does.
const AGENT: &str = r#"#!/bin/sh
say() {
  echo "$*" >> "$AGENT_LOG"
  if [ -n "${AGENT_DEVTTY:-}" ]; then echo "$*" > /dev/tty; else echo "$*"; fi
}
trap 'say AGENT-INT; exit 7' INT
trap 'say AGENT-TERM; exit 143' TERM
say "AGENT-START $$ $PPID"
while :; do
  if [ -n "${AGENT_DEVTTY:-}" ]; then IFS= read -r line < /dev/tty; else IFS= read -r line; fi
  rc=$?
  if [ $rc -eq 0 ]; then
    say "AGENT-READ:[$line]"
  else
    say "AGENT-READ-FAILED $rc"
    sleep 1
  fi
done
"#;

struct Terminal {
    master: fs::File,
    output: Arc<Mutex<Vec<u8>>>,
    mark: usize,
    shell: Child,
    dir: PathBuf,
    /// The shell's prompt.
    prompt: &'static str,
}

/// A job-control shell this machine has.
fn shell() -> Option<(&'static str, Vec<&'static str>, &'static str)> {
    let on_path = |name: &str| {
        ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(name))
            .find(|path| path.is_file())
    };
    if on_path("zsh").is_some() {
        Some(("zsh", vec!["-f", "-i"], "TTYP%# "))
    } else if on_path("bash").is_some() {
        Some(("bash", vec!["--norc", "--noprofile", "-i"], "TTYP$ "))
    } else {
        None
    }
}

fn git(dir: &Path, args: &[&str], home: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

impl Terminal {
    /// A shell on a pty of its own (its controlling terminal), in a sandbox
    /// with a git repository to work in. `None` (said) where this machine
    /// has no shell with job control, no pty or no git.
    fn new(tag: &str) -> Option<Terminal> {
        let Some((shell, args, prompt)) = shell() else {
            eprintln!("skipped: no zsh or bash for a job-control shell");
            return None;
        };
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipped: no git");
            return None;
        }
        // SAFETY: plain pty setup calls on fds this function owns.
        let (master, slave) = unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 || libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                eprintln!("skipped: no pty");
                return None;
            }
            let name = libc::ptsname(master);
            if name.is_null() {
                eprintln!("skipped: no pty name");
                return None;
            }
            let slave = std::ffi::CStr::from_ptr(name).to_owned();
            (master, slave)
        };
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-tty-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for sub in ["home", "config", "state/ohjaa", "bin", "ours", "work"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let dir = fs::canonicalize(&dir).unwrap();
        let write = |path: PathBuf, text: &str| {
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        };
        write(dir.join("bin/toimija"), TOIMIJA_STUB);
        write(dir.join("bin/agent"), AGENT);
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ohjaa"), dir.join("bin/ohjaa")).unwrap();
        // The weekly report is not due: nothing but the session is read.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        fs::write(dir.join("state/ohjaa/report-at"), format!("{now_ms}\n")).unwrap();
        fs::write(
            dir.join("state/ohjaa/report-shown-at"),
            format!("{now_ms}\n"),
        )
        .unwrap();
        let home = dir.join("home");
        fs::write(home.join("gitconfig"), "").unwrap();
        let work = dir.join("work");
        git(&work, &["init", "-q", "-b", "main"], &home);
        git(
            &work,
            &["commit", "-q", "--allow-empty", "-m", "init"],
            &home,
        );

        let slave_path = slave.clone();
        let mut command = Command::new(shell);
        command
            .args(&args)
            .env_clear()
            .current_dir(&work)
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    dir.join("bin").display()
                ),
            )
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("OHJAA_OURS", dir.join("ours"))
            .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TERM", "dumb")
            .env("PS1", prompt)
            .env("AGENT_LOG", dir.join("agent.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: between fork and exec, only async-signal-safe calls: a new
        // session, the pty as its controlling terminal, on fds 0 to 2.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let fd = libc::open(slave_path.as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::ioctl(fd, libc::TIOCSCTTY as _, 0);
                for target in 0..3 {
                    libc::dup2(fd, target);
                }
                if fd > 2 {
                    libc::close(fd);
                }
                Ok(())
            });
        }
        let shell = command.spawn().unwrap();
        // SAFETY: the master fd is this function's own.
        let master = unsafe { fs::File::from_raw_fd(master) };
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buffer[..n]);
            }
        });
        let _ = slave;
        let mut terminal = Terminal {
            master,
            output,
            mark: 0,
            shell,
            dir,
            prompt,
        };
        assert!(
            terminal.expect(&prompt[..4], 10),
            "no prompt: {}",
            terminal.all()
        );
        Some(terminal)
    }

    fn all(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// Only what was said from here on counts for `expect`.
    fn mark(&mut self) {
        self.mark = self.output.lock().unwrap().len();
    }

    fn send(&mut self, text: &str) {
        self.master.write_all(text.as_bytes()).unwrap();
        self.master.flush().unwrap();
    }

    /// A line typed at the shell, Enter included, counted from here on.
    fn type_line(&mut self, line: &str) {
        self.mark();
        self.send(&format!("{line}\r"));
    }

    /// Whether `text` is said on the terminal within `seconds`, since the
    /// mark.
    fn expect(&mut self, text: &str, seconds: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            let said = {
                let output = self.output.lock().unwrap();
                String::from_utf8_lossy(&output[self.mark.min(output.len())..]).into_owned()
            };
            if said.contains(text) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The agent's own log.
    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("agent.log")).unwrap_or_default()
    }

    /// Whether the agent logged `text` within `seconds`.
    fn logged(&self, text: &str, seconds: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if self.log().contains(text) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The agent's pid and its parent's (the ohjaa that runs it: the stub
    /// toimija execs the agent), from its start line.
    fn agent(&self) -> (i32, i32) {
        let log = self.log();
        let line = log
            .lines()
            .find_map(|line| line.strip_prefix("AGENT-START "))
            .unwrap_or_else(|| panic!("no agent: {log}"));
        let mut pids = line.split_whitespace().map(|pid| pid.parse().unwrap());
        (pids.next().unwrap(), pids.next().unwrap())
    }

    /// The shell is at its prompt and answers: `echo` of a sum it works out,
    /// so the typed line never matches itself.
    fn shell_answers(&mut self, seconds: u64) -> bool {
        self.type_line("echo SHELL-$((40+2))-OK");
        self.expect("SHELL-42-OK", seconds)
    }

    fn report(&self) -> String {
        format!(
            "--- terminal ---\n{}\n--- agent log ---\n{}",
            self.all(),
            self.log()
        )
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Whatever is left of the sessions on this terminal goes with it.
        if let Some(line) = self
            .log()
            .lines()
            .filter_map(|line| line.strip_prefix("AGENT-START "))
            .next_back()
        {
            for pid in line.split_whitespace() {
                if let Ok(pid) = pid.parse::<i32>() {
                    // SAFETY: plain kill(2) of this test's own processes.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

const OHJAA_RUN: &str = "ohjaa run agent --intent tty --no-briefing";

/// The control: in the foreground the agent reads the terminal, and Ctrl-C
/// reaches it; the shell has its terminal back after.
#[test]
fn in_the_foreground_input_and_ctrl_c_reach_the_agent() {
    let Some(mut tty) = Terminal::new("fg") else {
        return;
    };
    tty.type_line(OHJAA_RUN);
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    tty.type_line("hello");
    assert!(tty.logged("AGENT-READ:[hello]", 10), "{}", tty.report());
    tty.send("\x03");
    assert!(tty.logged("AGENT-INT", 10), "{}", tty.report());
    assert!(tty.shell_answers(20), "{}", tty.report());
}

/// Blocker 1 of the review of #45: started with `&`, the session cannot
/// have the terminal; once `fg` brings ohjaa to the foreground the session
/// gets it, typed input and Ctrl-C reach the agent.
#[test]
fn started_in_the_background_the_session_gets_the_terminal_at_fg() {
    let Some(mut tty) = Terminal::new("bg-fg") else {
        return;
    };
    tty.type_line(&format!("{OHJAA_RUN} &"));
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    // Reading the terminal from the background stops the session; the shell
    // still has the terminal.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(tty.shell_answers(10), "{}", tty.report());
    tty.type_line("fg");
    std::thread::sleep(Duration::from_millis(1500));
    tty.type_line("typed-after-fg");
    assert!(
        tty.logged("AGENT-READ:[typed-after-fg]", 10),
        "{}",
        tty.report()
    );
    tty.send("\x03");
    assert!(tty.logged("AGENT-INT", 10), "{}", tty.report());
    assert!(tty.shell_answers(20), "{}", tty.report());
}

/// Blocker 1: every fd redirected, the agent reads and writes /dev/tty. The
/// pipeline is the foreground job, so the session gets the terminal.
#[test]
fn with_its_output_redirected_the_agent_still_reads_the_terminal() {
    let Some(mut tty) = Terminal::new("devtty") else {
        return;
    };
    tty.type_line(&format!(
        "AGENT_DEVTTY=1 {OHJAA_RUN} </dev/null 2>&1 | cat >/dev/null; echo PIPE-$((1+1))-DONE"
    ));
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    std::thread::sleep(Duration::from_millis(500));
    tty.type_line("typed-devtty");
    assert!(
        tty.logged("AGENT-READ:[typed-devtty]", 10),
        "{}",
        tty.report()
    );
    tty.send("\x03");
    assert!(tty.logged("AGENT-INT", 10), "{}", tty.report());
    assert!(tty.expect("PIPE-2-DONE", 20), "{}", tty.report());
}

/// Blocker 1: every fd redirected and an agent that never reads: Ctrl-C at
/// the terminal still ends the session at once.
#[test]
fn with_its_output_redirected_ctrl_c_still_ends_the_session() {
    let Some(mut tty) = Terminal::new("ctrlc") else {
        return;
    };
    tty.type_line(
        "ohjaa run sh --intent tty --no-briefing -- -c 'echo AGENT-START $$ $PPID >> \
         \"$AGENT_LOG\"; sleep 30' </dev/null 2>&1 | cat >/dev/null; echo PIPE-$((2+2))-DONE",
    );
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    std::thread::sleep(Duration::from_millis(500));
    let sent = Instant::now();
    tty.send("\x03");
    assert!(tty.expect("PIPE-4-DONE", 15), "{}", tty.report());
    assert!(
        sent.elapsed() < Duration::from_secs(15),
        "the session ran on: {}",
        tty.report()
    );
}

/// Should-fix 2: Ctrl-Z, then `kill %1`: the shell's SIGTERM (and SIGCONT)
/// reach only ohjaa's group; ohjaa passes them to the session's group, so
/// the stopped session ends at once, as it did when it shared ohjaa's group.
#[test]
fn ctrl_z_then_kill_ends_the_stopped_session() {
    let Some(mut tty) = Terminal::new("ctrlz-kill") else {
        return;
    };
    // toimija as it is: the agent its child, SIGTERM passed on to it.
    tty.type_line(&format!("STUB_TOIMIJA_CHILD=1 {OHJAA_RUN}"));
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    std::thread::sleep(Duration::from_millis(500));
    tty.mark();
    tty.send("\x1a");
    let prompt = &tty.prompt[..4];
    assert!(tty.expect(prompt, 10), "not suspended: {}", tty.report());
    tty.type_line("kill %1");
    assert!(tty.logged("AGENT-TERM", 10), "{}", tty.report());
    assert!(tty.shell_answers(20), "{}", tty.report());
}

/// Nit 6: under a wrapper without job control (`sh -c`), a SIGKILLed ohjaa
/// must not leave the terminal with a group nobody will give back: once the
/// agent is gone too, the wrapper still reads the terminal.
#[test]
fn under_a_wrapper_without_job_control_a_killed_ohjaa_leaves_the_terminal_usable() {
    let Some(mut tty) = Terminal::new("wrapper") else {
        return;
    };
    tty.type_line(&format!(
        "sh -c '{OHJAA_RUN}; echo AFTER-$((3+3)); read x; echo WRAPPER-GOT:$x'"
    ));
    assert!(tty.logged("AGENT-START", 15), "{}", tty.report());
    let (agent, ohjaa) = tty.agent();
    // SAFETY: plain kill(2) of this test's own processes.
    unsafe {
        libc::kill(ohjaa, libc::SIGKILL);
    }
    assert!(tty.expect("AFTER-6", 10), "{}", tty.report());
    // SAFETY: as above.
    unsafe {
        libc::kill(agent, libc::SIGTERM);
    }
    assert!(tty.logged("AGENT-TERM", 10), "{}", tty.report());
    std::thread::sleep(Duration::from_millis(500));
    tty.type_line("typed-after");
    assert!(
        tty.expect("WRAPPER-GOT:typed-after", 10),
        "the wrapper cannot read its terminal: {}",
        tty.report()
    );
}
