//! Repository knowledge reaches the repository's own AGENTS.md through
//! `ohjaa lessons`, which writes the `## Lessons` section into a working
//! copy for the person to commit and push. A person's `ohjaa accept` (at a
//! real terminal) keeps a repo node in ohjaa's state, and writes nothing
//! into the repository.
//!
//! Never the network: the checkout's origin is a local bare repository, and
//! a gh stub first on PATH records any call, which no test may see. Sandboxed
//! as the CLI tests are, in an emptied environment: HOME, XDG_CONFIG_HOME,
//! XDG_STATE_HOME, OHJAA_OURS and git's global config are all the test's own.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Never the real gh: each call's arguments go to `$STUB_GH/calls`, and it
/// fails. Ohjaa calls no gh, so no test may find that file.
const GH_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_GH/calls"
echo "gh stub: not expected: $*" >&2
exit 64
"#;

const AGENTS: &str = "# Agent guide\n\nRead this first.\n\n## Languages\n\nRust.\n";

/// Never the real toimija: no sessions anywhere, no context.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions) echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}';;
  context) echo '{}';;
esac
exit 0
"#;

/// The node most tests write into AGENTS.md.
const RUST: &str = "Widget code is Rust.";
/// Its line in the section.
const RUST_LINE: &str =
    "- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-20)";

