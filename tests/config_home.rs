//! The workflow home from outside the repositories: `apply`, `doctor`,
//! `brief`, `collect` and `shell-init`, run as the real binary against real
//! git.
//!
//! Every command runs with an emptied environment: HOME, XDG_CONFIG_HOME,
//! XDG_STATE_HOME, CARGO_HOME and TMPDIR inside the sandbox,
//! GIT_CONFIG_GLOBAL at a sandbox file, GIT_CONFIG_NOSYSTEM=1, no ZDOTDIR, and
//! a PATH of stubs (toimija, claude, codex) plus the system directories.
//! `apply` writes git's global excludes and ~/.zshrc, so nothing here may reach
//! the person's real ones, nor run a real agent or tool.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Never the real toimija: no live sessions (unless a test writes a
/// checkout.s `sessions --json` into `$SANDBOX/toimija-sessions/<its folder
/// name>`).
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions)
    # A checkout's sessions: $SANDBOX/toimija-sessions/<its folder name>, else none.
    listed="$SANDBOX/toimija-sessions/$(basename "$PWD")"
    if [ -f "$listed" ]; then cat "$listed"
    else echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}'; fi;;
  context) echo '{}';;
esac
"#;

const CLAUDE_STUB: &str = "#!/bin/sh\necho \"direct $*\"\n";
const CODEX_STUB: &str = "#!/bin/sh\necho \"direct-codex $*\"\n";

const OLD_ZSHRC: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
    # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
    [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
    alias ll='ls -l'\n";

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
    copy_executable(&source, path);
    fs::remove_file(&source).unwrap();
}

