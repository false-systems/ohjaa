//! Repository knowledge reaches the repository's own AGENTS.md through a
//! pull request: `ohjaa lessons publish` for the nodes accepted before, still
//! in `<repo>/.ohjaa/knowledge/`, and a person's `ohjaa accept` (at a real
//! terminal) for a repo proposal.
//!
//! Never the real gh and never the network: a stub gh on PATH, a local bare
//! repository as the remote (git's `insteadOf` turns the GitHub URL the
//! checkout names into it), and `GIT_ALLOW_PROTOCOL=file`, so a rule that did
//! not apply fails instead of reaching GitHub. Sandboxed as the CLI tests
//! are, in an emptied environment: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME,
//! OHJAA_OURS and git's global config are all the test's own.

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

/// Never the real gh. Each call's arguments go to `$STUB_GH/calls`.
/// `repo view <owner>/<name>` says the visibility in
/// `$STUB_GH/visibility.<owner>.<name>`, else `$STUB_GH/visibility` (and
/// `main`), or, with `$STUB_GH/signed-out`, that gh is not signed in; `api
/// user` prints `$STUB_GH/user.json` and `api user/repos?…`
/// `$STUB_GH/repos.json`; `pr list` prints `$STUB_GH/open.json`; `pr create`
/// keeps its body (stdin) in `$STUB_GH/body-<n>`, its repo, base and head in
/// `$STUB_GH/created`, makes it the open one and prints its URL. With
/// STUB_PROBE set, `pr create` first runs `$STUB_PROBE apply --undo` (an
/// ohjaa change to the home: it waits while another holds the home's lock)
/// into `$STUB_GH/probe`.
const GH_STUB: &str = r#"#!/bin/sh
d="$STUB_GH"
printf '%s\n' "$*" >> "$d/calls"
case "$1 $2" in
  "repo view")
    if [ -f "$d/signed-out" ]; then
      echo "To get started with GitHub CLI, please run:  gh auth login" >&2
      exit 4
    fi
    own="$d/visibility.$(printf '%s' "$3" | tr '/' '.')"
    if [ -f "$own" ]; then visibility=$(cat "$own"); else visibility=$(cat "$d/visibility"); fi
    printf '{"visibility":"%s","defaultBranchRef":{"name":"main"}}\n' "$visibility"
    exit 0;;
  "api user") cat "$d/user.json"; exit 0;;
  "api user/repos"*) cat "$d/repos.json"; exit 0;;
  "pr list") cat "$d/open.json"; exit 0;;
  "pr create")
    if [ -n "${STUB_PROBE:-}" ]; then "$STUB_PROBE" apply --undo > "$d/probe" 2>&1; fi
    repo=""; head=""; base=""
    while [ $# -gt 0 ]; do
      case "$1" in
        -R|--repo) repo="$2"; shift;;
        --head) head="$2"; shift;;
        --base) base="$2"; shift;;
      esac
      shift
    done
    n=$(( $(cat "$d/created" 2>/dev/null | wc -l) + 41 ))
    cat > "$d/body-$n"
    printf '%s %s %s\n' "$repo" "$base" "$head" >> "$d/created"
    printf '[{"number":%s,"url":"https://github.com/%s/pull/%s","headRefName":"%s","isCrossRepository":false}]\n' \
      "$n" "$repo" "$n" "$head" > "$d/open.json"
    printf 'https://github.com/%s/pull/%s\n' "$repo" "$n"
    exit 0;;
esac
echo "gh stub: not expected: $*" >&2
exit 64
"#;

/// The person, as gh is signed in.
const USER: &str = r#"{"login":"octo-gh","name":"Octo Cat"}"#;

/// The person's repositories, as GitHub lists them to gh: two pages, with
/// a public one. `widget` is also the name of the repository the lessons go
/// to: its own name is no secret there.
const REPOS: &str = concat!(
    r#"[{"name":"secret-thing","full_name":"octo-gh/secret-thing","visibility":"private"},"#,
    r#"{"name":"ote","full_name":"acme/ote","visibility":"internal"},"#,
    r#"{"name":"open-thing","full_name":"acme/open-thing","visibility":"public"}]"#,
    "\n",
    r#"[{"name":"widget","full_name":"acme/widget"},"#,
    r#"{"name":"ohjaa","full_name":"acme/ohjaa","visibility":"private"}]"#,
    "\n"
);

const AGENTS: &str = "# Agent guide\n\nRead this first.\n\n## Languages\n\nRust.\n";

/// Never the real toimija: no sessions anywhere, no context, and `run`
/// ends at once, so `ohjaa run` records a session and its briefing.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions) echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}';;
  context) echo '{}';;
esac
exit 0
"#;

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