/// Write an executable that no process holds open for writing when it runs:
/// this process writes only the text, beside it, and `cp` writes the
/// executable (on Linux a sibling test's forked child can hold a descriptor
/// this process opened for writing, and the run fails with `Text file busy`).
fn write_executable(path: &Path, text: &str) {
    let name = path.file_name().unwrap().to_string_lossy();
    let source = path.with_file_name(format!(".{name}.text"));
    fs::write(&source, text).unwrap();
    let copied = Command::new("cp").arg(&source).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    fs::remove_file(&source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A git this machine has, by absolute path.
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"].map(PathBuf::from))
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .expect("git")
}

fn node_text(id: &str, kind: &str, body: &str) -> String {
    format!(
        "+++\nid = \"{id}\"\nkind = \"{kind}\"\nstatus = \"current\"\nas_of = 2026-10-20\n\
         origin = [\"why {id} was accepted\"]\n+++\n{body}\n"
    )
}

/// One sandbox: the person's home, a remote, and their checkout of it.
struct World {
    dir: PathBuf,
    home: PathBuf,
    remote: PathBuf,
    checkout: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-lessons-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let home = dir.join("homes/octocat");
        for sub in ["config", "state", "bin", "ours", "gh", "projects", "remote"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("gitconfig"),
            "[user]\n\tname = Octo Cat\n\temail = octo@example.invalid\n\
             [init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        write_executable(&dir.join("bin/gh"), GH_STUB);
        write_executable(&dir.join("bin/toimija"), TOIMIJA_STUB);
        let world = World {
            remote: dir.join("remote/widget.git"),
            checkout: dir.join("projects/widget"),
            dir,
            home,
        };
        let remote = world.remote.to_str().unwrap().to_owned();
        world.git(&world.dir, &["init", "-q", "--bare", "-b", "main", &remote]);
        let seed = world.dir.join("seed");
        world.git(
            &world.dir,
            &["clone", "-q", &remote, seed.to_str().unwrap()],
        );
        fs::write(seed.join("AGENTS.md"), AGENTS).unwrap();
        world.git(&seed, &["add", "AGENTS.md"]);
        world.git(&seed, &["commit", "-q", "-m", "agents"]);
        world.git(&seed, &["push", "-q", "origin", "HEAD:main"]);
        world.git(
            &world.dir.join("projects"),
            &["clone", "-q", &remote, "widget"],
        );
        world
    }

    /// git, as the person's: their home and global config, files only.
    fn git_command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(real_git());
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR");
        command
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_command(cwd, args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The real binary in an emptied environment, with the stub gh first on
    /// PATH and everything else the sandbox's own.
    fn ohjaa(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ohjaa"));
        command
            .env_clear()
            .current_dir(cwd)
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    self.dir.join("bin").display()
                ),
            )
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("OHJAA_OURS", self.dir.join("ours"))
            .env("OHJAA_TODAY", "2026-10-27")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env("STUB_GH", self.dir.join("gh"));
        command
    }

    fn run(&self, command: &mut Command) -> (String, String, Option<i32>) {
        let output = command.output().unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.code(),
        )
    }

    /// `ohjaa lessons` with `args`, run in `cwd`.
    fn lessons_in(&self, cwd: &Path, args: &[&str]) -> (String, String, Option<i32>) {
        let mut command = self.ohjaa(cwd);
        command.arg("lessons").args(args);
        self.run(&mut command)
    }

    /// `ohjaa lessons` with `args`, run in the checkout.
    fn lessons(&self, args: &[&str]) -> (String, String, Option<i32>) {
        self.lessons_in(&self.checkout, args)
    }

    /// A node accepted before ohjaa kept repo knowledge in its state: an
    /// untracked file in the checkout.
    fn legacy(&self, id: &str, kind: &str, body: &str) -> PathBuf {
        let dir = self.checkout.join(".ohjaa/knowledge");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        fs::write(&path, node_text(id, kind, body)).unwrap();
        path
    }

    /// The checkout's AGENTS.md as it is now.
    fn agents_md(&self) -> String {
        fs::read_to_string(self.checkout.join("AGENTS.md")).unwrap()
    }

    /// Ohjaa called no gh, and pushed nothing: the remote's main is where
    /// the seed put it.
    fn sent_nothing(&self) {
        let calls = self.dir.join("gh/calls");
        assert!(
            !calls.exists(),
            "gh was called: {}",
            fs::read_to_string(&calls).unwrap_or_default()
        );
        let heads = self.git(
            &self.dir,
            &[
                "--git-dir",
                self.remote.to_str().unwrap(),
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
            ],
        );
        assert_eq!(heads, "main\n", "nothing is pushed");
        assert_eq!(
            self.git(
                &self.dir,
                &[
                    "--git-dir",
                    self.remote.to_str().unwrap(),
                    "show",
                    "main:AGENTS.md"
                ]
            ),
            AGENTS
        );
    }

    /// The folder ohjaa keeps the checkout's accepted lessons in.
    fn accepted(&self) -> PathBuf {
        let dir = self.dir.join("state/ohjaa/lessons");
        let clones: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|_| panic!("no {}", dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        assert_eq!(clones.len(), 1, "{clones:?}");
        clones[0].clone()
    }

    /// `ohjaa brief --knowledge-only [--agent <agent>]` in `dir`, with `env`
    /// set: what it printed.
    fn brief_in(&self, dir: &Path, agent: Option<&str>, env: &[(&str, &str)]) -> String {
        let mut brief = self.ohjaa(dir);
        brief.args(["brief", "--knowledge-only"]);
        if let Some(agent) = agent {
            brief.args(["--agent", agent]);
        }
        for (key, value) in env {
            brief.env(key, value);
        }
        let (briefing, stderr, code) = self.run(&mut brief);
        assert_eq!(code, Some(0), "{stderr}");
        briefing
    }

    /// [`World::brief_in`], in the checkout.
    fn brief(&self, agent: Option<&str>, env: &[(&str, &str)]) -> String {
        self.brief_in(&self.checkout, agent, env)
    }

    fn list(&self) -> String {
        let mut list = self.ohjaa(&self.checkout);
        list.arg("list");
        let (listed, stderr, code) = self.run(&mut list);
        assert_eq!(code, Some(0), "{stderr}");
        listed
    }

    /// Write `text` at `path` in the checkout, its folders made.
    fn put(&self, path: &str, text: &str) {
        let file = self.checkout.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// A decision at a terminal, as a person makes it.
    fn decide(&self, args: &[&str]) -> String {
        let mut decide = self.ohjaa(&self.checkout);
        decide.args(args);
        let (said, code) = at_terminal(decide);
        assert_eq!(code, Some(0), "{args:?}: {said}");
        said
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// `command` on a pseudo-terminal of its own (stdin, stdout and stderr), as
/// a person at a terminal runs it: all it said, and its exit code.
fn at_terminal(mut command: Command) -> (String, Option<i32>) {
    // SAFETY: plain pty setup on descriptors this function owns.
    let (master, slave, name) = unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "no pty");
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let name = std::ffi::CStr::from_ptr(libc::ptsname(master)).to_owned();
        // Held open here too, so the master never reads as ended before the
        // last of what the command said is read.
        let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0, "no pty slave");
        (master, slave, name)
    };
    command
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
            let fd = libc::open(name.as_ptr(), libc::O_RDWR);
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
    let mut child = command.spawn().unwrap();
    // SAFETY: the master is this function's own.
    let mut reader = unsafe { fs::File::from_raw_fd(master) };
    let output = Arc::new(Mutex::new(Vec::new()));
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
    let deadline = Instant::now() + Duration::from_secs(120);
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!(
                "did not end: {}",
                String::from_utf8_lossy(&output.lock().unwrap())
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // What it said before it ended, to the last byte.
    let mut seen = usize::MAX;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let now = output.lock().unwrap().len();
        if now == seen {
            break;
        }
        seen = now;
    }
    // SAFETY: closing this function's own descriptor.
    unsafe {
        libc::close(slave);
    }
    let said = String::from_utf8_lossy(&output.lock().unwrap()).replace("\r\n", "\n");
    (said, code)
}