/// `from`'s bytes at `to`, executable, written by `cp` (see
/// [`write_executable`]).
fn copy_executable(from: &Path, to: &Path) {
    let copied = Command::new("cp").arg(from).arg(to).status().unwrap();
    assert!(copied.success(), "cp {} {}", from.display(), to.display());
    fs::set_permissions(to, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The real git, as found on the test's own PATH.
fn system_git() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|git| git.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"))
}

/// The lines a child writes to `pipe`, as it writes them.
fn lines_of(pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(pipe)).map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

/// The lines up to the first holding `wanted`, and whether it came: the
/// child ended, or a minute passed, without it.
fn until_line(lines: &std::sync::mpsc::Receiver<String>, wanted: &str) -> (bool, Vec<String>) {
    let mut seen = Vec::new();
    while let Ok(line) = lines.recv_timeout(std::time::Duration::from_secs(60)) {
        let found = line.contains(wanted);
        seen.push(line);
        if found {
            return (true, seen);
        }
    }
    (false, seen)
}

/// The home's lock, held by the test as another ohjaa's change would hold it.
fn hold_the_homes_lock(home: &Path) -> fs::File {
    use std::os::unix::io::AsRawFd as _;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.join(".git/ohjaa.lock"))
        .unwrap();
    // SAFETY: flock(2) on a descriptor the test owns.
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(taken, 0, "the home's lock is free");
    file
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The names of the files in a directory, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

struct Sandbox {
    dir: PathBuf,
    path: String,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        for sub in [
            "bin", "home", "config", "state", "cargo", "tmp", "projects", "remotes",
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let bin = dir.join("bin");
        write_executable(&bin.join("claude"), CLAUDE_STUB);
        write_executable(&bin.join("toimija"), TOIMIJA_STUB);
        write_executable(&bin.join("codex"), CODEX_STUB);
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ohjaa"), bin.join("ohjaa")).unwrap();
        // Only the stubs and the system: no real tool, cargo, agent or ohjaa.
        let git_dir = system_git().parent().unwrap().to_path_buf();
        let path = format!(
            "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
            bin.display(),
            git_dir.display()
        );
        Sandbox { dir, path }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config").join("ohjaa")
    }

    /// The launcher line apply writes: the sandbox's ohjaa, by its path.
    fn launch_line(&self) -> String {
        let ohjaa = self.dir.join("bin/ohjaa");
        format!(
            "[[ -x '{0}' ]] && eval \"$('{0}' shell-init zsh)\"",
            ohjaa.display()
        )
    }

    fn projects(&self) -> String {
        self.dir.join("projects").to_string_lossy().into_owned()
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CARGO_HOME", self.dir.join("cargo"))
            .env("TMPDIR", self.dir.join("tmp"))
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env("GIT_AUTHOR_NAME", "Ohjaa Test")
            .env("GIT_AUTHOR_EMAIL", "ohjaa@example.invalid")
            .env("GIT_COMMITTER_NAME", "Ohjaa Test")
            .env("GIT_COMMITTER_EMAIL", "ohjaa@example.invalid")
            .env("SANDBOX", &self.dir);
        command
    }

    fn ohjaa_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("ohjaa", cwd).args(args).output().unwrap()
    }

    fn ohjaa(&self, args: &[&str]) -> Output {
        self.ohjaa_in(&self.dir, args)
    }

    fn git_out(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("git", cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_out(cwd, args);
        let (stdout, stderr) = text(&output);
        assert!(
            output.status.success(),
            "git {args:?} in {}: {stderr}",
            cwd.display()
        );
        stdout.trim().to_owned()
    }

    /// `projects/<name>`, with one commit holding `files`, pushed to a bare
    /// `remotes/<owner>/<name>.git` as origin's main, and origin/HEAD set.
    fn repo(&self, owner: &str, name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let bare = self
            .dir
            .join("remotes")
            .join(owner)
            .join(format!("{name}.git"));
        fs::create_dir_all(&bare).unwrap();
        self.git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        let repo = self.dir.join("projects").join(name);
        fs::create_dir_all(&repo).unwrap();
        self.git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README"), "hello\n").unwrap();
        for (file, contents) in files {
            let path = repo.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        self.git(&repo, &["add", "-A"]);
        self.git(&repo, &["commit", "-q", "-m", "init"]);
        self.git(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
        self.git(&repo, &["push", "-q", "-u", "origin", "main"]);
        self.git(&repo, &["remote", "set-head", "origin", "main"]);
        (repo, bare)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn apply(&self) {
        let output = self.ohjaa(&["apply"]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }

    /// An agent's own instruction file, `home`-relative, empty: the next
    /// apply writes the knowledge block into it, one change to the home.
    fn agent_file(&self, path: &str) {
        let file = self.home().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "").unwrap();
    }

    /// The `start` events of the git processes `command` starts, from git's
    /// own trace2 event stream, with its output. A missing or empty stream
    /// fails the test: a count of nothing must never pass for zero.
    fn git_starts(&self, command: &mut Command) -> (Vec<String>, Output) {
        let (events, output) = self.git_events(command);
        let starts = events
            .into_iter()
            .filter(|line| line.contains("\"event\":\"start\""))
            .collect();
        (starts, output)
    }

    /// Every trace2 event of the git processes `command` starts; a missing
    /// or empty stream fails the test.
    fn git_events(&self, command: &mut Command) -> (Vec<String>, Output) {
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let trace = self.dir.join(format!("trace2-{n}.json"));
        command.env("GIT_TRACE2_EVENT", &trace);
        let output = command.output().unwrap();
        let events = fs::read_to_string(&trace).unwrap_or_default();
        assert!(
            !events.trim().is_empty(),
            "no trace2 events in {}: git never ran under {command:?}\n{}",
            trace.display(),
            text(&output).1
        );
        (events.lines().map(str::to_owned).collect(), output)
    }
}

/// Whether one of the `start` events is git's own `git … <word> …` (trace2
/// names git by the path it ran from).
fn started(starts: &[String], word: &str) -> bool {
    let word = format!("\"{word}\"");
    starts.iter().any(|line| {
        line.split_once("\"argv\":[")
            .and_then(|(_, argv)| argv.split(',').next())
            .is_some_and(|program| program.ends_with("git\""))
            && line.contains(&word)
    })
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn apply_dry_run_says_every_change_and_makes_none() {
    let sandbox = Sandbox::new("dry");
    fs::write(sandbox.home().join(".zshrc"), OLD_ZSHRC).unwrap();
    let output = sandbox.ohjaa(&["apply", "--dry-run"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for said in [
        format!(
            "would add .sykli/ .teko/ .ohjaa/ to {}",
            sandbox.dir.join("config/git/ignore").display()
        ),
        format!("would change {}", sandbox.home().join(".zshrc").display()),
        "  - [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh".to_owned(),
        format!("  + {}", sandbox.launch_line()),
        "dry run; nothing was changed".to_owned(),
    ] {
        assert!(stdout.contains(&said), "missing {said:?} in:\n{stdout}");
    }
    assert!(!sandbox.config().exists(), "nothing written in the home");
    assert!(
        !sandbox.dir.join("gitconfig").exists(),
        "global config untouched"
    );
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        OLD_ZSHRC
    );
}

#[test]
fn undo_puts_back_exactly_what_apply_changed() {
    let sandbox = Sandbox::new("undo");
    let gitconfig = "[user]\n\tname = Someone\n[alias]\n\tst = status\n";
    fs::write(sandbox.dir.join("gitconfig"), gitconfig).unwrap();
    // No final newline in either: undo keeps it so.
    let zshrc = OLD_ZSHRC.trim_end();
    fs::write(sandbox.home().join(".zshrc"), zshrc).unwrap();
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp").unwrap();
    sandbox.apply();
    assert_eq!(
        sandbox.read("gitconfig"),
        gitconfig,
        "git's config untouched"
    );
    let record = sandbox.config().join("applied.toml");

    let output = sandbox.ohjaa(&["apply", "--undo", "--dry-run"]);
    let (stdout, _) = text(&output);
    for said in [
        "would remove the \"ohjaa apply\" block from ".to_owned(),
        "would change ".to_owned(),
        "  + [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh".to_owned(),
        format!("would remove {}", record.display()),
        "dry run; nothing was changed".to_owned(),
    ] {
        assert!(stdout.contains(&said), "missing {said:?} in:\n{stdout}");
    }
    assert!(record.exists(), "a dry run undoes nothing");

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(sandbox.read("gitconfig"), gitconfig);
    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        zshrc
    );
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp");
    assert_eq!(
        names_in(&sandbox.config()),
        [".git"],
        "the home's history kept"
    );
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert!(text(&output).0.contains("nothing to undo"));
}

#[test]
fn undo_removes_the_files_apply_created() {
    let sandbox = Sandbox::new("undo-created");
    sandbox.apply();
    assert!(sandbox.home().join(".zshrc").exists());
    assert!(sandbox.dir.join("config/git/ignore").exists());
    assert!(
        !sandbox.dir.join("gitconfig").exists(),
        "apply creates no global git config"
    );
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(!sandbox.home().join(".zshrc").exists());
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    assert_eq!(
        names_in(&sandbox.config()),
        [".git"],
        "only the home's history is kept"
    );
}

/// What the record holds that this undo does not take back (an earlier
/// apply's, say) stays in it, never dropped unread, and undo says it is not
/// all done (exit 4).
#[test]
fn undo_keeps_what_it_does_not_take_back_and_says_so() {
    let sandbox = Sandbox::new("undo-unknown");
    sandbox.apply();
    let record = sandbox.config().join("applied.toml");
    let mut written = fs::read_to_string(&record).unwrap();
    written.push_str("include = \"/somewhere/git/config\"\n");
    fs::write(&record, written).unwrap();
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    assert!(
        stdout.contains("not all undone") && stdout.contains("(include)"),
        "{stdout}"
    );
    let kept = fs::read_to_string(&record).unwrap();
    assert!(
        kept.contains("include = \"/somewhere/git/config\""),
        "{kept}"
    );
    assert!(
        !kept.contains("zshrc"),
        "what undo took back is gone: {kept}"
    );
    assert!(!sandbox.home().join(".zshrc").exists());
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("nothing to undo") && stdout.contains("not all undone"),
        "{stdout}"
    );
}

/// `apply --root` writes down where the repositories are, which collect and
/// lessons read wherever they run from; without it and without the file,
/// apply says how. Undo keeps the roots.
#[test]
fn apply_root_writes_the_roots_down() {
    let sandbox = Sandbox::new("roots");
    let output = sandbox.ohjaa(&["apply", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(stderr.contains("ohjaa apply --root <dir>"), "{stderr}");
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let roots = sandbox.read("config/ohjaa/roots");
    assert!(
        roots.lines().any(|line| line == sandbox.projects()),
        "{roots}"
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("config/ohjaa/roots"), roots);
}
/// Each change is recorded before it is made: an apply that fails midway
/// can still be undone.
#[test]
fn an_apply_that_fails_midway_can_be_undone() {
    let sandbox = Sandbox::new("midway");
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    // ~/.zshrc's folder cannot be written: apply fails there, after the
    // ignore block is in git's global excludes.
    let home = sandbox.home();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o555)).unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(output.status.code(), Some(0), "{}", text(&output).0);
    assert!(sandbox.read("config/git/ignore").contains(".sykli/"));
    assert!(sandbox
        .read("config/ohjaa/applied.toml")
        .contains("ignore_file = "));
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp\n");
}

#[test]
fn apply_writes_the_ignore_block_where_core_excludes_file_points() {
    let sandbox = Sandbox::new("excludes");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "core.excludesFile", "~/my-ignore"],
    );
    sandbox.apply();
    assert!(fs::read_to_string(sandbox.home().join("my-ignore"))
        .unwrap()
        .contains(".sykli/"));
    assert!(!sandbox.dir.join("config/git/ignore").exists());
}

/// A current node of the home's own knowledge, as a person's accept leaves it.
fn knowledge_node(dir: &Path, id: &str, kind: &str, body: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(format!("{id}.md")),
        format!(
            "+++\nid = \"{id}\"\nkind = \"{kind}\"\nstatus = \"current\"\n\
             as_of = 2026-10-27\norigin = [\"the origin, never in the block\"]\n+++\n{body}\n"
        ),
    )
    .unwrap();
}

impl Sandbox {
    /// The home's history, newest first: each commit's subject and body.
    fn history(&self) -> Vec<String> {
        let output = self.git_out(&self.config(), &["log", "--format=%B%x00"]);
        if !output.status.success() {
            return Vec::new();
        }
        text(&output)
            .0
            .split('\0')
            .map(|commit| commit.trim().to_owned())
            .filter(|commit| !commit.is_empty())
            .collect()
    }
}

/// The team's knowledge and the person's (ours, mine, machine) reach each
/// agent's own global instruction file, in a block apply writes, rewrites where it is
/// and `--undo` takes out, leaving every hand-written byte as it was. No
/// file is created, and none inside a repository is written. What apply
/// changes in the home is a commit in its history each.
#[test]
fn apply_writes_the_knowledge_block_into_each_agents_own_file_and_undo_takes_it_out() {
    let sandbox = Sandbox::new("instructions");
    let claude = sandbox.home().join(".claude/CLAUDE.md");
    let codex = sandbox.home().join(".codex/AGENTS.md");
    // Hand-written, as the person's are; one ends without a newline.
    let claude_text = "# Global instructions\n\nCode is Rust, or Elixir/Erlang.";
    let codex_text = "# Codex\n\nKeep me.\n";
    for (file, text) in [(&claude, claude_text), (&codex, codex_text)] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    // pi is set up and has no instruction file; opencode's is in a repository.
    fs::create_dir_all(sandbox.home().join(".pi/agent")).unwrap();
    let opencode = sandbox.dir.join("config/opencode");
    fs::create_dir_all(&opencode).unwrap();
    sandbox.git(&opencode, &["init", "-q"]);
    fs::write(opencode.join("AGENTS.md"), "# opencode\n").unwrap();
    let mine = sandbox.config().join("mine");
    knowledge_node(
        &mine,
        "code-is-rust",
        "decision",
        "Code is Rust\nor Elixir.",
    );
    knowledge_node(&mine, "answer-first", "taste", "Answer first.");
    knowledge_node(
        &sandbox.config().join("machine"),
        "lima",
        "fact",
        "Lima is here.",
    );
    // The team's knowledge goes there too: every agent reads its own file
    // wherever it starts, which a workplace file above the repositories is not.
    let ours = sandbox.dir.join("ours");
    fs::write(
        sandbox.config().join("ours"),
        format!("{}\n", ours.display()),
    )
    .unwrap();
    knowledge_node(
        &ours.join("knowledge"),
        "team-rule",
        "decision",
        "Team rule.",
    );
    let lines = "- decision: Team rule.\n- decision: Code is Rust or Elixir.\n\
                 - taste: Answer first.\n- fact: Lima is here.\n<!-- ohjaa:knowledge end -->\n";

    let output = sandbox.ohjaa(&["apply", "--dry-run"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for file in [&claude, &codex] {
        assert!(
            stdout.contains(&format!(
                "would write the knowledge block (4 nodes) into {}",
                file.display()
            )),
            "{stdout}"
        );
    }
    assert_eq!(fs::read_to_string(&claude).unwrap(), claude_text);
    assert!(sandbox.history().is_empty(), "a dry run commits nothing");

    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let written = fs::read_to_string(&claude).unwrap();
    assert!(
        written.starts_with(&format!(
            "{claude_text}\n\n<!-- ohjaa:knowledge begin: mine and machine"
        )) && written.ends_with(lines),
        "{written}"
    );
    assert!(!written.contains("the origin"), "{written}");
    let written = fs::read_to_string(&codex).unwrap();
    assert!(
        written.starts_with(&format!("{codex_text}\n<!-- ohjaa:knowledge begin"))
            && written.ends_with(lines),
        "{written}"
    );
    assert!(
        stdout.contains(&format!(
            "pi reads {}, which does not exist; apply creates no instruction file",
            sandbox.home().join(".pi/agent/AGENTS.md").display()
        )),
        "{stdout}"
    );
    assert!(!sandbox.home().join(".pi/agent/AGENTS.md").exists());
    assert!(
        stdout.contains(&format!(
            "{} (opencode's own instructions) is inside the repository {}",
            opencode.join("AGENTS.md").display(),
            opencode.display()
        )),
        "{stdout}"
    );
    assert_eq!(
        fs::read_to_string(opencode.join("AGENTS.md")).unwrap(),
        "# opencode\n"
    );
    let history = sandbox.history();
    assert_eq!(history.len(), 1, "{history:#?}");
    assert!(
        history[0].starts_with("apply: ")
            && history[0].contains(&format!(
                "\nwrote the knowledge block (4 nodes) into {}",
                claude.display()
            )),
        "{history:#?}"
    );
    assert!(
        stdout.contains(&format!(
            "ohjaa apply: committed in {}: apply: ",
            sandbox.config().display()
        )),
        "{stdout}"
    );

    // Applied again: nothing to change, and no commit.
    let output = sandbox.ohjaa(&["apply"]);
    assert!(text(&output).0.contains("nothing to change"), "{output:?}");
    assert_eq!(sandbox.history().len(), 1);

    // Knowledge changed by hand: the block is rewritten where it is.
    knowledge_node(&mine, "short", "taste", "Short answers.");
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert!(
        stdout.contains(&format!(
            "rewrote the knowledge block (5 nodes) in {}",
            claude.display()
        )),
        "{stdout}"
    );
    assert!(fs::read_to_string(&claude)
        .unwrap()
        .contains("- taste: Answer first.\n- taste: Short answers.\n"));
    assert_eq!(sandbox.history().len(), 2);

    let tracked = sandbox.git(&sandbox.config(), &["ls-files"]);
    for file in ["mine/short.md", "applied.toml"] {
        assert!(tracked.lines().any(|line| line == file), "{tracked}");
    }

    // Undone: every byte as it was, the history kept and told.
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "removed the knowledge block from {}",
            claude.display()
        )),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&claude).unwrap(), claude_text);
    assert_eq!(fs::read_to_string(&codex).unwrap(), codex_text);
    assert_eq!(
        fs::read_to_string(opencode.join("AGENTS.md")).unwrap(),
        "# opencode\n"
    );
    let history = sandbox.history();
    assert_eq!(history.len(), 3, "{history:#?}");
    assert!(history[0].starts_with("apply --undo: "), "{history:#?}");
}

