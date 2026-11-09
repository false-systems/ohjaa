//! A person.s decisions at a real terminal: a job-control shell (zsh, else
//! bash) on a pseudo-terminal of its own, driven as a person would.
//!
//! Sandboxed like the CLI tests: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME,
//! OHJAA_OURS, GIT_CONFIG_GLOBAL and PATH; no real toimija or ohjaa
//! state. Skipped, and said so, where there is no shell, pty or git; in CI
//! (`CI` set) that fails instead, so an image without them cannot go green.

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

/// Never the real toimija: no sessions anywhere, no context.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions) echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}';;
  context) echo '{}';;
esac
"#;

struct Terminal {
    master: fs::File,
    output: Arc<Mutex<Vec<u8>>>,
    mark: usize,
    shell: Child,
    dir: PathBuf,
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

/// Write an executable that no process holds open for writing when it runs.
/// Linux refuses to exec a file any process has open for writing (ETXTBSY),
/// and a child another test thread forks holds a copy of every descriptor
/// this process has open until that child execs: a file written here, then
/// closed, even renamed, can still be open for writing in such a child at
/// the moment it is run. So this process writes only the text, beside it,
/// and `cp`, a process of its own that forks nothing, writes the executable.
fn write_executable(path: &Path, text: &str) {
    let name = path.file_name().unwrap().to_string_lossy();
    let source = path.with_file_name(format!(".{name}.text"));
    fs::write(&source, text).unwrap();
    let copied = Command::new("cp").arg(&source).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    fs::remove_file(&source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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

/// A terminal test this machine cannot run: skipped, and said so; in CI
/// (`CI` set and not empty) it fails instead, so a CI image without a
/// shell, a pty or git cannot pass them without running them.
fn skip(why: &str) -> Option<Terminal> {
    skip_under(std::env::var_os("CI"), why)
}

fn skip_under(ci: Option<std::ffi::OsString>, why: &str) -> Option<Terminal> {
    if ci.is_some_and(|ci| !ci.is_empty()) {
        panic!("{why}: the terminal tests cannot run here, and CI is set");
    }
    eprintln!("skipped: {why}");
    None
}

#[test]
fn a_terminal_test_that_cannot_run_fails_in_ci_and_is_skipped_elsewhere() {
    assert!(skip_under(None, "no pty").is_none());
    assert!(skip_under(Some("".into()), "no pty").is_none());
    let in_ci = std::panic::catch_unwind(|| skip_under(Some("true".into()), "no pty"));
    assert!(in_ci.is_err(), "skipped in CI");
}

impl Terminal {
    /// A shell on a pty of its own (its controlling terminal), in a sandbox
    /// with a git repository to work in. `None` (said) where this machine
    /// has no shell with job control, no pty or no git; see [`skip`].
    fn new(tag: &str) -> Option<Terminal> {
        let Some((shell, args, prompt)) = shell() else {
            return skip("no zsh or bash for a job-control shell");
        };
        if Command::new("git").arg("--version").output().is_err() {
            return skip("no git");
        }
        // SAFETY: plain pty setup calls on fds this function owns.
        let (master, slave) = unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            if master < 0 || libc::grantpt(master) != 0 || libc::unlockpt(master) != 0 {
                return skip("no pty");
            }
            let name = libc::ptsname(master);
            if name.is_null() {
                return skip("no pty name");
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
        write_executable(&dir.join("bin/toimija"), TOIMIJA_STUB);
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

    /// The shell is at its prompt and answers: `echo` of a sum it works out,
    /// so the typed line never matches itself.
    fn shell_answers(&mut self, seconds: u64) -> bool {
        self.type_line("echo SHELL-$((40+2))-OK");
        self.expect("SHELL-42-OK", seconds)
    }

    fn report(&self) -> String {
        format!("--- terminal ---\n{}", self.all())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

// ---- a person's decisions, at a terminal ------------------------------------

#[test]
fn review_preserves_literal_spacing_indentation_and_long_code_lines() {
    let Some(mut tty) = Terminal::new("review-literal-layout") else {
        return;
    };
    let long_line = format!("`{}  end`", "long-literal-".repeat(12));
    let body = format!(
        "Require exactly `left  right`, not `left right`.\n\n\
         ```text\n    indented  literal  \n\ttabbed\tliteral\n```\n\n{long_line}"
    );
    let proposals = tty.dir.join("state/ohjaa/proposals/mine");
    fs::create_dir_all(&proposals).unwrap();
    fs::write(
        proposals.join("literal-layout.md"),
        format!(
            "+++\nid = \"literal-layout\"\nkind = \"lesson\"\nstatus = \"proposed\"\n\
             as_of = 2026-10-30\norigin = [\"Synthetic layout fixture\"]\n+++\n{body}\n"
        ),
    )
    .unwrap();
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    let card = tty.all()[tty.mark..].replace("\r\n", "\n");
    tty.send("q\r");
    assert!(tty.shell_answers(10), "{}", tty.report());
    for line in body.lines() {
        assert!(
            card.contains(&format!("  {line}\n")),
            "the decision card changed proposal text {line:?}:\n{card}"
        );
    }
    assert!(tty.history().is_empty());
}

#[test]
fn review_invalid_input_keeps_the_same_proposal_and_quit_keeps_the_rest() {
    let Some(mut tty) = Terminal::new("review-input") else {
        return;
    };
    fs::write(
        tty.dir.join("home/gitconfig"),
        "[user]\nname = Test Person\nemail = person@example.invalid\n",
    )
    .unwrap();
    for id in ["first-rule", "second-rule"] {
        let (code, said) = tty.run(&format!(
            "ohjaa propose --scope mine --id {id} --kind lesson \
             --origin 'Synthetic review fixture' 'Keep review choices explicit.'"
        ));
        assert_eq!(code, Some(0), "{said}");
    }
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    tty.mark();
    tty.send("aq\r");
    assert!(tty.expect("Choose one action", 5), "{}", tty.report());
    assert!(!tty.all()[tty.mark..].contains("Second rule"));
    assert!(!tty.config().join("mine/first-rule.md").exists());
    tty.mark();
    tty.send("d\r");
    assert!(tty.expect("Proposal file:", 5), "{}", tty.report());
    assert!(tty.all()[tty.mark..].contains("first-rule.md"));
    tty.send("\r");
    assert!(tty.expect("Review 2 of 2", 5), "{}", tty.report());
    tty.send("q\r");
    assert!(tty.shell_answers(10), "{}", tty.report());
    let said = tty.all();
    assert!(
        said.contains("0 accepted, 0 rejected, 1 skipped; 2 still waiting"),
        "{said}"
    );
    assert!(tty.history().is_empty());
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    tty.send("\u{4}");
    assert!(tty.shell_answers(10), "{}", tty.report());
    let said = tty.all();
    assert!(
        said.contains("0 accepted, 0 rejected, 0 skipped; 2 still waiting"),
        "{said}"
    );
    assert!(tty.history().is_empty());
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    tty.send("R\r");
    assert!(tty.expect("Review 2 of 2", 10), "{}", tty.report());
    tty.send("q\r");
    assert!(tty.shell_answers(10), "{}", tty.report());
    assert!(tty
        .all()
        .contains("0 accepted, 1 rejected, 0 skipped; 1 still waiting"));
    assert_eq!(tty.history().len(), 1);
}

#[test]
fn review_shows_a_readable_card_and_concise_success_without_hiding_history_failure() {
    let Some(mut tty) = Terminal::new("review-card") else {
        return;
    };
    fs::write(
        tty.dir.join("home/gitconfig"),
        "[user]\nname = Test Person\nemail = person@example.invalid\n",
    )
    .unwrap();
    let claude = tty.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(&claude, "# Test instructions\n<!-- ohjaa:knowledge begin: mine and machine -->\nold\n<!-- ohjaa:knowledge end -->\n").unwrap();
    let (code, said) = tty.run(
        "ohjaa propose --scope mine --id readable-review --kind lesson \
         --origin 'Synthetic review fixture' 'Keep each choice easy to understand.'",
    );
    assert_eq!(code, Some(0), "{said}");
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    let card = tty.all()[tty.mark..].to_owned();
    assert!(card.contains("Review 1 of 1"), "{card}");
    assert!(card.contains("Readable review"), "{card}");
    assert!(card.contains("Personal lesson"), "{card}");
    assert!(card.contains("Why this was proposed"), "{card}");
    assert!(card.contains("Synthetic review fixture"), "{card}");
    tty.send("a\r");
    assert!(tty.shell_answers(15), "{}", tty.report());
    let said = tty.all();
    assert!(said.contains("Accepted: Readable review"), "{said}");
    assert!(said.contains("Saved in your local history."), "{said}");
    assert!(!said.contains("  wrote   "), "{said}");
    assert!(!said.contains("  removed "), "{said}");
    assert!(!said.contains("  committed in "), "{said}");
    assert!(
        said.contains("Updated agent instructions: Claude Code."),
        "{said}"
    );
    assert!(!said.contains("rewrote the knowledge block"), "{said}");
    assert!(fs::read_to_string(&claude)
        .unwrap()
        .contains("Keep each choice easy to understand."));
    assert_eq!(tty.history().len(), 1);
    eprintln!("Synthetic terminal preview:\n{said}");

    // A full saved history can exceed the instruction block's text budget:
    // the shorter confirmation must still say that coverage is partial.
    // Keep these long rules older than the short rules accepted by this test.
    // A fixed future date made their priority change when UTC crossed it.
    for n in 0..40 {
        let id = format!("synthetic-rule-{n:02}");
        fs::write(tty.config().join("mine").join(format!("{id}.md")), format!(
            "+++\nid = \"{id}\"\nkind = \"lesson\"\nstatus = \"current\"\nas_of = 1970-01-01\norigin = [\"Synthetic coverage fixture\"]\n+++\n{}\n",
            "A synthetic rule kept only for the review coverage test. ".repeat(8)
        )).unwrap();
    }

    let (code, said) = tty.run(
        "ohjaa propose --scope mine --id another-rule --kind lesson \
         --origin 'Synthetic review fixture' 'Make the next choice explicit too.'",
    );
    assert_eq!(code, Some(0), "{said}");
    let lock = tty.config().join(".git/index.lock");
    fs::write(&lock, "a synthetic competing git operation\n").unwrap();
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 10), "{}", tty.report());
    tty.send("a\r");
    assert!(tty.shell_answers(15), "{}", tty.report());
    let said = tty.all()[tty.mark..].to_owned();
    assert!(said.contains("SHELL-42-OK"), "{said}");
    // Check all output: shell_answers moves the mark past the decision.
    let said = tty.all();
    assert!(said.contains("is not in the home's history"), "{said}");
    let words = said.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains("Agent instructions include 18 of 42 saved rules in full; 24 are only named by count because the text exceeds 8000 characters."), "{said}");
    assert!(words.contains("No saved knowledge was removed."), "{said}");
    assert!(
        fs::read_to_string(tty.config().join("mine/another-rule.md"))
            .unwrap()
            .contains("status = \"current\"")
    );
    assert_eq!(tty.history().len(), 1);
    fs::remove_file(lock).unwrap();
}

impl Terminal {
    /// `line` run at the shell and waited for; its exit status and what it
    /// said. The status is echoed with a sum the shell works out, so the
    /// typed line never matches itself.
    fn run(&mut self, line: &str) -> (Option<i32>, String) {
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(100);
        let n = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.type_line(&format!("{line}; echo \"STATUS-$?-END$(({n}*1))\""));
        let end = format!("-END{n}");
        assert!(self.expect(&end, 30), "{line}: {}", self.report());
        let said = {
            let output = self.output.lock().unwrap();
            String::from_utf8_lossy(&output[self.mark.min(output.len())..]).into_owned()
        };
        let at = said.find(&end).unwrap();
        let code = said[..at]
            .rfind("STATUS-")
            .and_then(|start| said[start + "STATUS-".len()..at].parse().ok());
        (code, said[..at].to_owned())
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config/ohjaa")
    }

    /// The home's history, newest first: each commit's message.
    fn history(&self) -> Vec<String> {
        let git_dir = self.config().join(".git");
        if !git_dir.exists() {
            return Vec::new();
        }
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(git_dir)
            .args(["log", "--format=%B%x00"])
            .current_dir(&self.dir)
            .env("HOME", self.dir.join("home"))
            .env("GIT_CONFIG_GLOBAL", self.dir.join("home/gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .map(|commit| commit.trim().to_owned())
            .filter(|commit| !commit.is_empty())
            .collect()
    }

    /// The git processes a command started, from git's trace2 events.
    fn git_starts(&self, trace: &Path) -> Vec<String> {
        let events = fs::read_to_string(trace).unwrap_or_default();
        events
            .lines()
            .filter(|line| line.contains("\"event\":\"start\""))
            .map(str::to_owned)
            .collect()
    }
}

/// Every decision a person makes on the home's knowledge (mine, machine) is
/// one commit in the home's history, saying what and why: the first makes
/// the history (`git init`), each costs two git processes (`add -A`,
/// `commit`). The agents' own instruction files get their block rewritten
/// where it is, and one without a block gets none. A commit that fails is
/// said, and the decision stands; the next commit takes it in. A decision on
/// shared knowledge is no commit here: it is reviewed where it lives.
#[test]
fn each_decision_is_one_commit_in_the_home_and_rewrites_the_agents_block() {
    let Some(mut tty) = Terminal::new("decide") else {
        return;
    };
    let home = tty.dir.join("home");
    fs::write(
        home.join("gitconfig"),
        "[user]\n\tname = A Person\n\temail = person@example.invalid\n",
    )
    .unwrap();
    // As an apply left it, and the person moved it: a block between their
    // lines, and a last line without a newline.
    let claude = home.join(".claude/CLAUDE.md");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    let block = |body: &str| {
        format!(
            "<!-- ohjaa:knowledge begin: mine and machine, written by `ohjaa apply` and after \
             each decision, removed by `ohjaa apply --undo`; edit the nodes (`ohjaa list`), not \
             this block -->\nKnowledge (ohjaa): what your team and the person you work for \
             have decided and know. Follow it; if an entry looks wrong or stale, say so instead \
             of quietly working around it. Propose changes with `ohjaa propose`; only a person \
             makes them current.\n{body}<!-- ohjaa:knowledge end -->\n"
        )
    };
    let (before, after) = ("# Mine\n\nKeep me.\n\n", "\nAfter me.");
    fs::write(&claude, format!("{before}{}{after}", block("old\n"))).unwrap();
    // One the person wrote with `brief --write`: theirs, never rewritten.
    let codex = home.join(".codex/AGENTS.md");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let briefed = "# Codex\n<!-- ohjaa:knowledge begin: generated by `ohjaa brief --write`; \
                   edit the nodes, not this block -->\nall scopes\n<!-- ohjaa:knowledge end -->\n";
    fs::write(&codex, briefed).unwrap();
    let pi = home.join(".pi/agent/AGENTS.md");
    fs::create_dir_all(pi.parent().unwrap()).unwrap();
    fs::write(&pi, "# pi, no block\n").unwrap();

    for (scope, id, kind, body) in [
        (
            "mine",
            "code-is-rust",
            "decision",
            "Code is Rust or Elixir.",
        ),
        ("mine", "answer-first", "taste", "Answer first."),
        ("machine", "lima", "fact", "Lima is here."),
        ("mine", "short-answers", "taste", "Short answers."),
        ("mine", "be-brief", "taste", "Be brief."),
        ("ours", "team-rule", "decision", "The team decides."),
    ] {
        let (code, said) = tty.run(&format!(
            "ohjaa propose --scope {scope} --id {id} --kind {kind} \
             --origin '2026-09-16: we said so' {body}"
        ));
        assert_eq!(code, Some(0), "{said}");
    }
    assert!(tty.history().is_empty(), "a proposal is no decision");

    // The first decision makes the history.
    let trace = tty.dir.join("trace-accept.json");
    let (code, said) = tty.run(&format!(
        "GIT_TRACE2_EVENT={} ohjaa accept code-is-rust",
        trace.display()
    ));
    assert_eq!(code, Some(0), "{said}");
    assert!(
        said.contains(&format!(
            "committed in {}: accept code-is-rust: Code is Rust or Elixir.",
            tty.config().display()
        )),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "rewrote the knowledge block in {}",
            claude.display()
        )),
        "{said}"
    );
    assert_eq!(
        fs::read_to_string(&claude).unwrap(),
        format!(
            "{before}{}{after}",
            block("- decision: Code is Rust or Elixir.\n")
        ),
        "rewritten where it was, every other byte kept"
    );
    assert_eq!(
        fs::read_to_string(&codex).unwrap(),
        briefed,
        "a block apply did not write is the person's"
    );
    assert!(
        said.contains(&format!(
            "left the knowledge block in {}: apply did not write it",
            codex.display()
        )),
        "{said}"
    );
    assert_eq!(
        fs::read_to_string(&pi).unwrap(),
        "# pi, no block\n",
        "a file without the block gets none"
    );
    let history = tty.history();
    assert_eq!(history.len(), 1, "{history:#?}");
    assert!(
        history[0].starts_with(
            "accept code-is-rust: Code is Rust or Elixir.\n\ndecision, mine: current as of "
        ) && history[0].ends_with("\nwhy: 2026-09-16: we said so"),
        "{history:#?}"
    );
    // Its git: the repository here (`rev-parse`), and the home's first
    // commit: `init`, `add -A`, `commit`, and no maintenance after it.
    let starts = tty.git_starts(&trace);
    let has = |starts: &[String], word: &str| {
        starts
            .iter()
            .any(|line| line.contains(&format!("\"{word}\"")))
    };
    assert!(
        has(&starts, "init") && has(&starts, "add") && has(&starts, "commit"),
        "{starts:#?}"
    );
    assert!(!has(&starts, "maintenance"), "{starts:#?}");
    assert_eq!(starts.len(), 1 + 3, "{starts:#?}");

    // Every later decision: two git processes for its commit.
    let trace = tty.dir.join("trace-reject.json");
    let (code, said) = tty.run(&format!(
        "GIT_TRACE2_EVENT={} ohjaa reject answer-first",
        trace.display()
    ));
    assert_eq!(code, Some(0), "{said}");
    let starts = tty.git_starts(&trace);
    assert!(
        has(&starts, "commit") && !has(&starts, "init"),
        "{starts:#?}"
    );
    assert_eq!(starts.len(), 1 + 2, "a decision's budget: {starts:#?}");
    let history = tty.history();
    assert_eq!(
        history[0],
        "reject answer-first: Answer first.\n\n\
         taste, mine: the proposal is dropped, never current\n\
         why it was proposed: 2026-09-16: we said so",
        "{history:#?}"
    );

    let (code, said) = tty.run("ohjaa retire code-is-rust --why 'moved to the team'");
    assert_eq!(code, Some(0), "{said}");
    let (code, said) = tty.run("ohjaa accept lima && ohjaa confirm lima");
    assert_eq!(code, Some(0), "{said}");
    let history = tty.history();
    assert_eq!(history.len(), 5, "{history:#?}");
    assert!(
        history[2].starts_with("retire code-is-rust: Code is Rust or Elixir.\n\n")
            && history[2].ends_with("\nwhy: moved to the team"),
        "{history:#?}"
    );
    assert!(history[1].starts_with("accept lima: Lima is here."));
    assert!(
        history[0].starts_with("confirm lima: Lima is here.\n\nfact, machine: still holds"),
        "{history:#?}"
    );
    assert_eq!(
        fs::read_to_string(&claude).unwrap(),
        format!("{before}{}{after}", block("- fact: Lima is here.\n")),
        "the retired decision is gone from the block"
    );

    // A commit that fails is said, and the decision stands.
    let lock = tty.config().join(".git/index.lock");
    fs::write(&lock, "").unwrap();
    let (code, said) = tty.run("ohjaa accept short-answers");
    assert_eq!(code, Some(0), "the decision stands: {said}");
    assert!(
        said.contains(
            "the decision stands, but \"accept short-answers: Short answers.\" is not in the \
             home's history"
        ),
        "{said}"
    );
    assert!(
        fs::read_to_string(tty.config().join("mine/short-answers.md"))
            .unwrap()
            .contains("status = \"current\"")
    );
    assert!(fs::read_to_string(&claude)
        .unwrap()
        .contains("- taste: Short answers.\n"));
    assert_eq!(tty.history().len(), 5);
    fs::remove_file(&lock).unwrap();

    // Shared knowledge is reviewed where it lives: no commit in the home.
    let (code, said) = tty.run("ohjaa accept team-rule");
    assert_eq!(code, Some(0), "{said}");
    assert!(tty.dir.join("ours/knowledge/team-rule.md").is_file());
    assert_eq!(tty.history().len(), 5, "no commit for shared knowledge");

    // `review`'s decisions are commits too; the next one takes in what the
    // failed one left.
    tty.type_line("ohjaa review");
    assert!(tty.expect("[a]ccept", 15), "{}", tty.report());
    tty.send("a\r");
    assert!(tty.shell_answers(20), "{}", tty.report());
    let history = tty.history();
    assert_eq!(history.len(), 6, "{history:#?}");
    let output = Command::new("git")
        .args(["show", "--stat", "--format=%s", "HEAD"])
        .current_dir(tty.config())
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        shown.starts_with("accept be-brief: Be brief.")
            && shown.contains("mine/be-brief.md")
            && shown.contains("mine/short-answers.md"),
        "{shown}"
    );
}

/// Eight decisions at once (eight jobs of one shell, each at the terminal):
/// one lock in the home holds each decision's write, its commit and the
/// agents' block, so each is exactly one commit, holding its own node and
/// nothing else, and the block ends with all eight.
#[test]
fn eight_decisions_at_once_are_eight_commits_each_its_own() {
    let Some(mut tty) = Terminal::new("eight") else {
        return;
    };
    let home = tty.dir.join("home");
    fs::write(
        home.join("gitconfig"),
        "[user]\n\tname = A Person\n\temail = person@example.invalid\n",
    )
    .unwrap();
    let claude = home.join(".claude/CLAUDE.md");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(
        &claude,
        "# Mine\n\n<!-- ohjaa:knowledge begin: mine and machine, written by `ohjaa apply` \
         and after each decision, removed by `ohjaa apply --undo`; edit the nodes (`ohjaa \
         list`), not this block -->\nold\n<!-- ohjaa:knowledge end -->\n",
    )
    .unwrap();
    let (code, said) = tty.run(
        "for i in 1 2 3 4 5 6 7 8; do ohjaa propose --scope mine --id par-$i --kind taste \
         --origin 'said once' Parallel $i. || break; done",
    );
    assert_eq!(code, Some(0), "{said}");

    let (code, said) =
        tty.run("for i in 1 2 3 4 5 6 7 8; do ohjaa accept par-$i & done; wait; echo ALL-WAITED");
    assert_eq!(code, Some(0), "{said}");
    assert!(!said.contains("warning"), "{said}");
    let history = tty.history();
    assert_eq!(
        history.len(),
        8,
        "eight decisions, eight commits: {history:#?}"
    );
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(tty.config().join(".git"))
        .args(["log", "--name-status", "--format=@%s"])
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut seen = Vec::new();
    for commit in log.split('@').filter(|commit| !commit.trim().is_empty()) {
        let mut lines = commit.lines().filter(|line| !line.trim().is_empty());
        let subject = lines.next().unwrap();
        let files: Vec<&str> = lines.collect();
        let n = subject
            .strip_prefix("accept par-")
            .and_then(|rest| rest.split(':').next())
            .unwrap_or_else(|| panic!("{subject}"));
        assert!(subject.ends_with(&format!(": Parallel {n}.")), "{subject}");
        assert_eq!(
            files,
            [format!("A\tmine/par-{n}.md")],
            "{subject} holds its own node and nothing else: {log}"
        );
        seen.push(n.to_owned());
    }
    seen.sort();
    assert_eq!(seen, ["1", "2", "3", "4", "5", "6", "7", "8"], "{log}");
    let written = fs::read_to_string(&claude).unwrap();
    for n in 1..=8 {
        assert!(
            written.contains(&format!("- taste: Parallel {n}.\n")),
            "{written}"
        );
    }
}