fn propose(world: &World, id: &str, body: &str, supersedes: Option<&str>) {
    let mut propose = world.ohjaa(&world.checkout);
    propose.args([
        "propose",
        "--scope",
        "repo",
        "--id",
        id,
        "--kind",
        "decision",
        "--origin",
        "a person said so",
    ]);
    if let Some(old) = supersedes {
        propose.args(["--supersedes", old]);
    }
    propose.arg(body);
    let (stdout, stderr, code) = world.run(&mut propose);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
}

/// A world whose checkout's AGENTS.md holds the line of [`RUST`], written by
/// `ohjaa lessons` and not committed.
fn written_world(tag: &str) -> World {
    let world = World::new(tag);
    world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    world
}

/// Left to AGENTS.md: the briefing does not repeat the node, and says so.
fn left(briefing: &str) -> bool {
    !briefing.contains(RUST) && briefing.contains("are not repeated here")
}

/// Carried: the briefing holds the node, and says why in one line.
fn carried(briefing: &str, why: &str) -> bool {
    briefing.contains(RUST)
        && briefing.contains("lessons are carried here, though its AGENTS.md holds 1 of them")
        && briefing.contains(why)
}

/// `ohjaa lessons` writes the section into the working copy's AGENTS.md, one
/// line per node, decisions first, after everything the file held, kept as
/// it was. It commits nothing, pushes nothing, calls no gh and writes no
/// state; the person reads it, then commits and pushes it. Written again,
/// nothing changes.
#[test]
fn lessons_writes_the_section_into_the_working_copy_and_nothing_else() {
    let world = World::new("write");
    let rust = world.legacy("widget-uses-rust", "decision", RUST);
    world.legacy(
        "widget-tests-first",
        "lesson",
        "Write the failing test\nfirst.",
    );
    let head = world.git(&world.checkout, &["rev-parse", "HEAD"]);
    let agents_md = world.checkout.join("AGENTS.md");
    let tests_line = "- <!-- lesson: widget-tests-first --> **lesson**: Write the failing test \
                      first. (as of 2026-10-20)";

    let (stdout, stderr, code) = world.lessons(&["--dry-run"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    for said in [
        format!("+ {RUST_LINE}"),
        format!("+ {tests_line}"),
        format!("dry run; {} is unchanged", agents_md.display()),
    ] {
        assert!(stdout.contains(&said), "missing {said:?} in:\n{stdout}");
    }
    assert_eq!(world.agents_md(), AGENTS);

    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "wrote the lessons into {}: review it, then commit and push it like any change",
            agents_md.display()
        )),
        "{stdout}"
    );
    let written = world.agents_md();
    assert!(written.starts_with(AGENTS), "{written}");
    let section = &written[AGENTS.len()..];
    assert!(
        section.starts_with("\n## Lessons\n\n<!-- This repository's accepted knowledge"),
        "{written}"
    );
    let decision = section.find(RUST_LINE).expect(&written);
    let lesson = section.find(tests_line).expect(&written);
    assert!(decision < lesson, "{written}");
    assert!(written.ends_with("(as of 2026-10-20)\n"), "{written}");
    // Ohjaa's own words name no tool (these nodes' words name none either),
    // and no why goes with a line.
    assert!(!written.to_lowercase().contains("ohjaa"), "{written}");
    assert!(!written.contains("was accepted"), "{written}");

    // Nothing committed, pushed, sent or kept: the file is the person's to
    // commit, and the nodes stay where they were.
    assert_eq!(world.git(&world.checkout, &["rev-parse", "HEAD"]), head);
    let status = world.git(&world.checkout, &["status", "--porcelain"]);
    assert!(
        status.lines().any(|line| line == " M AGENTS.md"),
        "{status}"
    );
    assert!(rust.exists());
    assert!(!world.dir.join("state/ohjaa").exists());
    world.sent_nothing();

    // Again: nothing to change.
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("nothing to change"), "{stdout}");
    assert_eq!(world.agents_md(), written);

    // The agent reads them in AGENTS.md, so the briefing no longer repeats
    // them, and says where they are; `list` says so too.
    let briefing = world.brief(Some("codex"), &[]);
    assert!(left(&briefing), "{briefing}");
    assert!(!briefing.contains("failing test"), "{briefing}");
    let listed = world.list();
    assert!(
        listed.contains(&format!("in AGENTS.md ({})", agents_md.display())),
        "{listed}"
    );

    // A line edited by hand is not the node: the briefing carries it again,
    // and `ohjaa lessons` puts the node's line back.
    fs::write(
        &agents_md,
        written.replace("Widget code is Rust.", "Widget code is anything."),
    )
    .unwrap();
    let briefing = world.brief(Some("codex"), &[]);
    assert!(briefing.contains(RUST), "{briefing}");
    assert!(
        briefing.contains("AGENTS.md holds another line of it"),
        "{briefing}"
    );
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains(&format!("~ {RUST_LINE}")), "{stdout}");
    assert_eq!(world.agents_md(), written);
    world.sent_nothing();
}