/// The home's history holds only what ohjaa keeps there, never what an
/// editor, the Finder or a half-done write leaves beside it: an allowlist
/// in the repository's own `info/exclude`, so the home needs no file of its own.
#[test]
fn the_homes_history_never_takes_a_stray_file() {
    let sandbox = Sandbox::new("strays");
    let config = sandbox.config();
    knowledge_node(&config.join("mine"), "lima", "fact", "Lima is here.");
    fs::write(config.join("drafter"), "claude -p\n").unwrap();
    fs::write(config.join("roots"), format!("{}\n", sandbox.projects())).unwrap();
    let strays = [
        ".DS_Store",
        "mine/.DS_Store",
        "mine/.lima.md.swp",
        "mine/lima.md.tmp",
        "mine/lima.md~",
        "machine/.#lima.md",
        ".applied.toml.ohjaa-123",
        "notes.txt",
        "repos/acme/widget/repo.toml",
        "git/config",
    ];
    let stray = |path: &str| {
        let path = config.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "stray\n").unwrap();
    };
    strays[..8].iter().for_each(|path| stray(path));
    sandbox.apply();
    strays[8..].iter().for_each(|path| stray(path));
    sandbox.agent_file(".codex/AGENTS.md");
    sandbox.apply();
    let tracked = sandbox.git(&config, &["ls-files"]);
    for owned in ["mine/lima.md", "drafter", "roots", "applied.toml"] {
        assert!(
            tracked.lines().any(|line| line == owned),
            "{owned}: {tracked}"
        );
    }
    for path in strays {
        assert!(
            !tracked.lines().any(|line| line == path),
            "{path}: {tracked}"
        );
    }
    assert!(
        !config.join(".gitignore").exists(),
        "the home holds no file for git"
    );
    assert!(fs::read_to_string(config.join(".git/info/exclude"))
        .unwrap()
        .contains("!/mine/"));
}