/// A git this machine has, by absolute path, for a PATH that holds nothing
/// else.
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
        // The home folder's name is the person's account: `octocat`.
        let home = dir.join("homes/octocat");
        for sub in [
            "config",
            "state",
            "bin",
            "only-git",
            "ours",
            "gh",
            "projects",
            "remote/acme",
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        let remote = dir.join("remote/acme/widget.git");
        fs::write(
            home.join("gitconfig"),
            format!(
                "[user]\n\tname = Octo Cat\n\temail = octo@example.invalid\n\
                 [init]\n\tdefaultBranch = main\n\
                 [url \"file://{}\"]\n\tinsteadOf = https://github.com/acme/widget.git\n",
                remote.display()
            ),
        )
        .unwrap();
        write_executable(&dir.join("bin/gh"), GH_STUB);
        write_executable(&dir.join("bin/toimija"), TOIMIJA_STUB);
        write_executable(
            &dir.join("only-git/git"),
            &format!("#!/bin/sh\nexec {} \"$@\"\n", real_git().display()),
        );
        fs::write(dir.join("gh/visibility"), "PRIVATE").unwrap();
        fs::write(dir.join("gh/user.json"), USER).unwrap();
        fs::write(dir.join("gh/repos.json"), REPOS).unwrap();
        fs::write(dir.join("gh/open.json"), "[]\n").unwrap();
        let world = World {
            checkout: dir.join("projects/widget"),
            dir,
            home,
            remote,
        };
        world.git(
            &world.dir,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                world.remote.to_str().unwrap(),
            ],
        );
        let seed = world.dir.join("seed");
        world.git(
            &world.dir,
            &[
                "clone",
                "-q",
                "https://github.com/acme/widget.git",
                seed.to_str().unwrap(),
            ],
        );
        fs::write(seed.join("AGENTS.md"), AGENTS).unwrap();
        world.git(&seed, &["add", "AGENTS.md"]);
        world.git(&seed, &["commit", "-q", "-m", "agents"]);
        world.git(&seed, &["push", "-q", "origin", "HEAD:main"]);
        world.git(
            &world.dir.join("projects"),
            &[
                "clone",
                "-q",
                "https://github.com/acme/widget.git",
                "widget",
            ],
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

    /// [`World::ohjaa`] with a PATH that has git and no gh at all.
    fn ohjaa_without_gh(&self, cwd: &Path) -> Command {
        let mut command = self.ohjaa(cwd);
        command.env("PATH", self.dir.join("only-git"));
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

    fn publish(&self, args: &[&str]) -> (String, String, Option<i32>) {
        let mut command = self.ohjaa(&self.checkout);
        command.args(["lessons", "publish"]).args(args);
        self.run(&mut command)
    }

    /// A node accepted before ohjaa wrote lessons into AGENTS.md: an
    /// untracked file in the checkout.
    fn legacy(&self, id: &str, kind: &str, body: &str) -> PathBuf {
        let dir = self.checkout.join(".ohjaa/knowledge");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        fs::write(&path, node_text(id, kind, body)).unwrap();
        path
    }

    fn set_visibility(&self, visibility: &str) {
        fs::write(self.dir.join("gh/visibility"), visibility).unwrap();
    }

    fn gh_calls(&self) -> String {
        fs::read_to_string(self.dir.join("gh/calls")).unwrap_or_default()
    }

    fn created(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("gh/created"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn body(&self, number: u32) -> String {
        fs::read_to_string(self.dir.join(format!("gh/body-{number}"))).unwrap()
    }

    fn remote_heads(&self) -> Vec<String> {
        self.git(
            &self.dir,
            &[
                "--git-dir",
                self.remote.to_str().unwrap(),
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads",
            ],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    /// The one branch ohjaa pushed for lessons.
    fn lessons_branch(&self) -> String {
        let heads: Vec<String> = self
            .remote_heads()
            .into_iter()
            .filter(|head| head.starts_with("agents-md-lessons/"))
            .collect();
        assert_eq!(heads.len(), 1, "{heads:?}");
        heads[0].clone()
    }

    fn remote_show(&self, rev: &str) -> String {
        self.git(
            &self.dir,
            &[
                "--git-dir",
                self.remote.to_str().unwrap(),
                "show",
                &format!("{rev}:AGENTS.md"),
            ],
        )
    }

    fn remote_log(&self, rev: &str) -> String {
        self.git(
            &self.dir,
            &[
                "--git-dir",
                self.remote.to_str().unwrap(),
                "log",
                "--format=%H %P%n%B",
                rev,
            ],
        )
    }

    /// The folder ohjaa keeps the checkout's accepted lessons in.
    fn accepted(&self) -> PathBuf {
        let dir = self.dir.join("state/ohjaa/lessons");
        let clones: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|_| panic!("no {}", dir.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.is_dir() && !path.file_name().unwrap().to_string_lossy().starts_with('.')
            })
            .collect();
        assert_eq!(clones.len(), 1, "{clones:?}");
        clones[0].clone()
    }

    fn delivery(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(self.accepted().join("delivery.json")).unwrap())
            .unwrap()
    }

    /// Every file under the sandbox with its bytes, but the stub's own log:
    /// what a run that writes nothing leaves exactly as it was.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, skip: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<PathBuf> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            entries.sort();
            for path in entries {
                if path == skip {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    out.push((path.clone(), Vec::new()));
                    walk(&path, skip, out);
                } else {
                    out.push((path.clone(), fs::read(&path).unwrap_or_default()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.dir, &self.dir.join("gh/calls"), &mut out);
        out
    }

    /// Delivered, merged into main and pulled into the checkout: AGENTS.md
    /// as checked out holds the lines.
    fn merged(&self) {
        let (stdout, stderr, code) = self.publish(&[]);
        assert_eq!(code, Some(0), "{stdout}{stderr}");
        let branch = self.lessons_branch();
        self.git(
            &self.dir,
            &[
                "--git-dir",
                self.remote.to_str().unwrap(),
                "update-ref",
                "refs/heads/main",
                &format!("refs/heads/{branch}"),
            ],
        );
        self.git(&self.checkout, &["pull", "-q", "--ff-only"]);
    }

    /// `ohjaa brief --knowledge-only [--agent <agent>]` in the checkout, with
    /// `env` set: what it printed.
    fn brief(&self, agent: Option<&str>, env: &[(&str, &str)]) -> String {
        let mut brief = self.ohjaa(&self.checkout);
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

    /// [`World::brief`], launched in `dir` (a folder of the checkout).
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

    /// `ohjaa run <agent> --dry-run -- <args>`: the briefing it would give,
    /// which it says on stderr.
    fn run_dry(&self, agent: &str, args: &[&str]) -> String {
        let mut run = self.ohjaa(&self.checkout);
        run.args(["run", agent, "--intent", "work", "--dry-run", "--"])
            .args(args);
        let (stdout, stderr, code) = self.run(&mut run);
        assert_eq!(code, Some(0), "{stdout}{stderr}");
        stderr
    }

    /// Write `text` at `path` in the checkout, its folders made.
    fn put(&self, path: &str, text: &str) {
        let file = self.checkout.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    fn worktrees(&self) -> usize {
        self.git(&self.checkout, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count()
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

#[test]
fn a_dry_run_says_what_it_would_write_and_changes_nothing() {
    let world = World::new("dry");
    world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    world.legacy(
        "widget-tests-first",
        "lesson",
        "Write the failing test first.",
    );
    let before = world.snapshot();
    let (stdout, stderr, code) = world.publish(&["--dry-run"]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("acme/widget"), "{stdout}");
    assert!(stdout.contains("private"), "{stdout}");
    for line in [
        "- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-20)",
        "- <!-- lesson: widget-tests-first --> **lesson**: Write the failing test first. (as of 2026-10-20)",
    ] {
        assert!(stdout.contains(line), "the line it would add: {stdout}");
    }
    assert!(stdout.contains("would open a pull request"), "{stdout}");
    // What each agent would get once it merges: no CLAUDE.md imports it here.
    assert!(
        stdout.contains("once merged, claude: carried in the briefing"),
        "{stdout}"
    );
    assert!(
        stdout.contains("once merged, Codex: left to AGENTS.md"),
        "{stdout}"
    );
    assert!(
        world.snapshot() == before,
        "a dry run writes nothing: no state, no file, no ref, no worktree"
    );
    assert!(!world.dir.join("state/ohjaa").exists());
    let calls = world.gh_calls();
    assert!(!calls.contains("pr create"), "{calls}");
    assert_eq!(world.remote_heads(), vec!["main"]);
}

#[test]
fn publish_opens_one_pull_request_then_moves_each_file_out_of_the_checkout() {
    let world = World::new("publish");
    let rust = world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let first = world.legacy(
        "widget-tests-first",
        "lesson",
        "Write the failing test\nfirst.",
    );
    let head = world.git(&world.checkout, &["rev-parse", "HEAD"]);
    // Where origin pushes to is not where the pull request is: git goes to
    // the repository on GitHub itself.
    world.git(
        &world.checkout,
        &[
            "config",
            "remote.origin.pushurl",
            "file:///nowhere/at/all.git",
        ],
    );
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("opened https://github.com/acme/widget/pull/41"),
        "{stdout}"
    );

    // One pull request, from a branch of its own, onto main.
    let branch = world.lessons_branch();
    assert_eq!(
        world.created(),
        vec![format!("acme/widget main {branch}")],
        "{}",
        world.gh_calls()
    );
    // The section, after everything the file held, kept as it was; one line
    // per node, decisions first, the body on one line.
    let agents = world.remote_show(&branch);
    assert!(agents.starts_with(AGENTS), "{agents}");
    let section = &agents[AGENTS.len()..];
    assert!(
        section.starts_with("\n## Lessons\n\n<!-- This repository's accepted knowledge"),
        "{agents}"
    );
    let decision = section
        .find("- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-20)\n")
        .expect(&agents);
    let lesson = section
        .find("- <!-- lesson: widget-tests-first --> **lesson**: Write the failing test first. (as of 2026-10-20)\n")
        .expect(&agents);
    assert!(decision < lesson, "{agents}");
    assert!(agents.ends_with("(as of 2026-10-20)\n"), "{agents}");
    // A private repository's pull request says why each was accepted.
    let body = world.body(41);
    assert!(body.contains("widget-uses-rust"), "{body}");
    assert!(body.contains("why widget-uses-rust was accepted"), "{body}");
    // The person's main is untouched: only the branch moved.
    assert_eq!(world.remote_show("main"), AGENTS);

    // Nothing is left in the checkout, and nothing is lost.
    assert!(!rust.exists() && !first.exists());
    assert!(!world.checkout.join(".ohjaa").exists());
    assert_eq!(world.git(&world.checkout, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        world.git(&world.checkout, &["status", "--porcelain"]),
        "",
        "the person's checkout is as it was"
    );
    assert_eq!(world.worktrees(), 1, "the worktree it wrote in is gone");
    let accepted = world.accepted();
    assert_eq!(
        fs::read_to_string(accepted.join("widget-uses-rust.md")).unwrap(),
        node_text("widget-uses-rust", "decision", "Widget code is Rust.")
    );
    let delivery = world.delivery();
    let entry = &delivery["nodes"]["widget-uses-rust"];
    assert_eq!(entry["state"], "pr", "{delivery}");
    assert_eq!(entry["url"], "https://github.com/acme/widget/pull/41");
    let moved = &delivery["moved"]["widget-uses-rust"];
    assert_eq!(moved["from"], rust.to_str().unwrap(), "{delivery}");
    assert_eq!(moved["url"], "https://github.com/acme/widget/pull/41");

    // Until it merges, the briefing carries them, marked; `list` says so.
    let mut brief = world.ohjaa(&world.checkout);
    brief.args(["brief", "--knowledge-only"]);
    let (briefing, stderr, code) = world.run(&mut brief);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(briefing.contains("Widget code is Rust."), "{briefing}");
    assert!(
        briefing.contains("pending PR https://github.com/acme/widget/pull/41"),
        "{briefing}"
    );
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, stderr, code) = world.run(&mut list);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        listed.contains("pending PR https://github.com/acme/widget/pull/41"),
        "{listed}"
    );

    // Merged and pulled: the agent reads them in AGENTS.md, so the briefing
    // no longer repeats them, and says where they are.
    world.git(
        &world.dir,
        &[
            "--git-dir",
            world.remote.to_str().unwrap(),
            "update-ref",
            "refs/heads/main",
            &format!("refs/heads/{branch}"),
        ],
    );
    world.git(&world.checkout, &["pull", "-q", "--ff-only"]);
    // For an agent that reads AGENTS.md (Codex).
    let briefing = world.brief(Some("codex"), &[]);
    assert!(!briefing.contains("Widget code is Rust."), "{briefing}");
    assert!(!briefing.contains("failing test"), "{briefing}");
    assert!(briefing.contains("AGENTS.md"), "{briefing}");
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, _, _) = world.run(&mut list);
    assert!(listed.contains("in AGENTS.md"), "{listed}");
    // Only the person's own lessons pull request is added to.
    assert!(
        world
            .gh_calls()
            .lines()
            .filter(|call| call.starts_with("pr list"))
            .all(|call| call.contains("--author @me")),
        "{}",
        world.gh_calls()
    );

    // A line edited by hand is not the node: the briefing carries it again.
    let agents = world.checkout.join("AGENTS.md");
    let edited = fs::read_to_string(&agents)
        .unwrap()
        .replace("Widget code is Rust.", "Widget code is anything.");
    fs::write(&agents, edited).unwrap();
    let briefing = world.brief(Some("codex"), &[]);
    assert!(briefing.contains("Widget code is Rust."), "{briefing}");
    assert!(
        briefing.contains("AGENTS.md holds another line of it"),
        "{briefing}"
    );
    assert!(!briefing.contains("failing test"), "{briefing}");
}

#[test]
fn a_later_publish_adds_to_the_open_pull_request_and_one_with_nothing_new_changes_nothing() {
    let world = World::new("again");
    world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let branch = world.lessons_branch();
    let tip = world.git(
        &world.dir,
        &[
            "--git-dir",
            world.remote.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{branch}"),
        ],
    );

    world.legacy("widget-no-unwrap", "taste", "No unwrap outside tests.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("added to https://github.com/acme/widget/pull/41"),
        "{stdout}"
    );
    assert_eq!(world.created().len(), 1, "no second pull request");
    assert_eq!(world.lessons_branch(), branch, "the same branch");
    let log = world.remote_log(&format!("refs/heads/{branch}"));
    let newest = log.lines().next().unwrap();
    assert!(
        newest.ends_with(tip.trim()),
        "a commit on top of the one before, never a force: {log}"
    );
    let agents = world.remote_show(&branch);
    assert_eq!(agents.matches("<!-- lesson: ").count(), 2, "{agents}");
    assert!(agents.contains("No unwrap outside tests."), "{agents}");
    assert!(!world.checkout.join(".ohjaa").exists());
    assert_eq!(
        world.delivery()["nodes"]["widget-no-unwrap"]["url"],
        "https://github.com/acme/widget/pull/41"
    );

    // Named again, with nothing new: no commit, no pull request.
    let before = world.remote_log(&format!("refs/heads/{branch}"));
    let (stdout, stderr, code) = world.publish(&[world.checkout.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(world.remote_log(&format!("refs/heads/{branch}")), before);
    assert_eq!(world.created().len(), 1);
    assert_eq!(world.worktrees(), 1);
}

#[test]
fn a_public_repository_gets_no_line_that_names_something_private() {
    let world = World::new("public");
    world.set_visibility("PUBLIC");
    world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    world.legacy(
        "widget-own-name",
        "fact",
        "The widget repository builds a remote note: neither is private.",
    );
    let secret = world.legacy(
        "widget-calls-secret",
        "fact",
        "The widget calls Secret-Thing's API.",
    );
    let path = world.legacy(
        "widget-local-path",
        "fact",
        &format!("Builds land in {}/cache.", world.home.display()),
    );
    let person = world.legacy("widget-owner", "fact", "Octocat decides what ships.");

    let (dry, stderr, code) = world.publish(&["--dry-run"]);
    assert_eq!(code, Some(4), "held lines are not delivered: {dry}{stderr}");
    assert!(dry.contains("PUBLIC"), "{dry}");
    assert!(
        dry.contains("`secret-thing`"),
        "what matched is said: {dry}"
    );
    assert!(
        dry.contains(&format!("`{}`", world.home.display())),
        "{dry}"
    );
    assert!(dry.contains("`octocat`"), "{dry}");
    assert!(!world.dir.join("state/ohjaa").exists());

    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("widget-calls-secret"), "{stdout}");
    let branch = world.lessons_branch();
    let agents = world.remote_show(&branch);
    assert!(agents.contains("Widget code is Rust."), "{agents}");
    assert!(
        agents.contains("remote note"),
        "its own name, and a word that only holds a private one, go: {agents}"
    );
    for private in ["Secret-Thing", "secret-thing", "Octocat", "octocat"] {
        assert!(!agents.contains(private), "{private}: {agents}");
    }
    assert!(
        !agents.contains(&world.home.display().to_string()),
        "{agents}"
    );
    let log = world.remote_log(&format!("refs/heads/{branch}"));
    let body = world.body(41);
    // `pr create`'s arguments: its title among them.
    let created = world
        .gh_calls()
        .lines()
        .find(|call| call.starts_with("pr create"))
        .unwrap()
        .to_owned();
    for sent in [&log, &body, &agents, &branch, &created] {
        assert!(!sent.to_lowercase().contains("secret-thing"), "{sent}");
        assert!(!sent.contains("widget-calls-secret"), "{sent}");
        assert!(
            !sent.contains("was accepted"),
            "no origins go to a public repo: {sent}"
        );
        // The tool is private too: what it writes names no tool.
        assert!(!sent.to_lowercase().contains("ohjaa"), "{sent}");
    }
    // What was held stays where it was, and waits; the rest moved.
    assert!(secret.exists() && path.exists() && person.exists());
    assert!(!world
        .checkout
        .join(".ohjaa/knowledge/widget-uses-rust.md")
        .exists());
    // `list` says why it waits.
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, _, _) = world.run(&mut list);
    let held = listed
        .lines()
        .skip_while(|line| !line.contains("widget-calls-secret"))
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    assert!(
        held.contains("held back") && held.contains("`secret-thing`"),
        "{listed}"
    );
}

#[test]
fn a_scoped_repositorys_lessons_are_carried_though_its_agents_md_holds_them() {
    let world = World::new("scoped");
    world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let branch = world.lessons_branch();
    world.git(
        &world.dir,
        &[
            "--git-dir",
            world.remote.to_str().unwrap(),
            "update-ref",
            "refs/heads/main",
            &format!("refs/heads/{branch}"),
        ],
    );
    world.git(&world.checkout, &["pull", "-q", "--ff-only"]);
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
    assert!(briefing.contains("Widget code is Rust."), "{briefing}");
    assert!(briefing.contains("in AGENTS.md too"), "{briefing}");
}

#[test]
fn when_every_line_is_held_nothing_is_pushed() {
    let world = World::new("allheld");
    world.set_visibility("PUBLIC");
    // gh does not list the tool's own repository: it is held back anyway.
    fs::write(world.dir.join("gh/repos.json"), "[{\"name\":\"ote\"}]\n").unwrap();
    let held = world.legacy("widget-calls-ote", "fact", "Widget calls ote.");
    let tool = world.legacy(
        "widget-knowledge-folder",
        "fact",
        "Accepted lessons sat in .ohjaa/knowledge/ before.",
    );
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("`ote`"), "{stdout}");
    assert!(
        stdout.contains("`ohjaa` (the name of this tool, which is private)"),
        "{stdout}"
    );
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert!(!world.gh_calls().contains("pr create"));
    assert!(held.exists() && tool.exists());
    assert_eq!(world.worktrees(), 1);
}

#[test]
fn without_a_remote_or_gh_nothing_moves_and_the_reason_is_said() {
    let world = World::new("nogh");
    let file = world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let mut publish = world.ohjaa_without_gh(&world.checkout);
    publish.args(["lessons", "publish"]);
    let (stdout, stderr, code) = world.run(&mut publish);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("gh"),
        "{stdout}{stderr}"
    );
    assert!(file.exists());
    assert_eq!(world.remote_heads(), vec!["main"]);

    world.git(&world.checkout, &["remote", "remove", "origin"]);
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("no remote named origin"),
        "{stdout}{stderr}"
    );
    assert!(file.exists());
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, _, _) = world.run(&mut list);
    assert!(listed.contains("not yet in AGENTS.md"), "{listed}");
}

fn propose(world: &World, id: &str, body: &str) {
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
        body,
    ]);
    let (stdout, stderr, code) = world.run(&mut propose);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
}

#[test]
fn accepting_a_repo_proposal_at_a_terminal_opens_the_pull_request() {
    let world = World::new("accept");
    propose(&world, "widget-uses-rust", "Widget code is Rust.");
    let mut accept = world.ohjaa(&world.checkout);
    accept.args(["accept", "widget-uses-rust"]);
    let (said, code) = at_terminal(accept);
    assert_eq!(code, Some(0), "{said}");
    assert!(
        said.contains("opened https://github.com/acme/widget/pull/41"),
        "{said}"
    );
    assert!(
        !said.contains("commit it and open a pull request"),
        "ohjaa opened it: {said}"
    );
    assert!(
        !world.checkout.join(".ohjaa").exists(),
        "nothing is written into the checkout"
    );
    let branch = world.lessons_branch();
    let agents = world.remote_show(&branch);
    assert!(
        agents.contains(
            "- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-27)"
        ),
        "{agents}"
    );
    assert!(world.accepted().join("widget-uses-rust.md").is_file());
    assert_eq!(world.delivery()["nodes"]["widget-uses-rust"]["state"], "pr");
    assert_eq!(world.worktrees(), 1);

    // Retired, its line leaves the open pull request's branch.
    let mut retire = world.ohjaa(&world.checkout);
    retire.args(["retire", "widget-uses-rust", "--why", "the widget is gone"]);
    let (said, code) = at_terminal(retire);
    assert_eq!(code, Some(0), "{said}");
    assert!(
        said.contains("added to https://github.com/acme/widget/pull/41"),
        "{said}"
    );
    assert!(!world.remote_show(&branch).contains("widget-uses-rust"));
    assert_eq!(world.created().len(), 1);
}

#[test]
fn an_accept_whose_line_names_something_private_is_refused_and_stays_waiting() {
    let world = World::new("refused");
    world.set_visibility("PUBLIC");
    propose(&world, "widget-calls-secret", "Widget calls secret-thing.");
    let mut accept = world.ohjaa(&world.checkout);
    accept.args(["accept", "widget-calls-secret"]);
    let (said, code) = at_terminal(accept);
    assert_eq!(code, Some(3), "{said}");
    assert!(said.contains("`secret-thing`"), "{said}");
    assert!(said.contains("stays waiting"), "{said}");
    let mut review = world.ohjaa(&world.checkout);
    review.args(["review", "--list"]);
    let (waiting, _, _) = world.run(&mut review);
    assert!(waiting.contains("widget-calls-secret"), "{waiting}");
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert!(!world.gh_calls().contains("pr create"));
    assert!(!world.dir.join("state/ohjaa/lessons").exists());
}

#[test]
fn an_accept_gh_cannot_deliver_stays_accepted_and_undelivered_until_publish() {
    let world = World::new("later");
    fs::write(world.dir.join("gh/signed-out"), "").unwrap();
    propose(&world, "widget-uses-rust", "Widget code is Rust.");
    let mut accept = world.ohjaa(&world.checkout);
    accept.args(["accept", "widget-uses-rust"]);
    let (said, code) = at_terminal(accept);
    assert_eq!(code, Some(0), "the decision stands: {said}");
    assert!(said.contains("not delivered"), "{said}");
    assert!(said.contains("gh auth login"), "{said}");
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert_eq!(
        world.delivery()["nodes"]["widget-uses-rust"]["state"],
        "undelivered"
    );
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, _, _) = world.run(&mut list);
    assert!(listed.contains("not delivered"), "{listed}");
    let mut brief = world.ohjaa(&world.checkout);
    brief.args(["brief", "--knowledge-only"]);
    let (briefing, _, _) = world.run(&mut brief);
    assert!(
        briefing.contains("Widget code is Rust.") && briefing.contains("not delivered"),
        "{briefing}"
    );

    // Signed in again, publish (named nothing) finds it and delivers it.
    fs::remove_file(world.dir.join("gh/signed-out")).unwrap();
    let mut publish = world.ohjaa(&world.dir);
    publish
        .args(["--root", world.dir.join("projects").to_str().unwrap()])
        .args(["lessons", "publish"]);
    let (stdout, stderr, code) = world.run(&mut publish);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("opened https://github.com/acme/widget/pull/41"),
        "{stdout}"
    );
    assert_eq!(world.delivery()["nodes"]["widget-uses-rust"]["state"], "pr");
}