/// Until `ohjaa lessons` writes it, a current repo node is carried in the
/// briefing, marked, and `list` says how it gets there.
#[test]
fn a_node_not_yet_in_agents_md_is_carried_and_list_says_how_it_gets_there() {
    let world = World::new("notyet");
    world.legacy("widget-uses-rust", "decision", RUST);
    let briefing = world.brief(None, &[]);
    assert!(
        briefing.contains(RUST) && briefing.contains("not yet in AGENTS.md"),
        "{briefing}"
    );
    let listed = world.list();
    assert!(
        listed.contains("not yet in AGENTS.md: `ohjaa lessons` writes it there"),
        "{listed}"
    );
}

/// The working copy can be named, by any folder inside it, from anywhere;
/// a linked worktree of the clone gets the lessons accepted for the clone.
/// A folder in no repository is said, and nothing is written.
#[test]
fn lessons_writes_into_a_named_working_copy_or_a_worktree_of_the_clone() {
    let world = World::new("named");
    propose(&world, "widget-uses-rust", RUST, None);
    world.decide(&["accept", "widget-uses-rust"]);
    fs::create_dir_all(world.checkout.join("src")).unwrap();
    let inside = world.checkout.join("src");
    let (stdout, stderr, code) = world.lessons_in(&world.dir, &[inside.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let accepted_line = RUST_LINE.replace("2026-10-20", "2026-10-27");
    assert!(world.agents_md().contains(&accepted_line));

    let worktree = world.dir.join("projects/widget-other");
    world.git(
        &world.checkout,
        &["worktree", "add", "-q", worktree.to_str().unwrap()],
    );
    let (stdout, stderr, code) = world.lessons_in(&worktree, &[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        fs::read_to_string(worktree.join("AGENTS.md"))
            .unwrap()
            .contains(&accepted_line),
        "the clone's lessons, in its other worktree"
    );

    let nowhere = world.dir.join("nowhere");
    fs::create_dir_all(&nowhere).unwrap();
    let (stdout, stderr, code) = world.lessons_in(&world.dir, &[nowhere.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("is not inside a git repository"),
        "{stderr}"
    );
    assert!(!nowhere.join("AGENTS.md").exists());
    world.sent_nothing();
}

/// A person accepts a repo proposal at a terminal: it is kept in ohjaa's
/// state, nothing is written into the repository, and the person is told how
/// its line gets there. Retired, AGENTS.md still says it until `ohjaa
/// lessons` takes it out, and every briefing says not to follow it.
#[test]
fn an_accepted_or_retired_repo_node_reaches_agents_md_through_lessons() {
    let world = World::new("accept");
    propose(&world, "widget-uses-rust", RUST, None);
    let said = world.decide(&["accept", "widget-uses-rust"]);
    assert!(
        said.contains(&format!(
            "Run `ohjaa lessons` in a working copy of {} to bring its AGENTS.md in line, then \
             commit and push it like any change.",
            world.checkout.display()
        )),
        "{said}"
    );
    assert_eq!(world.agents_md(), AGENTS, "nothing is written into it");
    assert!(!world.checkout.join(".ohjaa").exists());
    assert!(world.accepted().join("widget-uses-rust.md").is_file());
    world.sent_nothing();

    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(world
        .agents_md()
        .contains(&RUST_LINE.replace("2026-10-20", "2026-10-27")));

    let said = world.decide(&["retire", "widget-uses-rust", "--why", "the widget is gone"]);
    assert!(said.contains("Run `ohjaa lessons`"), "{said}");
    assert!(world.agents_md().contains("widget-uses-rust"));
    let still = "AGENTS.md still says `widget-uses-rust`, but it was retired on 2026-10-27 (the \
                 widget is gone); `ohjaa lessons` takes its line out: do not follow it.";
    // So does a claude session, which reads AGENTS.md only by chance.
    for agent in ["codex", "claude"] {
        let briefing = world.brief(Some(agent), &[]);
        assert!(briefing.contains(still), "{agent}: {briefing}");
    }
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("- the line of `widget-uses-rust`"),
        "{stdout}"
    );
    assert!(!world.agents_md().contains("widget-uses-rust"));
    assert!(!world.brief(Some("codex"), &[]).contains("still says"));
    world.sent_nothing();
}

#[test]
fn a_superseded_line_still_in_agents_md_names_its_successor() {
    let world = written_world("superseded");
    propose(
        &world,
        "widget-uses-go",
        "Widget code is Go.",
        Some("widget-uses-rust"),
    );
    world.decide(&["accept", "widget-uses-go"]);
    let briefing = world.brief(Some("codex"), &[]);
    assert!(
        briefing.contains("AGENTS.md still says `widget-uses-rust`"),
        "{briefing}"
    );
    assert!(
        briefing.contains("superseded by `widget-uses-go`"),
        "{briefing}"
    );
    assert!(briefing.contains("Widget code is Go."), "{briefing}");
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let written = world.agents_md();
    assert!(
        written.contains("Widget code is Go.") && !written.contains(RUST),
        "{written}"
    );
}

/// AGENTS.md is read whole or not at all: one that is not UTF-8 is never
/// written over.
#[test]
fn an_agents_md_that_cannot_be_read_whole_is_never_written_over() {
    let world = World::new("notutf8");
    let latin1: &[u8] = b"# Agent guide\n\nLatin-1: caf\xe9\n";
    fs::write(world.checkout.join("AGENTS.md"), latin1).unwrap();
    world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("not UTF-8"), "{stderr}");
    assert_eq!(fs::read(world.checkout.join("AGENTS.md")).unwrap(), latin1);
}

/// A repo node that cannot be read is no node gone: AGENTS.md keeps its
/// line, the file is named, and `ohjaa lessons` exits 4.
#[test]
fn an_unreadable_repo_node_keeps_its_line_and_is_said() {
    let world = written_world("unreadable");
    let written = world.agents_md();
    let file = world.checkout.join(".ohjaa/knowledge/widget-uses-rust.md");
    fs::write(&file, "not a knowledge node\n").unwrap();
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        stderr.contains(&format!("unreadable {}", file.display())),
        "{stderr}"
    );
    assert_eq!(world.agents_md(), written);
}

#[test]
fn a_scoped_repositorys_lessons_are_carried_though_its_agents_md_holds_them() {
    let world = written_world("scoped");
    // Briefed from elsewhere, with the repository in scope: the agent is not
    // working there, and need not read its AGENTS.md.
    let mut brief = world.ohjaa(&world.dir);
    brief.args([
        "brief",
        "--knowledge-only",
        "--scope",
        world.checkout.to_str().unwrap(),
    ]);
    let (briefing, stderr, code) = world.run(&mut brief);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(briefing.contains(RUST), "{briefing}");
    assert!(briefing.contains("in AGENTS.md too"), "{briefing}");
}

#[test]
fn retiring_an_untracked_node_at_a_terminal_takes_it_out_of_the_checkout() {
    let world = World::new("retire-untracked");
    world.legacy("widget-old-rule", "decision", "An old rule.");
    let said = world.decide(&["retire", "widget-old-rule", "--why", "no longer"]);
    assert!(
        !world.checkout.join(".ohjaa").exists(),
        "nothing is left in the checkout: {said}"
    );
    let node = fs::read_to_string(world.accepted().join("widget-old-rule.md")).unwrap();
    assert!(node.contains("status = \"retired\""), "{node}");
    assert!(node.contains("retired: no longer"), "{node}");
    world.sent_nothing();
}

#[test]
fn a_node_in_both_the_checkout_and_ohjaas_state_is_ohjaas() {
    let world = World::new("both");
    let file = world.legacy("widget-uses-rust", "decision", RUST);
    let text = fs::read_to_string(&file).unwrap();
    world.decide(&["retire", "widget-uses-rust", "--why", "gone"]);
    // A move that was cut short: the checkout's copy back beside the state's.
    world.put(".ohjaa/knowledge/widget-uses-rust.md", &text);
    let mut brief = world.ohjaa(&world.checkout);
    brief.args(["brief", "--knowledge-only", "--agent", "codex"]);
    let (briefing, stderr, code) = world.run(&mut brief);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !briefing.contains(RUST),
        "the retired node stays retired: {briefing}"
    );
    assert!(!stderr.contains("duplicate"), "{stderr}");
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains(&format!("left {}", file.display())),
        "{stderr}"
    );
    assert!(!world.agents_md().contains(RUST));
}