/// #65's re-review: a home that is a linked worktree of another repository
/// (the person's dotfiles, say). git reads one `info/exclude` for every
/// worktree of a repository, so the home's allowlist, which begins by
/// leaving out everything (`/*`), is kept to the home's own worktree: its
/// history still takes no stray, and the other worktree still sees a new
/// file of its own.
#[test]
fn a_home_that_is_a_linked_worktree_keeps_its_allowlist_to_itself() {
    let sandbox = Sandbox::new("linked-home");
    let dotfiles = sandbox.dir.join("dotfiles");
    fs::create_dir_all(&dotfiles).unwrap();
    sandbox.git(&dotfiles, &["init", "-q", "-b", "main"]);
    fs::write(dotfiles.join(".zprofile"), "# mine\n").unwrap();
    sandbox.git(&dotfiles, &["add", "-A"]);
    sandbox.git(&dotfiles, &["commit", "-q", "-m", "dotfiles"]);
    let config = sandbox.config();
    sandbox.git(
        &dotfiles,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "ohjaa",
            &config.to_string_lossy(),
        ],
    );
    knowledge_node(&config.join("mine"), "lima", "fact", "Lima is here.");
    // The person's own global excludes still keep out what they name (the
    // review of #68, L1): a merge tool's `.orig` here.
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.orig\n").unwrap();
    let strays = [
        ".DS_Store",
        "notes.txt",
        "mine/lima.md~",
        "mine/lima.md.orig",
    ];
    for stray in strays {
        fs::write(config.join(stray), "stray\n").unwrap();
    }
    sandbox.apply();
    let tracked = sandbox.git(&config, &["ls-files"]);
    for owned in ["mine/lima.md", "applied.toml"] {
        assert!(
            tracked.lines().any(|line| line == owned),
            "{owned}: {tracked}"
        );
    }
    for stray in strays {
        assert!(
            !tracked.lines().any(|line| line == stray),
            "{stray}: {tracked}"
        );
    }
    fs::write(dotfiles.join(".zshenv"), "# new\n").unwrap();
    let status = sandbox.git(&dotfiles, &["status", "--porcelain"]);
    assert!(
        status.lines().any(|line| line == "?? .zshenv"),
        "the other worktree sees its new file: {status}"
    );
    let shared = fs::read_to_string(dotfiles.join(".git/info/exclude")).unwrap_or_default();
    assert!(!shared.contains("ohjaa"), "{shared}");
}