#[test]
fn when_what_ohjaa_itself_writes_names_something_private_nothing_is_sent() {
    let world = World::new("ownwords");
    world.set_visibility("PUBLIC");
    // A private repository named like the section's heading.
    fs::write(
        world.dir.join("gh/repos.json"),
        "[{\"name\":\"lessons\"}]\n",
    )
    .unwrap();
    let file = world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("`lessons`"), "{stdout}");
    assert!(stdout.contains("nothing was sent"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert!(!world.gh_calls().contains("pr create"));
    assert!(file.exists());
    assert_eq!(world.worktrees(), 1);
}

#[test]
fn an_agents_md_that_cannot_be_read_whole_is_never_written_over() {
    let world = World::new("notutf8");
    // main's AGENTS.md, with a byte that is no UTF-8.
    let seed = world.dir.join("seed");
    fs::write(
        seed.join("AGENTS.md"),
        b"# Agent guide\n\nLatin-1: caf\xe9\n",
    )
    .unwrap();
    world.git(&seed, &["commit", "-q", "-am", "latin-1"]);
    world.git(&seed, &["push", "-q", "origin", "HEAD:main"]);
    let file = world.legacy("widget-uses-rust", "decision", "Widget code is Rust.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("not UTF-8"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"], "nothing is pushed");
    assert!(!world.gh_calls().contains("pr create"));
    assert!(file.exists());
    assert_eq!(world.worktrees(), 1);
}