#[test]
fn codex_is_left_the_lessons_unless_an_override_or_its_byte_cap_hides_them() {
    let world = written_world("codex");
    let briefing = world.brief(Some("codex"), &[]);
    assert!(left(&briefing), "{briefing}");
    world.put("AGENTS.override.md", "Read this instead.\n");
    let briefing = world.brief(Some("codex"), &[]);
    assert!(carried(&briefing, "AGENTS.override.md"), "{briefing}");
    fs::remove_file(world.checkout.join("AGENTS.override.md")).unwrap();
    // Past the bytes of project instructions Codex reads, the section is
    // never read.
    fs::create_dir_all(world.home.join(".codex")).unwrap();
    fs::write(
        world.home.join(".codex/config.toml"),
        "model = \"x\"\nproject_doc_max_bytes = 64\n\n[profiles.x]\nmodel = \"y\"\n",
    )
    .unwrap();
    let briefing = world.brief(Some("codex"), &[]);
    assert!(carried(&briefing, "project_doc_max_bytes"), "{briefing}");
}

#[test]
fn claude_launched_in_a_subfolder_skips_the_roots_import_unless_its_project_approved_them() {
    let world = written_world("subfolder");
    world.put("CLAUDE.md", "@AGENTS.md\n");
    fs::create_dir_all(world.checkout.join("src")).unwrap();
    assert!(
        left(&world.brief(Some("claude"), &[])),
        "at the root it reads it"
    );
    // Claude Code 2.1.311 skips an import whose file is outside the folder it
    // starts in, unless the project approved external imports.
    let briefing = world.brief_in(&world.checkout.join("src"), Some("claude"), &[]);
    assert!(
        carried(&briefing, "hasClaudeMdExternalIncludesApproved"),
        "{briefing}"
    );
    let config = |approved: bool| {
        fs::write(
            world.home.join(".claude.json"),
            format!(
                "{{\"projects\":{{\"{}\":{{\"hasClaudeMdExternalIncludesApproved\":{approved}}}}}}}",
                world.checkout.display()
            ),
        )
        .unwrap();
    };
    config(false);
    let briefing = world.brief_in(&world.checkout.join("src"), Some("claude"), &[]);
    assert!(
        carried(&briefing, "hasClaudeMdExternalIncludesApproved"),
        "{briefing}"
    );
    config(true);
    let briefing = world.brief_in(&world.checkout.join("src"), Some("claude"), &[]);
    assert!(left(&briefing), "{briefing}");
}