/// #65's re-review, L1: ohjaa is killed while the home's git commits. That
/// git goes on (it runs in a process group of its own), and it holds the
/// home's lock until it ends: the next change waits for it, and each commit
/// holds its own change, never another's. The home runs no hook, so the
/// commit is held up by a `git` first on PATH, the git ohjaa runs.
#[test]
fn a_commit_its_ohjaa_was_killed_in_holds_the_homes_lock_until_it_ends() {
    let sandbox = Sandbox::new("killed-commit");
    knowledge_node(&sandbox.config().join("mine"), "rust", "decision", "Rust.");
    sandbox.apply();
    let config = sandbox.config();
    // The home's first commit after `hold` is made says it began, and waits
    // while `hold` exists, before it is git's; every other git is git's.
    write_executable(
        &sandbox.dir.join("bin/git"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" {home}/.git \"*\" commit \"*)\n    \
             if [ -e \"$SANDBOX/hold\" ] && [ ! -e \"$SANDBOX/in-commit\" ]; then\n      \
             touch \"$SANDBOX/in-commit\"\n      \
             while [ -e \"$SANDBOX/hold\" ]; do sleep 0.05; done\n    fi;;\nesac\n\
             exec '{git}' \"$@\"\n",
            home = config.display(),
            git = system_git().display()
        ),
    );
    let hold = sandbox.dir.join("hold");
    fs::write(&hold, "").unwrap();
    sandbox.agent_file(".codex/AGENTS.md");
    let mut killed = sandbox
        .command("ohjaa", &sandbox.dir)
        .args(["apply"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let began = std::time::Instant::now();
    while !sandbox.dir.join("in-commit").exists() {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(60),
            "the first apply never began its commit"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    killed.kill().unwrap();
    killed.wait().unwrap();
    sandbox.agent_file(".claude/CLAUDE.md");
    let mut next = sandbox
        .command("ohjaa", &sandbox.dir)
        .args(["apply"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = lines_of(next.stdout.take().unwrap());
    let stderr = lines_of(next.stderr.take().unwrap());
    let (waited, said) = until_line(&stderr, "waiting for another ohjaa's change to the home");
    // Let the killed apply's commit end, whatever the next one did.
    fs::remove_file(&hold).unwrap();
    let status = next.wait().unwrap();
    let said: Vec<String> = said.into_iter().chain(stderr.iter()).collect();
    let printed: Vec<String> = stdout.iter().collect();
    assert!(waited, "the next change waited for the lock: {said:#?}");
    assert!(status.success(), "{printed:#?}{said:#?}");
    assert!(
        !said.iter().any(|line| line.contains("warning")),
        "{said:#?}"
    );
    let body = |commit: &str| sandbox.git(&config, &["show", "-s", "--format=%B", commit]);
    let (newest, before) = (body("HEAD"), body("HEAD~1"));
    assert!(newest.contains(".claude/CLAUDE.md"), "{newest}");
    assert!(!newest.contains(".codex/AGENTS.md"), "{newest}");
    assert!(before.contains(".codex/AGENTS.md"), "{before}");
    assert!(!before.contains(".claude/CLAUDE.md"), "{before}");
}

/// The review of #68, M1: nothing the home's git can leave running holds
/// the home's lock. The person's config turns on what git starts beside
/// itself: an fsmonitor (`core.fsmonitor`, here as a command, which git runs
/// on every platform; `true` starts git's own daemon, which lives on), hooks
/// from a template (`init.templateDir`) and from config (`hook.<name>`), and
/// a hook in the home's own hooks folder. Each one that runs leaves a process
/// behind, with whatever it was given open. Once the home's commit is done,
/// its lock is free, and the next change does not wait.
#[test]
fn nothing_the_homes_git_leaves_running_holds_the_homes_lock() {
    let sandbox = Sandbox::new("lingering");
    knowledge_node(&sandbox.config().join("mine"), "rust", "decision", "Rust.");
    let linger = |name: &str| {
        format!("#!/bin/sh\ntouch \"$SANDBOX/ran-{name}\"\nsleep 15 >/dev/null 2>&1 &\nexit 1\n")
    };
    let template = sandbox.dir.join("template");
    fs::create_dir_all(template.join("hooks")).unwrap();
    let events = [
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
        "post-index-change",
        "reference-transaction",
    ];
    for event in events {
        write_executable(&template.join("hooks").join(event), &linger(event));
    }
    write_executable(&sandbox.dir.join("bin/fsmonitor"), &linger("fsmonitor"));
    write_executable(&sandbox.dir.join("bin/config-hook"), &linger("config-hook"));
    for (key, value) in [
        ("core.fsmonitor", sandbox.dir.join("bin/fsmonitor")),
        ("init.templateDir", template),
        ("hook.linger.command", sandbox.dir.join("bin/config-hook")),
    ] {
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", key, &value.to_string_lossy()],
        );
    }
    for event in events {
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--add", "hook.linger.event", event],
        );
    }
    // The home's first commit: init, then add and commit.
    sandbox.apply();
    let config = sandbox.config();
    drop(hold_the_homes_lock(&config));
    assert!(
        !config.join(".git/hooks/pre-commit").exists(),
        "no hook is copied from a template"
    );
    fs::create_dir_all(config.join(".git/hooks")).unwrap();
    write_executable(&config.join(".git/hooks/post-commit"), &linger("home"));
    sandbox.agent_file(".codex/AGENTS.md");
    sandbox.apply();
    drop(hold_the_homes_lock(&config));
    for event in events {
        assert!(
            !sandbox.dir.join(format!("ran-{event}")).exists(),
            "{event}: a template's hook ran"
        );
    }
    assert!(
        !sandbox.dir.join("ran-home").exists(),
        "the home's own hook ran"
    );
    sandbox.agent_file(".claude/CLAUDE.md");
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains(".claude/CLAUDE.md"), "{stdout}");
    assert!(!stderr.contains("waiting"), "{stderr}");
    drop(hold_the_homes_lock(&config));
}

/// A knowledge block apply did not write (`brief --write`'s, or one the
/// person wrote) is theirs: apply leaves it and says so, and so does undo.
#[test]
fn a_block_apply_did_not_write_is_left_as_the_persons() {
    let sandbox = Sandbox::new("theirs");
    knowledge_node(&sandbox.config().join("mine"), "rust", "decision", "Rust.");
    let codex = sandbox.home().join(".codex/AGENTS.md");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let theirs = "# Codex\r\n<!-- ohjaa:knowledge begin: generated by `ohjaa brief --write`; edit \
                  the nodes, not this block -->\r\nall scopes\r\n<!-- ohjaa:knowledge end -->\r\n\
                  \tAfter.";
    fs::write(&codex, theirs).unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "{} holds a knowledge block apply did not write",
            codex.display()
        )),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&codex).unwrap(), theirs);
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "left the knowledge block in {}: apply did not write it",
            codex.display()
        )),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&codex).unwrap(), theirs);
}

