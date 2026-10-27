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
/// `repo view` says the visibility in `$STUB_GH/visibility` (and `main`),
/// or, with `$STUB_GH/signed-out`, that gh is not signed in; `api user`
/// prints `$STUB_GH/user.json` and `api user/repos?…` `$STUB_GH/repos.json`;
/// `pr list` prints `$STUB_GH/open.json`;
/// `pr create` keeps its body (stdin) in `$STUB_GH/body-<n>`, its repo, base
/// and head in `$STUB_GH/created`, makes it the open one and prints its URL.
const GH_STUB: &str = r#"#!/bin/sh
d="$STUB_GH"
printf '%s\n' "$*" >> "$d/calls"
case "$1 $2" in
  "repo view")
    if [ -f "$d/signed-out" ]; then
      echo "To get started with GitHub CLI, please run:  gh auth login" >&2
      exit 4
    fi
    printf '{"visibility":"%s","defaultBranchRef":{"name":"main"}}\n' "$(cat "$d/visibility")"
    exit 0;;
  "api user") cat "$d/user.json"; exit 0;;
  "api user/repos"*) cat "$d/repos.json"; exit 0;;
  "pr list") cat "$d/open.json"; exit 0;;
  "pr create")
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

/// The person's private repositories, as GitHub lists them to gh: two
/// pages. `widget` is also the name of the repository the lessons go to:
/// its own name is no secret there.
const REPOS: &str = "[{\"name\":\"secret-thing\"},{\"name\":\"ote\"}]\n[{\"name\":\"widget\"},{\"name\":\"ohjaa\"}]\n";

const AGENTS: &str = "# Agent guide\n\nRead this first.\n\n## Languages\n\nRust.\n";

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
    let mut brief = world.ohjaa(&world.checkout);
    brief.args(["brief", "--knowledge-only"]);
    let (briefing, stderr, code) = world.run(&mut brief);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!briefing.contains("Widget code is Rust."), "{briefing}");
    assert!(!briefing.contains("failing test"), "{briefing}");
    assert!(briefing.contains("AGENTS.md"), "{briefing}");
    let mut list = world.ohjaa(&world.checkout);
    list.arg("list");
    let (listed, _, _) = world.run(&mut list);
    assert!(listed.contains("in AGENTS.md"), "{listed}");
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
    let held = world.legacy("widget-calls-ote", "fact", "Widget calls ote.");
    let (stdout, stderr, code) = world.publish(&[]);
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stdout.contains("`ote`"), "{stdout}");
    assert_eq!(world.remote_heads(), vec!["main"]);
    assert!(!world.gh_calls().contains("pr create"));
    assert!(held.exists());
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