#[test]
fn an_import_claude_code_ignores_does_not_count() {
    let world = written_world("ignored");
    for (name, text) in [
        ("a comment", "<!-- @AGENTS.md -->\n"),
        ("a comment over lines", "<!--\n@AGENTS.md\n-->\n"),
        ("an HTML block", "<details>\n@AGENTS.md\n</details>\n"),
        ("frontmatter", "---\nnote: x\n@AGENTS.md\n---\n# Notes\n"),
        ("indented code", "# Notes\n\n    @AGENTS.md\n"),
    ] {
        world.put("CLAUDE.md", text);
        let briefing = world.brief(Some("claude"), &[]);
        assert!(carried(&briefing, "does not import"), "{name}: {briefing}");
    }
    world.put("CLAUDE.md", "@AGENTS.md\n");
    assert!(left(&world.brief(Some("claude"), &[])));
    // Claude Code reads no CLAUDE.md at all with it set (as --safe-mode sets it).
    let briefing = world.brief(Some("claude"), &[("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")]);
    assert!(
        carried(&briefing, "CLAUDE_CODE_DISABLE_CLAUDE_MDS"),
        "{briefing}"
    );
}

/// `ohjaa lessons` writes into the working copy and nowhere else: an
/// `AGENTS.md` that is a link leading outside it is refused and left as it
/// is, the file it leads to untouched. A link to a file inside the working
/// copy is written through, and stays a link.
#[test]
fn an_agents_md_that_leads_outside_the_working_copy_is_never_written() {
    let world = World::new("symlink");
    world.legacy("widget-uses-rust", "decision", RUST);
    let agents_md = world.checkout.join("AGENTS.md");
    let outside = world.dir.join("elsewhere.md");
    fs::write(&outside, AGENTS).unwrap();
    fs::remove_file(&agents_md).unwrap();
    std::os::unix::fs::symlink(&outside, &agents_md).unwrap();
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("outside the working copy"),
        "{stdout}{stderr}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), AGENTS);
    assert_eq!(fs::read_link(&agents_md).unwrap(), outside);

    // A link that leads nowhere is refused too: writing would replace it.
    fs::remove_file(&agents_md).unwrap();
    std::os::unix::fs::symlink(world.dir.join("gone.md"), &agents_md).unwrap();
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(fs::symlink_metadata(&agents_md)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!world.dir.join("gone.md").exists());

    // A link to a file inside the working copy is written through.
    fs::remove_file(&agents_md).unwrap();
    world.put("docs/guide.md", AGENTS);
    std::os::unix::fs::symlink("docs/guide.md", &agents_md).unwrap();
    let (stdout, stderr, code) = world.lessons(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(fs::read_to_string(world.checkout.join("docs/guide.md"))
        .unwrap()
        .contains(RUST_LINE));
    assert!(fs::symlink_metadata(&agents_md)
        .unwrap()
        .file_type()
        .is_symlink());
}