/// The review of #66: apply ran once with an agent's folder where it usually
/// is and once moved (`CLAUDE_CONFIG_DIR`, `OPENCODE_CONFIG_DIR`), and wrote a
/// block into each file. Every file apply writes a block into is recorded, so
/// undo, run with neither variable, takes back all four and says so; none
/// keeps a block nobody will take out.
#[test]
fn undo_takes_back_every_file_apply_wrote_a_block_into() {
    let sandbox = Sandbox::new("moved-folders");
    knowledge_node(&sandbox.config().join("mine"), "rust", "decision", "Rust.");
    let claude = sandbox.home().join(".claude/CLAUDE.md");
    let opencode = sandbox.dir.join("config/opencode/AGENTS.md");
    let moved_claude = sandbox.dir.join("elsewhere/claude/CLAUDE.md");
    let moved_opencode = sandbox.dir.join("elsewhere/opencode/AGENTS.md");
    let before = [
        (&claude, "# Claude\n"),
        (&opencode, "# opencode\r\n\r\nKeep."),
        (&moved_claude, "# Moved Claude"),
        (&moved_opencode, "# Moved opencode\n\n"),
    ];
    for (file, text) in before {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let output = sandbox
        .command("ohjaa", &sandbox.dir)
        .env("CLAUDE_CONFIG_DIR", moved_claude.parent().unwrap())
        .env("OPENCODE_CONFIG_DIR", moved_opencode.parent().unwrap())
        .args(["apply"])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for (file, _) in before {
        assert!(
            fs::read_to_string(file)
                .unwrap()
                .contains("- decision: Rust.\n"),
            "{} holds the block",
            file.display()
        );
    }
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for (file, text) in before {
        assert_eq!(
            fs::read_to_string(file).unwrap(),
            text,
            "{} is as it was: {stdout}",
            file.display()
        );
        assert!(
            stdout.contains(&format!(
                "removed the knowledge block from {}\n",
                file.display()
            )),
            "{stdout}"
        );
    }
}

/// The review of #68, L2: one file named two ways (`~/.claude` a link to
/// the folder a later `CLAUDE_CONFIG_DIR` names) is one file to apply and to
/// undo: recorded once, and put back byte for byte.
#[test]
fn a_file_named_two_ways_is_recorded_once_and_undone_exactly() {
    let sandbox = Sandbox::new("two-spellings");
    let mine = sandbox.config().join("mine");
    knowledge_node(&mine, "rust", "decision", "Rust.");
    let real = sandbox.home().join("real-claude");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, sandbox.home().join(".claude")).unwrap();
    let before = "# Claude\nmy own text, no final newline";
    fs::write(real.join("CLAUDE.md"), before).unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    knowledge_node(&mine, "lima", "fact", "Lima is here.");
    let output = sandbox
        .command("ohjaa", &sandbox.dir)
        .env("CLAUDE_CONFIG_DIR", &real)
        .args(["apply"])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        fs::read_to_string(real.join("CLAUDE.md"))
            .unwrap()
            .contains("- fact: Lima is here.\n"),
        "rewritten through the other name"
    );
    let record = fs::read_to_string(sandbox.config().join("applied.toml")).unwrap();
    assert!(!record.contains("instructions_claude_2"), "{record}");
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(real.join("CLAUDE.md")).unwrap(),
        before,
        "{stdout}"
    );
}