#[test]
fn retiring_an_untracked_node_at_a_terminal_takes_it_out_of_the_checkout() {
    let world = World::new("retire-untracked");
    world.legacy("widget-old-rule", "decision", "An old rule.");
    let mut retire = world.ohjaa(&world.checkout);
    retire.args(["retire", "widget-old-rule", "--why", "no longer"]);
    let (said, code) = at_terminal(retire);
    assert_eq!(code, Some(0), "{said}");
    assert!(
        !world.checkout.join(".ohjaa").exists(),
        "nothing is written into the checkout: {said}"
    );
    let node = fs::read_to_string(world.accepted().join("widget-old-rule.md")).unwrap();
    assert!(node.contains("status = \"retired\""), "{node}");
    assert!(node.contains("retired: no longer"), "{node}");
    assert!(
        !world.gh_calls().contains("pr create"),
        "a retired node that never had a line opens nothing"
    );
}

/// The node every briefing test delivers, merges and pulls.
const RUST: &str = "Widget code is Rust.";

fn merged_world(tag: &str) -> World {
    let world = World::new(tag);
    world.legacy("widget-uses-rust", "decision", RUST);
    world.merged();
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

#[test]
fn claude_code_is_left_the_lessons_only_when_claude_md_imports_agents_md() {
    let world = merged_world("claude");
    // No CLAUDE.md: Claude Code 2.1.311 falls back to AGENTS.md only where no
    // CLAUDE.md is found at the root or above, which ohjaa does not rely on.
    let briefing = world.brief(Some("claude"), &[]);
    assert!(carried(&briefing, "@AGENTS.md"), "{briefing}");
    // Named in prose, or imported inside a fence: not read.
    world.put("CLAUDE.md", "Read AGENTS.md before you start.\n");
    let briefing = world.brief(Some("claude"), &[]);
    assert!(carried(&briefing, "does not import"), "{briefing}");
    world.put("CLAUDE.md", "# Notes\n\n```\n@AGENTS.md\n```\n");
    let briefing = world.brief(Some("claude"), &[]);
    assert!(carried(&briefing, "does not import"), "{briefing}");
    // Imported: left to it.
    world.put("CLAUDE.md", "# Notes\n\n@AGENTS.md\n");
    let briefing = world.brief(Some("claude"), &[]);
    assert!(left(&briefing), "{briefing}");
    assert!(briefing.contains("Claude Code"), "{briefing}");
    // But no project instructions load at all.
    let briefing = world.brief(Some("claude"), &[("CLAUDE_CODE_SIMPLE", "1")]);
    assert!(carried(&briefing, "CLAUDE_CODE_SIMPLE"), "{briefing}");
    let briefing = world.run_dry("claude", &["--bare"]);
    assert!(carried(&briefing, "--bare"), "{briefing}");
    let briefing = world.run_dry("claude", &["--setting-sources", "user"]);
    assert!(carried(&briefing, "project"), "{briefing}");
    // Imported from .claude/CLAUDE.md, relative to it.
    fs::remove_file(world.checkout.join("CLAUDE.md")).unwrap();
    world.put(".claude/CLAUDE.md", "@../AGENTS.md\n");
    assert!(left(&world.brief(Some("claude"), &[])));
    fs::remove_file(world.checkout.join(".claude/CLAUDE.md")).unwrap();
    // The person's setting loads AGENTS.md beside CLAUDE.md.
    fs::create_dir_all(world.home.join(".claude")).unwrap();
    fs::write(
        world.home.join(".claude/settings.json"),
        r#"{"pluginConfigs":{"cc-plugin-agents-md@builtin":{"options":{"instructionFiles":"claude-md-and-agents-md"}}}}"#,
    )
    .unwrap();
    let briefing = world.brief(Some("claude"), &[]);
    assert!(left(&briefing), "{briefing}");
}

#[test]
fn codex_is_left_the_lessons_unless_an_override_or_its_byte_cap_hides_them() {
    let world = merged_world("codex");
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
fn an_agent_not_known_to_read_agents_md_gets_the_lessons_carried() {
    let world = merged_world("unknown");
    assert!(carried(&world.brief(None, &[]), "no agent"));
    assert!(carried(&world.brief(Some("gemini"), &[]), "gemini"));
    assert!(left(&world.brief(Some("pi"), &[])));
    assert!(carried(&world.run_dry("pi", &["-nc"]), "-nc"));
    assert!(left(&world.brief(Some("opencode"), &[])));
    assert!(carried(
        &world.brief(
            Some("opencode"),
            &[("OPENCODE_DISABLE_PROJECT_CONFIG", "1")]
        ),
        "OPENCODE_DISABLE_PROJECT_CONFIG"
    ));
}

#[test]
fn the_session_log_says_via_agents_md_only_when_the_agent_reads_it() {
    let world = merged_world("vialog");
    let via = |agent: &str| -> serde_json::Value {
        let mut run = world.ohjaa(&world.checkout);
        run.args(["run", agent, "--intent", "work"]);
        let (stdout, stderr, code) = world.run(&mut run);
        assert_eq!(code, Some(0), "{stdout}{stderr}");
        let mut export = world.ohjaa(&world.checkout);
        export.args(["usage", "export"]);
        let (export, _, _) = world.run(&mut export);
        let export: serde_json::Value = serde_json::from_str(&export).unwrap();
        let sessions = export["sessions"].as_array().unwrap();
        let node = sessions.last().unwrap()["delivered"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "widget-uses-rust")
            .cloned()
            .unwrap_or_else(|| panic!("not delivered: {export}"));
        node["via"].clone()
    };
    let agents = world.checkout.join("AGENTS.md");
    assert_eq!(via("codex"), agents.to_str().unwrap());
    assert_eq!(
        via("claude"),
        serde_json::Value::Null,
        "carried by the briefing"
    );
}

#[test]
fn a_retired_or_superseded_line_still_in_agents_md_is_said_not_to_be_followed() {
    let world = merged_world("stale");
    let mut retire = world.ohjaa(&world.checkout);
    retire.args(["retire", "widget-uses-rust", "--why", "the widget is gone"]);
    let (said, code) = at_terminal(retire);
    assert_eq!(code, Some(0), "{said}");
    // The removal waits in a pull request: AGENTS.md still holds the line.
    assert!(fs::read_to_string(world.checkout.join("AGENTS.md"))
        .unwrap()
        .contains("widget-uses-rust"));
    let briefing = world.brief(Some("codex"), &[]);
    assert!(
        briefing.contains("AGENTS.md still says `widget-uses-rust`"),
        "{briefing}"
    );
    assert!(briefing.contains("retired"), "{briefing}");
    assert!(briefing.contains("do not follow it"), "{briefing}");
    assert!(
        briefing.contains("https://github.com/acme/widget/pull/4"),
        "{briefing}"
    );
    // So does a claude session, which reads AGENTS.md only by chance.
    let briefing = world.brief(Some("claude"), &[]);
    assert!(briefing.contains("do not follow it"), "{briefing}");
}

#[test]
fn a_superseded_line_still_in_agents_md_names_its_successor() {
    let world = merged_world("superseded");
    let mut propose = world.ohjaa(&world.checkout);
    propose.args([
        "propose",
        "--scope",
        "repo",
        "--id",
        "widget-uses-go",
        "--kind",
        "decision",
        "--origin",
        "a person said so",
        "--supersedes",
        "widget-uses-rust",
        "Widget code is Go.",
    ]);
    let (stdout, stderr, code) = world.run(&mut propose);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let mut accept = world.ohjaa(&world.checkout);
    accept.args(["accept", "widget-uses-go"]);
    let (said, code) = at_terminal(accept);
    assert_eq!(code, Some(0), "{said}");
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
}

#[test]
fn an_empty_or_partial_list_of_private_repositories_sends_nothing_public() {
    let world = World::new("emptylist");
    world.set_visibility("PUBLIC");
    let file = world.legacy("widget-calls-secret", "fact", "Widget calls secret-thing.");
    fs::write(world.dir.join("gh/repos.json"), "[]\n").unwrap();
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("lists none"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"], "nothing is pushed");
    assert!(file.exists());

    // A list without a private repository found under the roots.
    fs::write(world.dir.join("gh/repos.json"), REPOS).unwrap();
    let other = world.dir.join("projects/otherthing");
    fs::create_dir_all(&other).unwrap();
    world.git(&other, &["init", "-q"]);
    world.git(
        &other,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/otherthing.git",
        ],
    );
    fs::write(world.dir.join("gh/visibility.acme.otherthing"), "PRIVATE").unwrap();
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("acme/otherthing"), "{stdout}");
    assert!(stdout.contains("not complete"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert!(!world.gh_calls().contains("pr create"));
}

#[test]
fn a_repository_under_the_roots_of_unknown_visibility_counts_as_private() {
    let world = World::new("unknownvis");
    world.set_visibility("PUBLIC");
    // No remote at all: gh cannot say it is public.
    let hidden = world.dir.join("projects/secret-proj");
    fs::create_dir_all(&hidden).unwrap();
    world.git(&hidden, &["init", "-q"]);
    let held = world.legacy(
        "widget-uses-proj",
        "fact",
        "Widget reads secret-proj's output.",
    );
    world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("`secret-proj`"), "{stdout}");
    assert!(held.exists());
    let agents = world.remote_show(&world.lessons_branch());
    assert!(
        agents.contains(RUST) && !agents.contains("secret-proj"),
        "{agents}"
    );
}

#[test]
fn a_node_in_both_the_checkout_and_ohjaas_state_is_ohjaas() {
    let world = World::new("both");
    let file = world.legacy("widget-uses-rust", "decision", RUST);
    let text = fs::read_to_string(&file).unwrap();
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let mut retire = world.ohjaa(&world.checkout);
    retire.args(["retire", "widget-uses-rust", "--why", "gone"]);
    let (said, code) = at_terminal(retire);
    assert_eq!(code, Some(0), "{said}");
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
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(
        code,
        Some(4),
        "a file left is not delivered: {stdout}{stderr}"
    );
    assert!(stdout.contains("left"), "{stdout}");
}

#[test]
fn a_failing_pre_push_hook_is_said_in_its_own_words() {
    let world = World::new("prepush");
    write_executable(
        &world.checkout.join(".git/hooks/pre-push"),
        "#!/bin/sh\necho 'pre-push says no: run the gate first' >&2\nexit 1\n",
    );
    let file = world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(
        stdout.contains("pre-push says no: run the gate first"),
        "{stdout}"
    );
    assert!(file.exists());
}

#[test]
fn a_commit_that_holds_more_than_agents_md_is_never_pushed() {
    let world = World::new("extra");
    write_executable(
        &world.checkout.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho stray > stray.txt\ngit add stray.txt\n",
    );
    let file = world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("more than AGENTS.md"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"], "nothing is pushed");
    assert!(file.exists());
}

#[test]
fn an_accept_delivers_without_holding_the_homes_lock() {
    let world = World::new("homelock");
    propose(&world, "widget-uses-rust", RUST);
    let mut accept = world.ohjaa(&world.checkout);
    accept
        .args(["accept", "widget-uses-rust"])
        .env("STUB_PROBE", env!("CARGO_BIN_EXE_ohjaa"));
    let (said, code) = at_terminal(accept);
    assert_eq!(code, Some(0), "{said}");
    let probe = fs::read_to_string(world.dir.join("gh/probe")).unwrap();
    assert!(
        !probe.contains("waiting for another ohjaa"),
        "another change to the home waited on the delivery: {probe}"
    );
    assert!(
        said.contains("opened https://github.com/acme/widget/pull/41"),
        "{said}"
    );
}

#[test]
fn claude_launched_in_a_subfolder_skips_the_roots_import_unless_its_project_approved_them() {
    let world = merged_world("subfolder");
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
    let world = merged_world("ignored");
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

#[test]
fn a_hook_that_changes_the_commit_after_the_check_stops_the_push() {
    let world = World::new("commitmsg");
    world.set_visibility("PUBLIC");
    // A commit-msg hook that adds a trailer naming a private path.
    write_executable(
        &world.checkout.join(".git/hooks/commit-msg"),
        "#!/bin/sh\nprintf '\\nSeen-in: %s/projects/secret-thing\\n' \"$HOME\" >> \"$1\"\n",
    );
    let file = world.legacy("widget-uses-rust", "decision", RUST);
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("changed the commit message"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"], "nothing is pushed");
    assert!(!world.gh_calls().contains("pr create"));
    assert!(file.exists());

    // A post-commit hook that makes a second commit.
    fs::remove_file(world.checkout.join(".git/hooks/commit-msg")).unwrap();
    write_executable(
        &world.checkout.join(".git/hooks/post-commit"),
        "#!/bin/sh\n[ -n \"$IN_HOOK\" ] || IN_HOOK=1 git commit -q --allow-empty -m extra\n",
    );
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("more than one commit"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"]);
}