/// One agent's file apply cannot read as a whole (two blocks in it) is said
/// and skipped, and everything else is applied; but apply is not done while
/// it stays unwritten. Every apply ends saying which file and why, and exits
/// 4 (incomplete), the commit names it, and doctor reports it, until the
/// file is fixed; undo skips it and undoes the rest.
#[test]
fn one_broken_agent_file_is_skipped_and_the_rest_is_applied() {
    let sandbox = Sandbox::new("broken-file");
    knowledge_node(&sandbox.config().join("mine"), "rust", "decision", "Rust.");
    let claude = sandbox.home().join(".claude/CLAUDE.md");
    let pi = sandbox.home().join(".pi/agent/AGENTS.md");
    let block =
        "<!-- ohjaa:knowledge begin: mine and machine -->\nx\n<!-- ohjaa:knowledge end -->\n";
    let broken = format!("# pi\n{block}{block}");
    for (file, text) in [(&claude, "# Mine\n"), (&pi, broken.as_str())] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let except = format!(
        "applied, except 1 agent file not written: {} (pi's own instructions:",
        pi.display()
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    assert!(
        stderr.contains(&format!("warning: not writing {}", pi.display())),
        "{stderr}"
    );
    assert!(
        stdout.contains(&format!("ohjaa apply: {except}")),
        "{stdout}"
    );
    assert_eq!(fs::read_to_string(&pi).unwrap(), broken);
    assert!(fs::read_to_string(&claude)
        .unwrap()
        .contains("- decision: Rust.\n"));
    assert!(fs::read_to_string(sandbox.home().join(".zshrc"))
        .unwrap()
        .contains(&sandbox.launch_line()));
    let history = sandbox.history();
    assert!(
        history[0].starts_with("apply: ")
            && history[0].contains("1 agent file not written")
            && history[0].contains(&format!("\nnot written: {} (", pi.display())),
        "{history:#?}"
    );

    // Applied again, and as a dry run: still not done, and said so.
    for args in [vec!["apply"], vec!["apply", "--dry-run"]] {
        let output = sandbox.ohjaa(&args);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(4), "{args:?}: {stdout}{stderr}");
        assert!(
            stdout.contains("except 1 agent file not written")
                && !stdout.contains("the workflow is applied"),
            "{args:?}: {stdout}"
        );
    }
    assert_eq!(sandbox.history().len(), 1, "nothing more to commit");
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    // Said once, as the reason says it: "fix it by hand" (#65's review).
    let drift: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("DRIFT") && line.contains(&pi.display().to_string()))
        .collect();
    assert_eq!(
        drift,
        [format!(
            "  DRIFT  {} (pi's own instructions): apply cannot write the knowledge block there: \
             the file has an ohjaa knowledge marker without its partner, or more than one \
             block; fix it by hand (once it is fixed, ohjaa apply)",
            pi.display()
        )],
        "{stdout}"
    );

    // Undo skips it, and undoes the rest.
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains(&format!("warning: not changing {}", pi.display())),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&claude).unwrap(), "# Mine\n");
    assert_eq!(fs::read_to_string(&pi).unwrap(), broken);

    // Fixed by hand: apply is done, and doctor finds the block current.
    fs::write(&pi, "# pi\n").unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(!stdout.contains("except"), "{stdout}");
    assert!(fs::read_to_string(&pi)
        .unwrap()
        .contains("- decision: Rust.\n"));
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, _) = text(&output);
    for file in [&claude, &pi] {
        let file = file.display().to_string();
        assert!(
            stdout.lines().any(|line| line.contains("ok")
                && line.contains(&file)
                && line.contains("knowledge block")),
            "{stdout}"
        );
        assert!(
            !stdout
                .lines()
                .any(|line| line.contains("DRIFT") && line.contains(&file)),
            "{stdout}"
        );
    }
}

/// A commit that fails is said, and what apply did stands.
#[test]
fn a_failed_commit_in_the_home_is_said_and_apply_stands() {
    let sandbox = Sandbox::new("commit-fails");
    sandbox.apply();
    assert_eq!(sandbox.history().len(), 1);
    // Another git holds the home's index: `git add` cannot take it.
    fs::write(sandbox.config().join(".git/index.lock"), "").unwrap();
    let claude = sandbox.home().join(".claude/CLAUDE.md");
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(&claude, "# Mine\n").unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains(
            "ohjaa apply: warning: \"apply: 1 change\" stands, but is not in the history of"
        ),
        "{stderr}"
    );
    assert!(fs::read_to_string(&claude)
        .unwrap()
        .contains("<!-- ohjaa:knowledge end -->"));
    // Once git can, the next commit takes it in.
    fs::remove_file(sandbox.config().join(".git/index.lock")).unwrap();
    fs::write(&claude, "# Mine, edited\n").unwrap();
    sandbox.apply();
    let history = sandbox.history();
    assert_eq!(history.len(), 2, "{history:#?}");
    let tracked = sandbox.git(&sandbox.config(), &["show", "--stat", "--format=", "HEAD"]);
    assert!(tracked.contains("applied.toml"), "{tracked}");
}

/// Should-fix 3: settings in a file git's global config includes count.
#[test]
fn settings_in_an_included_global_file_count() {
    let sandbox = Sandbox::new("includes");
    let extra = sandbox.dir.join("team.gitconfig");
    fs::write(
        sandbox.dir.join("gitconfig"),
        format!("[include]\n\tpath = {}\n", extra.display()),
    )
    .unwrap();
    fs::write(&extra, "[core]\n\texcludesFile = ~/team-ignore\n").unwrap();
    sandbox.apply();
    assert!(sandbox.read("home/team-ignore").contains(".sykli/"));
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);
}

#[test]
fn shell_init_prints_the_line_and_wraps_no_agent() {
    let sandbox = Sandbox::new("shell");
    let output = sandbox.ohjaa(&["shell-init", "bash"]);
    assert_eq!(output.status.code(), Some(2));
    let output = sandbox.ohjaa(&["shell-init", "zsh"]);
    assert_eq!(output.status.code(), Some(0));
    let printed = text(&output).0;
    assert!(!printed.contains("ohjaa run"), "{printed}");

    if !Path::new("/bin/zsh").exists() && !Path::new("/usr/bin/zsh").exists() {
        eprintln!("skipping the shell run: zsh is not installed here");
        return;
    }
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    // Typed at a terminal, an agent runs as typed: its rules reach it
    // through its own instruction file, and nothing routes it through ohjaa.
    let script = r#"
eval "$(ohjaa shell-init zsh)"
claude "fix the parser"
codex "write it"
"#;
    let output = sandbox
        .command("zsh", &repo)
        .args(["-f", "-c", script])
        .env("OHJAA_LINE", "0")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert_eq!(
        stdout, "direct fix the parser\ndirect-codex write it\n",
        "{stderr}"
    );
}

/// "We don't want to run git git git": each command's own git processes, as
/// git's trace2 counts them, within its budget. Every case first proves it
/// measured something real (a positive control): what the command does
/// visibly happened, and the trace holds the git it ran.
#[test]
fn every_command_stays_within_its_git_budget() {
    let sandbox = Sandbox::new("budget");
    let (widget, _) = sandbox.repo(
        "acme",
        "widget",
        &[
            ("sykli.json", "{}\n"),
            (".toimija/repo.yaml", "repo:\n  name: widget\n"),
        ],
    );
    sandbox.repo("other", "free", &[]);
    let mut report = Vec::new();
    let mut budget = |what: &str, starts: usize, at_most: usize| {
        eprintln!("{what:<44} {starts} git processes (budget {at_most})");
        report.push((what.to_owned(), starts, at_most));
    };
    let root = sandbox.projects();
    let ohjaa_here = |args: &[&str]| {
        let mut command = sandbox.command("ohjaa", &sandbox.dir);
        command.args(args);
        command
    };

    // apply, first run: git's global config, read once, and no clone; then
    // what it changed, the home's first commit (its `init`, `add -A` and
    // `commit`, and no maintenance after it).
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply"]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("ohjaa apply: changed"),
        "{}",
        text(&output).0
    );
    assert!(started(&starts, "--global"), "{starts:#?}");
    assert!(
        started(&starts, "init") && started(&starts, "commit"),
        "{starts:#?}"
    );
    assert!(!started(&starts, "maintenance"), "{starts:#?}");
    budget("ohjaa apply (first run)", starts.len(), 1 + 3);
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply"]));
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    assert!(started(&starts, "--global"), "{starts:#?}");
    budget("ohjaa apply (no change)", starts.len(), 1);

    // doctor: git's global config, once; no clone is read.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["doctor"]));
    let (stdout, _) = text(&output);
    assert!(stdout.contains("ohjaa doctor: healthy"), "{stdout}");
    assert!(started(&starts, "--global"), "{starts:#?}");
    budget("ohjaa doctor", starts.len(), 1);

    // brief in a clone: the repository's root, asked once for the knowledge
    // and once for the roots (2), and no clone read beyond it.
    let (starts, output) = sandbox.git_starts(sandbox.command("ohjaa", &widget).arg("brief"));
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.contains("No knowledge recorded yet"),
        "{stdout}{stderr}"
    );
    budget("ohjaa brief", starts.len(), 2);

    // collect: one `git config` per clone of the fleet under the roots (the
    // one with `.toimija/repo.yaml`), to find the owners whose pull requests
    // GitHub is asked about; the other clone is not read.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["collect", "--root", &root]));
    assert!(
        sandbox
            .dir
            .join("state/ohjaa/human/collected.json")
            .is_file(),
        "{}",
        text(&output).1
    );
    let listings = starts
        .iter()
        .filter(|line| line.contains("\"config\",\"-z\",\"--list\""))
        .count();
    assert_eq!(
        listings, 1,
        "the fleet clone's config, read once: {starts:#?}"
    );
    budget("ohjaa collect (2 clones, 1 of the fleet)", starts.len(), 1);

    let over: Vec<_> = report
        .iter()
        .filter(|(_, starts, at_most)| starts > at_most)
        .collect();
    assert!(over.is_empty(), "over budget: {over:?}");
}

/// The ohjaa the terminal's line runs is the one on PATH, unless --bin
/// names another.
#[test]
fn apply_runs_the_ohjaa_on_path_unless_bin_names_another() {
    let sandbox = Sandbox::new("bin");
    let other = sandbox.dir.join("other");
    fs::create_dir_all(&other).unwrap();
    copy_executable(Path::new(env!("CARGO_BIN_EXE_ohjaa")), &other.join("ohjaa"));
    let elsewhere = other.join("ohjaa").to_string_lossy().into_owned();
    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply"])
        .output()
        .unwrap();
    let (_, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("is not the one on PATH"), "{stderr}");
    assert!(!sandbox.config().exists(), "nothing half applied");
    assert!(
        !sandbox.home().join(".zshrc").exists(),
        "nothing half applied"
    );

    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply", "--bin", &elsewhere])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(fs::read_to_string(sandbox.home().join(".zshrc"))
        .unwrap()
        .contains(&format!("[[ -x '{elsewhere}' ]]")));
}

/// The ignore block holds `.sykli/`, `.teko/` and `.ohjaa/`. A block an
/// earlier apply wrote with fewer is brought up to date in place, and undo
/// still puts back exactly what was there before the first apply.
#[test]
fn an_older_ignore_block_is_upgraded_in_place_and_undo_is_exact() {
    let sandbox = Sandbox::new("teko");
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    sandbox.apply();
    let ignore = sandbox.read("config/git/ignore");
    assert!(ignore.contains("\n.sykli/\n.teko/\n.ohjaa/\n"), "{ignore}");
    sandbox.git(&repo, &["check-ignore", "-q", ".teko/state.json"]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);
    sandbox.git(
        &repo,
        &["check-ignore", "-q", ".ohjaa/knowledge/a-lesson.md"],
    );

    // As each earlier apply left it: `.sykli/` only, then `.sykli/` and
    // `.teko/`, the block on machines applied before `.ohjaa/` was added.
    for older in [
        ignore.replace(".sykli/\n.teko/\n.ohjaa/\n", ".sykli/\n"),
        ignore.replace(".sykli/\n.teko/\n.ohjaa/\n", ".sykli/\n.teko/\n"),
    ] {
        fs::write(sandbox.dir.join("config/git/ignore"), &older).unwrap();
        let output = sandbox.ohjaa(&["doctor"]);
        let (stdout, _) = text(&output);
        assert!(
            stdout.contains("holds an older ohjaa block; it should ignore .sykli/ .teko/ .ohjaa/"),
            "{stdout}"
        );
        let output = sandbox.ohjaa(&["apply"]);
        let (stdout, _) = text(&output);
        assert!(
            stdout.contains("added .sykli/ .teko/ .ohjaa/ to"),
            "{stdout}"
        );
        assert_eq!(
            sandbox.read("config/git/ignore"),
            ignore,
            "upgraded in place"
        );
    }
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert!(!stdout.contains("added .sykli/"), "applied twice: {stdout}");
    assert_eq!(sandbox.read("config/git/ignore"), ignore, "applied twice");

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp\n");
}
