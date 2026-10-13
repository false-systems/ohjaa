//! The workflow home from outside the repositories: `apply`, `doctor`,
//! `adopt`, `hook`, `gate` and `shell-init`, run as the real binary against
//! real git.
//!
//! Every command runs with an emptied environment: HOME, XDG_CONFIG_HOME,
//! XDG_STATE_HOME, CARGO_HOME and TMPDIR inside the sandbox,
//! GIT_CONFIG_GLOBAL at a sandbox file, GIT_CONFIG_NOSYSTEM=1, no ZDOTDIR, and
//! a PATH of stubs (sykli, toimija, claude, codex) plus the system directories.
//! `apply` writes git's global config and ~/.zshrc, so nothing here may reach
//! the person's real ones, nor run a real agent or tool.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Never the real sykli. `hook pre-push --help` names `--contract` and
/// `--branch` with STUB_SYKLI_CONTRACT=1, only `--contract` with `nobranch`,
/// neither without it; with `fast`, every `--help` also names `--repo-root`
/// and `--git-common-dir`. The pre-push hook keeps its stdin and, like the
/// real one, judges exactly the branch it is given: a push to it is refused
/// in sykli's words unless the repository has `.sykli/receipt.json`, and an
/// accepted one says so on stderr. `run` passes unless `$SANDBOX/fail-gate`
/// exists, and its receipt names the tree in `$SANDBOX/tree`; `verify` passes
/// a receipt that says passed. It runs no git, so a trace2 count is ohjaa's
/// own.
const SYKLI_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$SANDBOX/sykli-calls"
facts() {
  if [ "${STUB_SYKLI_CONTRACT:-}" = fast ]; then
    echo "      --repo-root <DIR>        the repository's work tree"
    echo "      --git-common-dir <DIR>   its common git directory"
  fi
}
case "$1" in
  hook)
    if [ "$3" = --help ]; then
      echo "Usage: sykli hook pre-push [OPTIONS] [REMOTE] [URL]"
      case "${STUB_SYKLI_CONTRACT:-}" in
        1|fast) echo "      --contract <CONTRACT>  a contract outside the repository"
           echo "      --branch <BRANCH>      gate exactly this branch";;
        nobranch) echo "      --contract <CONTRACT>  a contract outside the repository";;
      esac
      facts
      exit 0
    fi
    shift 2; branch=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --contract|--repo-root|--git-common-dir) shift;;
        --branch) branch=$2; shift;;
        *) break;;
      esac
      shift
    done
    cat > "$SANDBOX/sykli-stdin"
    [ -n "$branch" ] || { echo "error: no --branch" >&2; exit 2; }
    if grep -q " refs/heads/$branch " "$SANDBOX/sykli-stdin" && [ ! -f .sykli/receipt.json ]; then
      echo "sykli hook pre-push: refusing to push 0123456789ab to refs/heads/$branch: no receipt in this worktree is a run of its tree" >&2
      echo "sykli hook pre-push: to produce a receipt, check out 0123456789ab and run: sykli run" >&2
      exit 1
    fi
    echo "sykli hook pre-push: kept the receipt as evidence" >&2
    exit 0;;
  init)
    if [ "$2" = --help ]; then
      echo "Usage: sykli init [OPTIONS] [PATH]"
      [ -n "${STUB_SYKLI_CONTRACT:-}" ] && echo "      --contract <CONTRACT>  write the contract there"
      exit 0
    fi
    [ "$2" = --contract ] && { printf '{"version":"1","tasks":{}}\n' > "$3"; exit 0; }
    exit 2;;
  run)
    if [ "$2" = --help ]; then echo "Usage: sykli run [OPTIONS] [CONTRACT]"; facts; exit 0; fi
    tree=$(cat "$SANDBOX/tree" 2>/dev/null)
    receipt() {
      printf '{"schema":"sykli-receipt.v1","outcome":"%s","started_at_ms":%s,"subject":{"tree_oid":"%s","head_tree_oid":"%s","dirty":false}}\n' "$1" "$(date +%s)000" "$tree" "$tree"
    }
    if [ -f "$SANDBOX/fail-gate" ]; then
      receipt failed; echo "failed: says-one" >&2; exit 1
    fi
    receipt passed; echo "passed: says-one" >&2; exit 0;;
  verify)
    if [ "$2" = --help ]; then
      echo "Usage: sykli verify [OPTIONS] <RECEIPT>"
      echo "      --commit <REV>  judge the receipt as evidence for this commit"
      echo "With --commit the subject is the commit, and nothing is read from the checkout."
      facts; exit 0
    fi
    grep -q passed "$2" || { echo "refused: outcome expected passed" >&2; exit 1; }
    exit 0;;
esac
exit 2
"#;

/// Never the real toimija: no live sessions, so `ohjaa run --dry-run` gets
/// as far as its warnings.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions) echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}';;
  context) echo '{}';;
esac
"#;

const CLAUDE_STUB: &str = "#!/bin/sh\necho \"direct $*\"\n";
const CODEX_STUB: &str = "#!/bin/sh\necho \"direct-codex $*\"\n";

const OLD_ZSHRC: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
    # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
    [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
    alias ll='ls -l'\n";

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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
    /// STUB_SYKLI_CONTRACT: "" (an old sykli), "nobranch" or "1".
    stub_contract: &'static str,
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
        write_executable(&bin.join("sykli"), SYKLI_STUB);
        write_executable(&bin.join("claude"), CLAUDE_STUB);
        write_executable(&bin.join("toimija"), TOIMIJA_STUB);
        write_executable(&bin.join("codex"), CODEX_STUB);
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ohjaa"), bin.join("ohjaa")).unwrap();
        // Only the stubs and the system: no real sykli, cargo, agent or ohjaa.
        let git_dir = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .find(|dir| dir.join("git").is_file())
            .unwrap_or_else(|| PathBuf::from("/usr/bin"));
        let path = format!(
            "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
            bin.display(),
            git_dir.display()
        );
        Sandbox {
            dir,
            path,
            stub_contract: "",
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config").join("ohjaa")
    }

    /// An entry's own hooks folder.
    fn entry_hooks(&self, owner: &str, name: &str) -> PathBuf {
        self.config()
            .join("repos")
            .join(owner)
            .join(name)
            .join("hooks")
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
        if !self.stub_contract.is_empty() {
            command.env("STUB_SYKLI_CONTRACT", self.stub_contract);
        }
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

    /// `core.hooksPath` as git resolves it in `repo`, `None` when unset.
    fn hooks_path(&self, repo: &Path) -> Option<String> {
        let output = self.git_out(repo, &["config", "--get", "core.hooksPath"]);
        output
            .status
            .success()
            .then(|| text(&output).0.trim().to_owned())
    }

    /// Whether git runs an entry's hooks in `repo`.
    fn gated(&self, repo: &Path) -> bool {
        let repos = format!("{}/", self.config().join("repos").display());
        self.hooks_path(repo)
            .is_some_and(|path| path.starts_with(&repos) && path.ends_with("/hooks"))
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
            fs::write(repo.join(file), contents).unwrap();
        }
        self.git(&repo, &["add", "-A"]);
        self.git(&repo, &["commit", "-q", "-m", "init"]);
        self.git(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
        self.git(&repo, &["push", "-q", "-u", "origin", "main"]);
        self.git(&repo, &["remote", "set-head", "origin", "main"]);
        (repo, bare)
    }

    /// An entry written by hand: `repo.toml`, and a gate unless `None`.
    fn entry(&self, owner: &str, name: &str, gate: Option<&str>) {
        let dir = self.config().join("repos").join(owner).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("repo.toml"), "branch = \"main\"\n").unwrap();
        if let Some(gate) = gate {
            fs::write(dir.join("sykli.json"), gate).unwrap();
        }
    }

    fn commit(&self, repo: &Path, file: &str) -> String {
        fs::write(repo.join(file), file).unwrap();
        self.git(repo, &["add", file]);
        self.git(repo, &["commit", "-q", "-m", file]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    fn status(&self, repo: &Path) -> String {
        self.git(
            repo,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
        )
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn apply(&self) {
        let output = self.ohjaa(&["apply", "--root", &self.projects()]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }

    fn adopt(&self, repo: &Path) {
        let output = self.ohjaa(&["adopt", &repo.to_string_lossy()]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }

    /// The ohjaa processes git starts for `args` in `repo`: its hook runs from
    /// the home's hooks folder, as GIT_TRACE reports them.
    fn ohjaa_runs(&self, repo: &Path, args: &[&str]) -> usize {
        let output = self
            .command("git", repo)
            .env("GIT_TRACE", "1")
            .args(args)
            .output()
            .unwrap();
        let (_, stderr) = text(&output);
        // `trace: run_command: [VAR=value …] <entry>/hooks/<name> args`
        let repos = format!(" {}/", self.config().join("repos").display());
        stderr
            .lines()
            .filter(|line| {
                line.contains("run_command:") && line.contains(&repos) && line.contains("/hooks/")
            })
            .count()
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
    sandbox.repo("acme", "any", &[]);
    let output = sandbox.ohjaa(&["apply", "--dry-run", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for said in [
        format!(
            "would write {} with {}, the roots apply and doctor scan",
            sandbox.config().join("roots").display(),
            sandbox.projects()
        ),
        format!(
            "would write {}",
            sandbox.config().join("git/config").display()
        ),
        format!(
            "would add include.path = {} to git's global config, {}",
            sandbox.config().join("git/config").display(),
            sandbox.dir.join("gitconfig").display()
        ),
        format!(
            "would add .sykli/ .teko/ to {}",
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
fn apply_includes_one_file_and_no_repository_without_an_entry_runs_ohjaa() {
    let sandbox = Sandbox::new("apply");
    fs::write(sandbox.home().join(".zshrc"), OLD_ZSHRC).unwrap();
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    let (other, bare) = sandbox.repo("acme", "other", &[]);
    let (widget, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&widget);

    // One include, no global core.hooksPath.
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get-all", "include.path"]
        ),
        sandbox.config().join("git/config").to_string_lossy()
    );
    let global = sandbox.git_out(
        &sandbox.dir,
        &["config", "--global", "--get", "core.hooksPath"],
    );
    assert!(!global.status.success(), "no global core.hooksPath");
    // The entry's own folder: only pre-push, since the clone has no hook of
    // its own; the dispatcher names ohjaa and sykli by their paths.
    let hooks = sandbox.entry_hooks("acme", "widget");
    assert_eq!(names_in(&hooks), ["pre-push"]);
    let dispatcher = fs::read_to_string(hooks.join("pre-push")).unwrap();
    assert!(dispatcher.contains(&format!(
        "OHJAA_SYKLI='{}'; export OHJAA_SYKLI\n",
        sandbox.dir.join("bin/sykli").display()
    )));
    assert!(dispatcher.ends_with(&format!(
        "exec '{}' hook pre-push \"$@\"\n",
        sandbox.dir.join("bin/ohjaa").display()
    )));
    assert!(executable(&hooks.join("pre-push")));
    assert_eq!(names_in(&sandbox.config().join("git")), ["config"]);
    assert_eq!(
        sandbox.hooks_path(&widget).as_deref(),
        Some(hooks.to_str().unwrap())
    );
    // The roots apply scanned are written down.
    assert!(sandbox
        .read("config/ohjaa/roots")
        .contains(&sandbox.projects()));
    // A repository whose remote names no entry never runs ohjaa.
    assert!(!sandbox.gated(&other));
    assert_eq!(
        sandbox.ohjaa_runs(&other, &["commit", "-q", "--allow-empty", "-m", "x"]),
        0
    );
    assert_eq!(
        sandbox.ohjaa_runs(&other, &["push", "-q", "origin", "main"]),
        0
    );
    // git init and git clone still work.
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "cloned"],
    );
    // Git reads the ignore file apply wrote: .sykli/ is ignored everywhere.
    let ignore = sandbox.read("config/git/ignore");
    assert!(
        ignore.starts_with("*.swp\n") && ignore.contains("\n.sykli/\n"),
        "{ignore}"
    );
    sandbox.git(&other, &["check-ignore", "-q", ".sykli/receipt.json"]);
    let zshrc = fs::read_to_string(sandbox.home().join(".zshrc")).unwrap();
    assert!(!zshrc.contains("ohjaa-launch.zsh"), "{zshrc}");
    assert!(
        zshrc.ends_with(&format!("{}\nalias ll='ls -l'\n", sandbox.launch_line())),
        "{zshrc}"
    );

    let before = (
        sandbox.read("gitconfig"),
        sandbox.read("config/git/ignore"),
        zshrc,
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    assert_eq!(
        before,
        (
            sandbox.read("gitconfig"),
            sandbox.read("config/git/ignore"),
            fs::read_to_string(sandbox.home().join(".zshrc")).unwrap()
        )
    );

    // An edited dispatcher is drift, and apply puts it back.
    fs::write(hooks.join("pre-push"), "#!/bin/sh\nexit 0\n").unwrap();
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("pre-push is not one of apply's dispatchers"),
        "{stdout}"
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    assert!(
        text(&output).0.contains("wrote 1 dispatcher into"),
        "{}",
        text(&output).0
    );
}

/// Every form a remote URL takes, against the patterns apply generates for
/// the entry false-systems/ohjaa (and git's insteadOf rules).
#[test]
fn the_remote_patterns_gate_every_url_form_and_nothing_else() {
    let sandbox = Sandbox::new("patterns");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "url.git@github.com:.insteadOf", "gh:"],
    );
    sandbox.git(
        &sandbox.dir,
        &[
            "config",
            "--global",
            "url.git@github.com:false-systems/.insteadOf",
            "fs:",
        ],
    );
    sandbox.entry("false-systems", "ohjaa", Some("{}\n"));
    sandbox.apply();
    let matrix: &[(&[&str], bool)] = &[
        (&["git@github.com:false-systems/ohjaa.git"], true),
        (&["git@github.com:false-systems/ohjaa"], true),
        (&["https://github.com/false-systems/ohjaa"], true),
        (&["https://github.com/false-systems/ohjaa.git"], true),
        (&["https://github.com/false-systems/ohjaa/"], true),
        (&["https://github.com/false-systems/ohjaa.git/"], true),
        (&["git@github.com:false-systems/ohjaa.git/"], true),
        (&["ssh://git@github.com/false-systems/ohjaa.git"], true),
        (&["ssh://git@github.com:22/false-systems/ohjaa.git"], true),
        (&["github.com:false-systems/ohjaa.git"], true),
        (&["https://x@github.com/false-systems/ohjaa.git"], true),
        (&["https://github.com/False-Systems/ohjaa.git"], true),
        (&["https://GITHUB.com/FALSE-SYSTEMS/OHJAA"], true),
        (&["gh:false-systems/ohjaa"], true),
        (&["fs:ohjaa.git"], true),
        (
            &[
                "git@github.com:yairfalse/fork.git",
                "git@github.com:false-systems/ohjaa.git",
            ],
            true,
        ),
        (
            &["https://evil.example/github.com/false-systems/ohjaa"],
            true,
        ),
        (&["https://github.com/false-systems/ohjaa-other.git"], false),
        (&["git@github.com:false-systems/ohjaa-other"], false),
        (&["git@github.com:yairfalse/ohjaa.git"], false),
        (&["https://github.com/other/ohjaa"], false),
        (&["git@github.com:false-systems/xohjaa"], false),
        (&["fs:ohjaa-other"], false),
        (&[], false),
    ];
    let mut wrong = Vec::new();
    for (at, (urls, gated)) in matrix.iter().enumerate() {
        let repo = sandbox.dir.join(format!("r{at}"));
        sandbox.git(&sandbox.dir, &["init", "-q", &repo.to_string_lossy()]);
        for (n, url) in urls.iter().enumerate() {
            sandbox.git(&repo, &["remote", "add", &format!("r{n}"), url]);
        }
        let label = if urls.is_empty() {
            "(no remote)".to_owned()
        } else {
            urls.join(" + ")
        };
        eprintln!(
            "{label:<72} gated: {}",
            if sandbox.gated(&repo) { "yes" } else { "no" }
        );
        if sandbox.gated(&repo) != *gated {
            wrong.push(label);
        }
    }
    assert!(wrong.is_empty(), "wrongly gated or not: {wrong:?}");

    // A linked worktree of a gated clone is gated too.
    let gated = sandbox.dir.join("r0");
    sandbox.git(&gated, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let linked = sandbox.dir.join("r0-linked");
    sandbox.git(
        &gated,
        &["worktree", "add", "-q", &linked.to_string_lossy()],
    );
    assert!(sandbox.gated(&linked));
}

#[test]
fn a_gated_repository_starts_ohjaa_only_for_push_and_for_hooks_it_has() {
    let sandbox = Sandbox::new("count");
    let (gated, _) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let (free, _) = sandbox.repo("acme", "free", &[]);
    write_executable(
        &own.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho own >> \"$SANDBOX/own-calls\"\n",
    );
    fs::create_dir_all(gated.join(".sykli")).unwrap();
    fs::write(gated.join(".sykli/receipt.json"), "{}").unwrap();
    // (status, commit, fetch, push): the ohjaa processes git starts.
    let counts = |label: &str, repo: &Path| {
        let status = sandbox.ohjaa_runs(repo, &["status"]);
        let commit = sandbox.ohjaa_runs(repo, &["commit", "-q", "--allow-empty", "-m", "n"]);
        let fetch = sandbox.ohjaa_runs(repo, &["fetch", "-q", "origin"]);
        let push = sandbox.ohjaa_runs(repo, &["push", "-q", "origin", "HEAD:refs/heads/side"]);
        eprintln!("{label:<36} status {status}  commit {commit}  fetch {fetch}  push {push}");
        (status, commit, fetch, push)
    };

    sandbox.apply();
    sandbox.adopt(&gated);
    sandbox.adopt(&own);
    // Each entry has its own folder: own's holds its pre-commit too (adopt
    // wrote it), gated's only pre-push.
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "gated")),
        ["pre-push"]
    );
    let own_hooks = sandbox.entry_hooks("acme", "own");
    assert_eq!(names_in(&own_hooks), ["pre-commit", "pre-push"]);
    fs::remove_file(own_hooks.join("pre-commit")).unwrap();
    sandbox.apply();
    assert_eq!(
        names_in(&own_hooks),
        ["pre-commit", "pre-push"],
        "apply arrives at it too"
    );

    assert_eq!(counts("entry, with its own pre-commit", &own), (0, 1, 0, 1));
    assert_eq!(counts("entry, without", &gated), (0, 0, 0, 1));
    assert_eq!(counts("no entry", &free), (0, 0, 0, 0));
    assert_eq!(
        sandbox.read("own-calls"),
        "own\n",
        "the own pre-commit ran once"
    );
}

#[test]
fn apply_refuses_a_global_hooks_path_unless_forced_and_undo_puts_it_back() {
    let sandbox = Sandbox::new("refuse");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "core.hooksPath", "/elsewhere/hooks"],
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(
        stderr.contains("core.hooksPath is /elsewhere/hooks"),
        "{stderr}"
    );
    assert!(stderr.contains("ohjaa apply --force"), "{stderr}");
    assert!(!sandbox.config().exists(), "nothing half applied");
    assert!(
        !sandbox.home().join(".zshrc").exists(),
        "nothing half applied"
    );

    let output = sandbox.ohjaa(&["apply", "--force", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("removed git's global core.hooksPath (/elsewhere/hooks)"),
        "{stdout}"
    );
    let global = sandbox.git_out(
        &sandbox.dir,
        &["config", "--global", "--get", "core.hooksPath"],
    );
    assert!(!global.status.success());

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get", "core.hooksPath"]
        ),
        "/elsewhere/hooks"
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
    let (repo, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    assert!(sandbox.gated(&repo));

    let output = sandbox.ohjaa(&["apply", "--undo", "--dry-run"]);
    let (stdout, _) = text(&output);
    for said in [
        "would remove include.path = ",
        "would remove the \"ohjaa apply\" block from ",
        "would change ",
        "  + [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh",
        "would remove what apply generated in ",
        "repos/acme/widget/hooks/pre-push",
        "repos/acme/widget/gated.config",
        "dry run; nothing was changed",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
    assert!(
        sandbox.entry_hooks("acme", "widget").exists() && sandbox.gated(&repo),
        "a dry run undoes nothing"
    );

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
        ["repos", "roots"],
        "entries and roots kept"
    );
    assert_eq!(
        names_in(&sandbox.config().join("repos/acme/widget")),
        ["repo.toml", "sykli.json"]
    );
    assert!(!sandbox.gated(&repo));
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert!(text(&output).0.contains("nothing to undo"));
}

#[test]
fn undo_removes_the_files_apply_created() {
    let sandbox = Sandbox::new("undo-created");
    sandbox.apply();
    assert!(sandbox.home().join(".zshrc").exists());
    assert!(sandbox.dir.join("config/git/ignore").exists());
    assert!(sandbox.dir.join("gitconfig").exists());
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(!sandbox.home().join(".zshrc").exists());
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    assert!(
        !sandbox.dir.join("gitconfig").exists(),
        "a global config apply created is removed, not left empty"
    );
    assert!(
        names_in(&sandbox.config()).is_empty(),
        "{:?}",
        names_in(&sandbox.config())
    );
}

/// Each change is recorded before it is made: an apply that fails midway
/// can still be undone.
#[test]
fn an_apply_that_fails_midway_can_be_undone() {
    let sandbox = Sandbox::new("midway");
    let gitconfig = "[user]\n\tname = Someone\n";
    fs::write(sandbox.dir.join("gitconfig"), gitconfig).unwrap();
    // The ignore file's folder cannot be written: apply fails there, after
    // the include is in git's global config.
    let folder = sandbox.dir.join("config/git");
    fs::create_dir_all(&folder).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(output.status.code(), Some(0), "{}", text(&output).0);
    assert!(sandbox.read("gitconfig").contains("[include]"));
    assert!(sandbox
        .read("config/ohjaa/applied.toml")
        .contains("include = "));
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("gitconfig"), gitconfig);
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

#[test]
fn adopt_copies_the_gate_from_the_default_branch_and_never_writes_into_the_repo() {
    let sandbox = Sandbox::new("adopt");
    let contract = "{\"version\":\"1\",\"tasks\":{\"test\":{}}}\n";
    let lock = "{\"schema\":\"sykli-lock.v2\"}\n";
    let (repo, _) = sandbox.repo(
        "acme",
        "widget",
        &[
            ("sykli.json", contract),
            ("sykli.lock", lock),
            (".gitignore", "target/\n"),
        ],
    );
    sandbox.apply();
    let global_before = sandbox.read("gitconfig");
    // Someone's uncommitted work: the gate comes from origin/main, not from it.
    fs::write(repo.join("sykli.json"), "{\"edited\":true}\n").unwrap();
    fs::write(repo.join("notes.txt"), "untracked\n").unwrap();
    fs::create_dir_all(repo.join("target")).unwrap();
    fs::write(repo.join("target/x"), "ignored\n").unwrap();
    let before = sandbox.status(&repo);
    assert!(!sandbox.gated(&repo));
    // A tracked file whose stat no longer matches the index: a `git status`
    // would rewrite .git/index (and take index.lock) to refresh it.
    fs::write(repo.join("README"), "hello\n").unwrap();
    let index = repo.join(".git/index");
    let index_before = fs::metadata(&index).unwrap().modified().unwrap();

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::metadata(&index).unwrap().modified().unwrap(),
        index_before,
        "adopt touched .git/index"
    );
    let entry = sandbox.config().join("repos/acme/widget");
    assert!(stdout.contains(&entry.display().to_string()), "{stdout}");
    assert!(stdout.contains("copied from origin/main"), "{stdout}");
    assert!(stdout.contains(&format!(
        "wrote {}",
        sandbox.config().join("git/config").display()
    )));
    assert!(stdout.contains("  observed: git runs"), "{stdout}");
    assert!(fs::read_to_string(entry.join("repo.toml"))
        .unwrap()
        .contains("\nbranch = \"main\"\n"));
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        contract
    );
    assert_eq!(fs::read_to_string(entry.join("sykli.lock")).unwrap(), lock);
    assert_eq!(
        sandbox.status(&repo),
        before,
        "adopt wrote into the repository"
    );
    assert_eq!(
        sandbox.read("gitconfig"),
        global_before,
        "adopt left git's global config alone"
    );
    assert!(sandbox.gated(&repo), "git runs ohjaa's hooks there now");

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(3), "adopting twice is refused");
    assert!(text(&output).1.contains("already adopted"));
    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy(), "--force"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn adopt_asks_sykli_for_a_gate_kept_outside_the_repo_and_refuses_when_it_cannot() {
    let mut sandbox = Sandbox::new("adopt-init");
    let (plain, _) = sandbox.repo("acme", "plain", &[]);
    let before = sandbox.status(&plain);
    let output = sandbox.ohjaa(&["adopt", &plain.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("names no --contract"), "{stderr}");
    assert!(
        !sandbox.config().join("repos/acme/plain").exists(),
        "a refused adopt leaves no entry"
    );

    sandbox.stub_contract = "1";
    let output = sandbox.ohjaa(&["adopt", &plain.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let entry = sandbox.config().join("repos/acme/plain");
    assert!(entry.join("sykli.json").is_file());
    assert!(entry.join("repo.toml").is_file());
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|init --contract {}",
            plain.display(),
            entry.join("sykli.json").display()
        )),
        "{}",
        sandbox.read("sykli-calls")
    );
    assert_eq!(sandbox.status(&plain), before);
    // Before apply, adopt cannot make ohjaa observe it: git's global config
    // does not include the home's file, and adopt never writes git's global
    // config.
    assert!(
        stderr.contains("its pushes are not observed (ohjaa apply)"),
        "{stderr}"
    );
}

#[test]
fn an_unproven_push_to_main_lands_with_a_flag_and_a_proven_one_is_quiet() {
    let mut sandbox = Sandbox::new("observe");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);

    // No receipt: sykli does not accept it, and the push lands all the same.
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", "main"]),
        head,
        "it landed"
    );
    let flags: Vec<&str> = stderr.lines().filter(|l| l.starts_with("ohjaa")).collect();
    assert_eq!(
        flags,
        [
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree). \
          It is pushed and flagged; prove it with: ohjaa gate"
        ],
        "one flag, in sykli's words: {stderr}"
    );
    assert!(
        !stderr.contains("sykli hook pre-push"),
        "sykli's own lines are not repeated: {stderr}"
    );
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|hook pre-push --contract {} --branch main origin {}",
            repo.display(),
            contract.display(),
            bare.display()
        )),
        "{}",
        sandbox.read("sykli-calls")
    );
    assert!(sandbox
        .read("sykli-stdin")
        .starts_with(&format!("refs/heads/main {head} refs/heads/main {landed}")));

    // Proven (a receipt sykli accepts): quiet, sykli's lines and all.
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    let head = sandbox.commit(&repo, "proven");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        !stderr.contains("ohjaa:") && !stderr.contains("sykli hook"),
        "a proven push is quiet: {stderr}"
    );

    // A push that does not move the entry's branch is not judged at all.
    let calls = sandbox.read("sykli-calls").lines().count();
    sandbox.commit(&repo, "side");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/side"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
    assert_eq!(sandbox.read("sykli-calls").lines().count(), calls);
    assert_eq!(
        sandbox.git(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "the repository stays clean"
    );
}

#[test]
fn without_a_sykli_that_takes_a_contract_the_push_lands_flagged() {
    let sandbox = Sandbox::new("no-contract");
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    sandbox.commit(&repo, "change");

    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    let head = sandbox.commit(&repo, "more");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (this sykli cannot judge")
            && stderr.contains("names no --contract and no --branch"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);

    fs::remove_file(sandbox.dir.join("bin/sykli")).unwrap();
    let head = sandbox.commit(&repo, "again");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (the sykli `ohjaa apply` recorded (")
            && stderr.contains(") is gone). It is pushed and flagged"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

#[test]
fn an_entry_that_cannot_be_read_flags_the_push_and_lets_it_through() {
    let sandbox = Sandbox::new("broken-entry");
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    fs::create_dir_all(sandbox.config().join("repos/acme/widget")).unwrap();
    fs::write(
        sandbox.config().join("repos/acme/widget/sykli.json"),
        "{}\n",
    )
    .unwrap();
    sandbox.apply();
    assert!(
        sandbox.gated(&repo),
        "a broken entry still has its repository observed"
    );
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/feature"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNOBSERVED push to acme/widget: its entry cannot be read")
            && stderr.contains("no repo.toml"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "feature"]), head);
}

#[test]
fn a_repo_without_an_entry_pushes_freely() {
    let sandbox = Sandbox::new("free");
    let (repo, bare) = sandbox.repo("acme", "free", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "other", Some("{}\n"));
    sandbox.apply();
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(!sandbox.read("sykli-calls").contains("hook pre-push"));
}

#[test]
fn the_repositorys_own_hooks_still_run_through_the_dispatcher() {
    let mut sandbox = Sandbox::new("chain");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let hooks = repo.join(".git/hooks");
    write_executable(
        &hooks.join("pre-commit"),
        "#!/bin/sh\necho \"pre-commit $#\" >> \"$SANDBOX/own-calls\"\n[ ! -f \"$SANDBOX/fail\" ]\n",
    );
    write_executable(
        &hooks.join("commit-msg"),
        "#!/bin/sh\necho \"commit-msg $1\" >> \"$SANDBOX/own-calls\"\n",
    );
    // No #! line: git runs such a hook through the shell, and so does ohjaa.
    write_executable(
        &hooks.join("post-commit"),
        "echo post-commit >> \"$SANDBOX/own-calls\"\n",
    );
    write_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\necho \"pre-push $*\" >> \"$SANDBOX/own-calls\"\ncat > \"$SANDBOX/own-stdin\"\n",
    );
    sandbox.apply();
    sandbox.adopt(&repo);
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "own")),
        ["commit-msg", "post-commit", "pre-commit", "pre-push"]
    );

    let first = sandbox.commit(&repo, "one");
    let calls = sandbox.read("own-calls");
    assert!(calls.contains("pre-commit 0\n"), "{calls}");
    assert!(
        calls.contains("commit-msg .git/COMMIT_EDITMSG\n"),
        "{calls}"
    );
    assert!(calls.contains("post-commit\n"), "{calls}");

    // The repository's own refusal still refuses.
    fs::write(sandbox.dir.join("fail"), "").unwrap();
    fs::write(repo.join("two"), "two").unwrap();
    sandbox.git(&repo, &["add", "two"]);
    let output = sandbox.git_out(&repo, &["commit", "-q", "-m", "two"]);
    assert!(!output.status.success(), "the own pre-commit refused");
    assert_eq!(sandbox.git(&repo, &["rev-parse", "HEAD"]), first);
    fs::remove_file(sandbox.dir.join("fail")).unwrap();

    // A linked worktree shares the common directory's hooks.
    let linked = sandbox.dir.join("linked");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ],
    );
    fs::write(sandbox.dir.join("own-calls"), "").unwrap();
    sandbox.commit(&linked, "side");
    assert!(sandbox.read("own-calls").contains("pre-commit 0\n"));

    // pre-push: sykli and the own hook read the same stdin.
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(sandbox
        .read("own-calls")
        .contains(&format!("pre-push origin {}\n", bare.display())));
    let own = sandbox.read("own-stdin");
    assert!(
        own.starts_with(&format!("refs/heads/main {first} refs/heads/main ")),
        "{own}"
    );
    assert_eq!(own, sandbox.read("sykli-stdin"));

    // The repository's own pre-push still refuses, unproven or not, and an
    // unproven push it refuses is not said to be pushed.
    write_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\ncat > /dev/null\necho 'own pre-push: no' >&2\nexit 3\n",
    );
    fs::remove_file(repo.join(".sykli/receipt.json")).unwrap();
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);
    sandbox.commit(&repo, "refused-by-own");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(
        !output.status.success(),
        "the own pre-push refused: {stderr}"
    );
    assert!(stderr.contains("own pre-push: no"), "{stderr}");
    assert!(!stderr.contains("It is pushed"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), landed);
    fs::remove_file(hooks.join("pre-push")).unwrap();

    // A hook that is not executable is skipped, with git's own hint.
    fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(sandbox.dir.join("fail"), "").unwrap();
    fs::write(repo.join("three"), "three").unwrap();
    sandbox.git(&repo, &["add", "three"]);
    let output = sandbox.git_out(&repo, &["commit", "-q", "-m", "three"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("hook was ignored because it's not set as executable"),
        "{stderr}"
    );
}

#[test]
fn hook_arguments_reach_the_own_hook_unchanged() {
    let sandbox = Sandbox::new("args");
    let (repo, _) = sandbox.repo("acme", "args", &[]);
    write_executable(
        &repo.join(".git/hooks/pre-rebase"),
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg[$a]\"; done\nexit 7\n",
    );
    // Arguments that look like ohjaa's own flags are the hook's, not ohjaa's.
    let output = sandbox.ohjaa_in(
        &repo,
        &["hook", "pre-rebase", "--root", "-x", "two words", "--"],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(7),
        "the own hook's status: {stderr}"
    );
    assert_eq!(stdout, "arg[--root]\narg[-x]\narg[two words]\narg[--]\n");

    let output = sandbox.ohjaa_in(&repo, &["hook", "not-a-hook"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).1.contains("is not a git hook"));
}

#[test]
fn doctor_checks_every_gated_clone_and_ohjaa_run_warns_in_one() {
    let sandbox = Sandbox::new("doctor");
    let root = sandbox.projects();
    let (gated, _) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own-hooks", &[]);
    sandbox.git(&own, &["config", "core.hooksPath", ".githooks"]);
    sandbox.repo("acme", "older", &[("sykli.json", "{}\n")]);
    sandbox.repo("acme", "plain", &[]);

    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    for said in [
        "ok     git 2.",
        "DRIFT  git's global config does not include",
        "ok     git's global core.hooksPath is unset",
        "/git/config is missing: nothing is observed (ohjaa apply)",
        "lacks the ohjaa block (.sykli/ .teko/)",
        "does not load the launcher",
        &format!("roots: {root} (given with --root)"),
        "note   older: an older in-repo sykli.json and no entry",
        "note   gated: an older in-repo sykli.json and no entry",
        "not observed (no entry; git never runs ohjaa there): own-hooks, plain",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    sandbox.apply();
    sandbox.adopt(&gated);
    // No --root: the roots file apply wrote.
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let hooks = sandbox.entry_hooks("acme", "gated");
    for said in [
        "ok     git's global config includes",
        "/git/config is current",
        "/repos/acme/gated/gated.config is current",
        &format!(
            "ok     acme/gated: dispatchers (pre-push) exec {}",
            sandbox.dir.join("bin/ohjaa").display()
        ),
        &format!(
            "roots: {root} (from {})",
            sandbox.config().join("roots").display()
        ),
        &format!(
            "ok     gated (acme/gated): observed; git runs {} here",
            hooks.display()
        ),
        "ohjaa doctor: healthy",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    // (a) An entry for a clone whose own core.hooksPath wins.
    sandbox.entry("acme", "own-hooks", Some("{}\n"));
    sandbox.apply();
    // (b) A hook of its own that has no dispatcher: git would skip it.
    write_executable(&gated.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    for said in [
        "DRIFT  own-hooks (acme/own-hooks): its own core.hooksPath (.githooks) overrides \
         ohjaa's: its pushes are not observed here: the repo's own hooks run",
        &format!(
            "DRIFT  gated (acme/gated): its own pre-commit hook has no dispatcher in {}, so git \
             skips it (ohjaa apply)",
            hooks.display()
        ),
        &format!(
            "DRIFT  acme/gated: dispatchers missing in {}: pre-commit (ohjaa apply)",
            hooks.display()
        ),
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    // ohjaa run says the same at launch, in the repository it starts in.
    let output = sandbox.ohjaa_in(&gated, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains("ohjaa: warning: ")
            && stderr.contains("its own pre-commit hook has no dispatcher"),
        "{stderr}"
    );

    // The dispatchers exec an ohjaa that is gone, and hand the gate to a
    // sykli that is gone.
    sandbox.apply();
    fs::remove_file(sandbox.dir.join("bin/ohjaa")).unwrap();
    fs::remove_file(sandbox.dir.join("bin/sykli")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ohjaa"))
        .env_clear()
        .envs(
            sandbox
                .command("x", &sandbox.dir)
                .get_envs()
                .filter_map(|(k, v)| Some((k, v?))),
        )
        .current_dir(&sandbox.dir)
        .args(["doctor", "--root", &root])
        .output()
        .unwrap();
    let (stdout, _) = text(&output);
    for said in [
        "which is gone: pushes in its clones fail, and so do commits in clones that chain a \
         hook of their own",
        "the pre-push dispatcher hands pushes to",
        "every push to the entry's branch is flagged unproven, never judged",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
}

/// B2: a hook whose mere presence changes what git does is never dispatched,
/// so a gated checkout that takes pushes with updateInstead still updates.
#[test]
fn a_gated_checkout_with_update_instead_still_updates_its_worktree() {
    let sandbox = Sandbox::new("update-instead");
    let (target, _) = sandbox.repo("acme", "target", &[("f", "v1\n")]);
    let (deploy, _) = sandbox.repo("acme", "deploy", &[]);
    sandbox.git(
        &target,
        &["config", "receive.denyCurrentBranch", "updateInstead"],
    );
    // Another gated clone has its own push-to-checkout: it gets no dispatcher.
    write_executable(
        &deploy.join(".git/hooks/push-to-checkout"),
        "#!/bin/sh\nexit 0\n",
    );
    sandbox.entry("acme", "target", Some("{}\n"));
    sandbox.entry("acme", "deploy", Some("{}\n"));
    sandbox.apply();
    assert!(sandbox.gated(&target));
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "target")),
        ["pre-push"]
    );
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "deploy")),
        ["pre-push"]
    );

    let work = sandbox.dir.join("work");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &target.to_string_lossy(), "work"],
    );
    fs::write(work.join("f"), "v2\n").unwrap();
    sandbox.git(&work, &["commit", "-q", "-am", "v2"]);
    sandbox.git(&work, &["push", "-q", "origin", "main"]);
    assert_eq!(
        fs::read_to_string(target.join("f")).unwrap(),
        "v2\n",
        "worktree updated"
    );
    assert_eq!(sandbox.git(&target, &["status", "--porcelain"]), "");

    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert!(
        stdout.contains(
            "deploy (acme/deploy): its own push-to-checkout cannot run under ohjaa's hooks folder"
        ),
        "{stdout}"
    );
}

/// B3: the entry's branch is the observed one, whatever origin/HEAD says.
#[test]
fn the_entry_branch_is_observed_whatever_origin_head_says() {
    let mut sandbox = Sandbox::new("branch");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    // origin/HEAD at develop, entry main.
    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    sandbox.git(&repo, &["remote", "set-head", "origin", "develop"]);
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main ("),
        "{stderr}"
    );
    assert!(sandbox.read("sykli-calls").contains("--branch main origin"));
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/develop"]);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);

    // Entry trunk2: trunk2 is observed, and main no longer is.
    fs::write(
        sandbox.config().join("repos/acme/widget/repo.toml"),
        "branch = \"trunk2\"\n",
    )
    .unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/trunk2"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to trunk2 ("),
        "{stderr}"
    );
    assert!(sandbox
        .read("sykli-calls")
        .contains("--branch trunk2 origin"));
    sandbox.commit(&repo, "b");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success());
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
}

/// B3: a sykli that takes --contract but not --branch cannot judge the
/// entry's branch, so a push to it is flagged, never said to be proven.
#[test]
fn a_sykli_without_branch_flags_the_entry_branch_and_passes_others_quietly() {
    let mut sandbox = Sandbox::new("nobranch");
    sandbox.stub_contract = "nobranch";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/feature"]);
    assert!(output.status.success());
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (") && stderr.contains("names no --branch"),
        "{stderr}"
    );
    assert!(!stderr.contains("no --contract"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// Should-fix 4: an entry means the repository is meant to be observed.
#[test]
fn an_entry_without_a_gate_flags_its_branch() {
    let mut sandbox = Sandbox::new("no-gate");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", None);
    sandbox.apply();
    let head = sandbox.commit(&repo, "a");
    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (its entry has no gate: "),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// Should-fix 1: `ohjaa gate` runs the gate a push is judged by, and says so.
#[test]
fn ohjaa_gate_runs_the_entry_gate_and_prints_it() {
    let mut sandbox = Sandbox::new("gate-command");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    fs::create_dir_all(repo.join("src")).unwrap();

    let output = sandbox.ohjaa_in(&repo.join("src"), &["gate", "--print"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "ohjaa gate: the gate is {} (the entry for acme/widget (branch main))",
            contract.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "cd '{}' && mkdir -p .sykli && sykli run '{c}' --json > .sykli/receipt.json && \
         sykli verify .sykli/receipt.json --contract '{c}' --commit HEAD",
            repo.display(),
            c = contract.display()
        )),
        "{stdout}"
    );
    assert!(!repo.join(".sykli").exists(), "--print runs nothing");

    let head = sandbox.commit(&repo, "a");
    let output = sandbox.ohjaa_in(&repo.join("src"), &["gate"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains(&format!(
        "$ sykli run '{}' --json > .sykli/receipt.json",
        contract.display()
    )));
    assert!(
        stdout.contains(
            "ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven"
        ),
        "{stdout}"
    );
    assert!(sandbox
        .read("projects/widget/.sykli/receipt.json")
        .contains("passed"));
    assert!(sandbox.read("sykli-calls").contains(&format!(
        "{}|run {} --json",
        repo.display(),
        contract.display()
    )));
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);

    fs::write(sandbox.dir.join("fail-gate"), "").unwrap();
    let output = sandbox.ohjaa_in(&repo, &["gate"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("ohjaa gate: failed: sykli run exited 1"),
        "{stdout}"
    );

    // No entry: the repository's own sykli.json; neither: no gate.
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let output = sandbox.ohjaa_in(&own, &["gate", "--print"]);
    assert!(text(&output).0.contains(&format!(
        "the gate is {} (the repository's own sykli.json (no entry))",
        own.join("sykli.json").display()
    )));
    let (none, _) = sandbox.repo("acme", "none", &[]);
    let output = sandbox.ohjaa_in(&none, &["gate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).1.contains("has no gate"));
}

/// A receipt of `tree` that says `outcome`, written as sykli writes one.
fn receipt(path: &Path, tree: &str, outcome: &str, at_ms: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{{\"schema\":\"sykli-receipt.v1\",\"outcome\":\"{outcome}\",\"started_at_ms\":{at_ms},\
             \"subject\":{{\"tree_oid\":\"{tree}\",\"head_tree_oid\":\"{tree}\",\"dirty\":false}}}}\n"
        ),
    )
    .unwrap();
}

fn short(commit: &str) -> &str {
    &commit[..7]
}

impl Sandbox {
    fn tree(&self, repo: &Path, rev: &str) -> String {
        self.git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// `ohjaa report` over the projects folder, as text.
    fn report(&self, cwd: &Path) -> String {
        let output = self.ohjaa_in(cwd, &["report", "--root", &self.projects()]);
        text(&output).0
    }

    /// `ohjaa report --json` over the projects folder.
    fn report_json(&self, cwd: &Path) -> serde_json::Value {
        let output = self.ohjaa_in(cwd, &["report", "--json", "--root", &self.projects()]);
        let (stdout, stderr) = text(&output);
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"))
    }

    /// The `sykli verify … --commit` calls so far.
    fn verifies(&self) -> usize {
        self.read("sykli-calls")
            .lines()
            .filter(|line| line.contains("|verify ") && line.contains("--commit"))
            .count()
    }
}

/// The marks: ✓ where a receipt of the commit's tree verifies, ✗ where one is
/// there and did not pass, ? where none is; tip first, down to the first
/// proven commit; sykli asked only where a receipt says it passed. A merge
/// the forge made, with the tree of a proven pull request head, is proven.
#[test]
fn marks_are_derived_from_receipts_tip_first() {
    let mut sandbox = Sandbox::new("marks");
    sandbox.stub_contract = "fast";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let common = repo.join(".git");
    // Proven: the receipt sykli kept for an accepted push.
    let c1 = sandbox.commit(&repo, "one");
    let t1 = sandbox.tree(&repo, "HEAD");
    receipt(
        &common.join("sykli/receipts/rcpt_1.json"),
        &t1,
        "passed",
        1_000,
    );
    // Failed: its run in this worktree.
    let c2 = sandbox.commit(&repo, "two");
    let t2 = sandbox.tree(&repo, "HEAD");
    receipt(
        &repo.join(".sykli/receipts/rcpt_2.json"),
        &t2,
        "failed",
        2_000,
    );
    // Never run.
    let c3 = sandbox.commit(&repo, "three");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    let before = sandbox.verifies();

    let report = sandbox.report(&repo);
    let line = format!(
        "- acme/widget main (never fetched here): ? {} ✗ {} ✓ {}\n",
        short(&c3),
        short(&c2),
        short(&c1)
    );
    assert!(
        report.contains(
            "Marks on each entry's branch, tip first (✓ proven · ✗ failed · ? unproven):\n"
        ) && report.contains(&line),
        "{report}"
    );
    assert_eq!(
        sandbox.verifies() - before,
        1,
        "sykli asked only about the one receipt that says it passed"
    );
    let calls = sandbox.read("sykli-calls");
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        calls.contains(&format!(
            "|verify {} --contract {} --repo-root {} --git-common-dir {} --commit {c1}",
            common.join("sykli/receipts/rcpt_1.json").display(),
            contract.display(),
            repo.display(),
            common.display()
        )),
        "{calls}"
    );

    let json = sandbox.report_json(&repo);
    assert_eq!(json["schema"], "ohjaa-marks.v1");
    let entry = &json["entries"][0];
    assert_eq!(entry["repo"], "acme/widget", "{json:#}");
    assert_eq!(entry["branch"], "main");
    assert_eq!(entry["ended"], "proven");
    assert_eq!(entry["fetched"], serde_json::Value::Null);
    let marks: Vec<(&str, &str)> = entry["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["sha"].as_str().unwrap(), c["mark"].as_str().unwrap()))
        .collect();
    assert_eq!(
        marks,
        [
            (c3.as_str(), "unproven"),
            (c2.as_str(), "failed"),
            (c1.as_str(), "proven")
        ]
    );
    assert_eq!(entry["commits"][1]["tree"], t2.as_str());
    assert_eq!(entry["commits"][1]["author"], "Ohjaa Test");
    assert_eq!(entry["commits"][1]["subject"], "two");

    // A pull request proven in a linked worktree (`ohjaa gate`'s receipt),
    // merged on the forge.
    let linked = sandbox.dir.join("pr");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.commit(&linked, "four");
    receipt(
        &linked.join(".sykli/receipt.json"),
        &sandbox.tree(&linked, "HEAD"),
        "passed",
        4_000,
    );
    sandbox.git(&linked, &["push", "-q", "origin", "pr"]);
    let forge = sandbox.dir.join("forge");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "forge"],
    );
    sandbox.git(
        &forge,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "origin/pr",
        ],
    );
    sandbox.git(&forge, &["push", "-q", "origin", "main"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    let merge = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    assert_ne!(merge, head);
    assert_eq!(
        sandbox.tree(&repo, &merge),
        sandbox.tree(&repo, &head),
        "the forge's merge has its head's tree"
    );
    let report = sandbox.report(&repo);
    assert!(
        report.contains("- acme/widget main (as of last fetch 2")
            && report.contains(&format!("UTC): ✓ {}\n", short(&merge))),
        "{report}"
    );
}

/// The briefing opens with the flag while the entry's branch is not proven
/// at its tip; `ohjaa gate` there proves it, and the flag is gone.
#[test]
fn the_briefing_opens_with_the_flag_and_ohjaa_gate_flips_the_tip() {
    let mut sandbox = Sandbox::new("flag");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let c1 = sandbox.commit(&repo, "one");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c2 = sandbox.commit(&repo, "two");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    let (stdout, stderr) = text(&output);
    let never = format!(
        "FLAG: main of acme/widget has never been proven (3 commits: ? {}, ? {}, ? {}; never \
         fetched here). Start by running `ohjaa gate` on main and fix forward what fails.\n",
        short(&c2),
        short(&c1),
        short(&init)
    );
    assert!(stdout.starts_with(&never), "{stdout}{stderr}");

    // Proven below: unproven since the commit above it, one sykli verify.
    receipt(
        &repo.join(".git/sykli/receipts/rcpt_1.json"),
        &sandbox.tree(&repo, &c1),
        "passed",
        1_000,
    );
    let before = sandbox.verifies();
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    let since = format!(
        "FLAG: main of acme/widget is unproven since {c} (1 commit: ? {c}; never fetched \
         here). Start by running `ohjaa gate` on main and fix forward what fails.",
        c = short(&c2)
    );
    assert!(stderr.contains(&format!("ohjaa: {since}\n")), "{stderr}");
    let would = stderr
        .lines()
        .find(|line| line.starts_with("ohjaa: dry-run; would run: "))
        .unwrap_or_else(|| panic!("{stderr}"));
    let briefing = would
        .split_once("--briefing ")
        .map(|(_, rest)| rest.trim_start_matches('\''))
        .unwrap_or_else(|| panic!("{would}"));
    assert!(
        briefing.starts_with(&since),
        "the flag comes first: {would}"
    );
    assert_eq!(sandbox.verifies() - before, 1, "one sykli verify");

    // `ohjaa gate` on main, at origin/main's tip: proven, and no flag.
    fs::write(sandbox.dir.join("tree"), sandbox.tree(&repo, "HEAD")).unwrap();
    let output = sandbox.ohjaa_in(&repo, &["gate"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).0);
    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    let (stdout, _) = text(&output);
    assert!(!stdout.contains("FLAG:"), "{stdout}");
    assert!(
        sandbox.report(&repo).contains(&format!(
            "- acme/widget main (never fetched here): ✓ {}\n",
            short(&c2)
        )),
        "{}",
        sandbox.report(&repo)
    );

    // No entry, no flag, and no git asked for one.
    let (plain, _) = sandbox.repo("acme", "plain", &[]);
    let output = sandbox.ohjaa_in(&plain, &["brief"]);
    assert!(!text(&output).0.contains("FLAG:"), "{}", text(&output).0);
}

// ---- against the real sykli --------------------------------------------------
//
// What a flag says and what a mark is depend on sykli's own judgement, so
// these run the real one: built from sykli's main and named by
// OHJAA_TEST_SYKLI. CI's runners have no sykli, so they are ignored there:
//
//     OHJAA_TEST_SYKLI=/path/to/sykli cargo test --test config_home -- --ignored

const NEEDS_SYKLI: &str = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored";

/// A gate that fails while `state.txt` says broken.
const REAL_GATE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"ok","run":"! grep -q broken state.txt","inputs":["state.txt"]}]}"#;

impl Sandbox {
    /// A sandbox whose sykli is the real one, with `acme/widget` cloned and
    /// given an entry whose gate the real sykli locked.
    fn with_real_sykli(tag: &str) -> (Sandbox, PathBuf, PathBuf) {
        let sykli = std::env::var_os("OHJAA_TEST_SYKLI")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| panic!("{NEEDS_SYKLI}"));
        let sandbox = Sandbox::new(tag);
        let bin = sandbox.dir.join("bin/sykli");
        fs::remove_file(&bin).unwrap();
        std::os::unix::fs::symlink(&sykli, &bin).unwrap();
        let (repo, bare) = sandbox.repo("acme", "widget", &[("state.txt", "ok\n")]);
        sandbox.entry("acme", "widget", Some(REAL_GATE));
        let contract = sandbox.config().join("repos/acme/widget/sykli.json");
        let output = sandbox
            .command("sykli", &repo)
            .arg("lock")
            .arg(&contract)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output).1);
        sandbox.apply();
        (sandbox, repo, bare)
    }

    /// Write `state.txt` and commit it.
    fn state(&self, repo: &Path, state: &str, message: &str) -> String {
        fs::write(repo.join("state.txt"), format!("{state}\n")).unwrap();
        self.git(repo, &["commit", "-q", "-am", message]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    /// `ohjaa gate` in `repo`, with its exit code.
    fn gate(&self, repo: &Path) -> (Option<i32>, String) {
        let output = self.ohjaa_in(repo, &["gate"]);
        let (stdout, stderr) = text(&output);
        (output.status.code(), format!("{stdout}{stderr}"))
    }
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_an_unproven_push_lands_flagged_and_a_proven_one_is_quiet() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-push");

    let head = sandbox.state(&repo, "ok again", "unproven");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        stderr.contains(
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree). \
             It is pushed and flagged; prove it with: ohjaa gate\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("sykli hook"), "{stderr}");

    // Proven with `ohjaa gate`: quiet, and sykli keeps the receipt.
    let head = sandbox.state(&repo, "ok, proven", "proven");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        !stderr.contains("ohjaa:") && !stderr.contains("sykli hook"),
        "a proven push is quiet: {stderr}"
    );
    assert_eq!(
        names_in(&repo.join(".git/sykli/receipts")).len(),
        1,
        "sykli kept the receipt that proved it"
    );

    // A failing run: the push lands, flagged in sykli's words.
    let head = sandbox.state(&repo, "broken", "broken");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(1), "{said}");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (no receipt of its tree verifies; the newest says .sykli/receipt.json: outcome expected passed or cached"),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "nothing but sykli's own .sykli/, which every repository ignores"
    );
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_marks_proven_failed_unproven_and_a_forge_merge() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-marks");
    let c1 = sandbox.state(&repo, "ok, one", "one");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c2 = sandbox.state(&repo, "broken", "two");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(1), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c3 = sandbox.state(&repo, "ok, three", "three");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (never fetched here): ? {} ✗ {} ✓ {}\n",
            short(&c3),
            short(&c2),
            short(&c1)
        )),
        "{report}"
    );
    let json = sandbox.report_json(&repo);
    let commits = json["entries"][0]["commits"].as_array().unwrap();
    assert_eq!(commits[1]["mark"], "failed");
    assert!(
        commits[1]["why"]
            .as_str()
            .unwrap()
            .contains("its receipt says the run failed"),
        "{json:#}"
    );

    // A pull request proven in a linked worktree, merged on the forge.
    let linked = sandbox.dir.join("pr");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.state(&linked, "ok, four", "four");
    let (code, said) = sandbox.gate(&linked);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&linked, &["push", "-q", "origin", "pr"]);
    let forge = sandbox.dir.join("forge");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "forge"],
    );
    sandbox.git(
        &forge,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "origin/pr",
        ],
    );
    sandbox.git(&forge, &["push", "-q", "origin", "main"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    let merge = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    assert_ne!(merge, head);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!("UTC): ✓ {}\n", short(&merge))),
        "the merge has its proven head's tree: {report}"
    );
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_the_briefing_flags_first_and_ohjaa_gate_flips_the_tip() {
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-flag");
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let tip = sandbox.state(&repo, "ok, unproven", "unproven");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.starts_with(&format!(
            "FLAG: main of acme/widget has never been proven (2 commits: ? {}, ? {}; never \
             fetched here). Start by running `ohjaa gate` on main and fix forward what fails.\n",
            short(&tip),
            short(&init)
        )),
        "{stdout}{stderr}"
    );
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains("--briefing 'FLAG: main of acme/widget has never been proven"),
        "{stderr}"
    );

    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    assert!(!text(&output).0.contains("FLAG:"), "{}", text(&output).0);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (never fetched here): ✓ {}\n",
            short(&tip)
        )),
        "{report}"
    );
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
    fs::write(&extra, "[core]\n\thooksPath = /team/hooks\n").unwrap();
    for args in [&["apply"][..], &["apply", "--force"][..]] {
        let output = sandbox.ohjaa(&[args, &["--root", &sandbox.projects()]].concat());
        let (_, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(3), "{args:?}: {stderr}");
        assert!(stderr.contains("/team/hooks"), "{stderr}");
    }
    assert!(
        text(&sandbox.ohjaa(&["apply", "--force", "--root", &sandbox.projects()]))
            .1
            .contains("a file git's global config includes")
    );

    fs::write(&extra, "[core]\n\texcludesFile = ~/team-ignore\n").unwrap();
    sandbox.apply();
    assert!(sandbox.read("home/team-ignore").contains(".sykli/"));
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);
}

/// The nit: entries are found whatever the case of the remote.
#[test]
fn an_entry_is_found_whatever_the_case_of_the_remote() {
    let mut sandbox = Sandbox::new("case");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("Acme", "Widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    assert!(sandbox
        .config()
        .join("repos/acme/widget/repo.toml")
        .is_file());
    assert!(sandbox.gated(&repo));
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(
        output.status.success() && stderr.contains("ohjaa: UNPROVEN push to main ("),
        "the entry was found: {stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// A clone that names an entry only through its push URL is not gated by
/// git's patterns (they see remote.*.url); doctor and `ohjaa run` say so.
#[test]
fn doctor_flags_a_clone_named_only_by_its_push_url() {
    let sandbox = Sandbox::new("pushurl");
    let (repo, bare) = sandbox.repo("acme", "pushy", &[]);
    sandbox.git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/other/thing",
        ],
    );
    sandbox.git(
        &repo,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            &bare.to_string_lossy(),
        ],
    );
    sandbox.entry("acme", "pushy", Some("{}\n"));
    sandbox.apply();
    assert!(!sandbox.gated(&repo));
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("DRIFT  pushy (acme/pushy): git does not include"),
        "{stdout}"
    );
}

/// A generated file git cannot read breaks every git command: doctor names
/// it, and apply puts it right.
#[test]
fn an_unreadable_generated_file_is_named_and_apply_repairs_it() {
    let sandbox = Sandbox::new("corrupt");
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    fs::write(
        sandbox.config().join("git/config"),
        "[includeIf \"hasconfig:remote.*.url:x\n",
    )
    .unwrap();
    assert!(
        !sandbox.git_out(&repo, &["status"]).status.success(),
        "git is broken"
    );
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("cannot be read by git, which then fails in every repository"),
        "{stdout}"
    );
    sandbox.apply();
    assert!(sandbox.git_out(&repo, &["status"]).status.success());
    assert!(sandbox.gated(&repo));
}

#[test]
fn shell_init_prints_the_launcher_and_its_rules_hold() {
    let sandbox = Sandbox::new("shell");
    let output = sandbox.ohjaa(&["shell-init", "bash"]);
    assert_eq!(output.status.code(), Some(2));
    let output = sandbox.ohjaa(&["shell-init", "zsh"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output).0.contains("_ohjaa_should_wrap() {"));

    if !Path::new("/bin/zsh").exists() && !Path::new("/usr/bin/zsh").exists() {
        eprintln!("skipping the launcher's rules: zsh is not installed here");
        return;
    }
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    // The launcher wraps only at a terminal; the test says it is at one, and
    // ohjaa is a function that prints how it was called.
    let script = r#"
eval "$(ohjaa shell-init zsh)"
_ohjaa_at_terminal() { true; }
ohjaa() { print -r -- "ohjaa $*"; }
claude
claude "fix the parser"
claude --model opus "do x"
claude -p hi
claude --resume
claude a b
claude mcp list
OHJAA_WRAP=0 claude
OHJAA_SESSION=s claude
codex "write it"
codex exec hi
cd /
claude
"#;
    let output = sandbox
        .command("zsh", &repo)
        .args(["-f", "-c", script])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert_eq!(
        stdout,
        "ohjaa run claude --intent interactive --\n\
         ohjaa run claude --intent interactive --task fix the parser --\n\
         ohjaa run claude --intent interactive --task do x -- --model opus\n\
         direct -p hi\n\
         direct --resume\n\
         direct a b\n\
         direct mcp list\n\
         direct \n\
         direct \n\
         ohjaa run codex --intent interactive --task write it --\n\
         direct-codex exec hi\n\
         direct \n",
        "{stderr}"
    );
    assert!(
        stderr.contains("not wrapped (more than one prompt-like argument)"),
        "{stderr}"
    );
}

/// "We don't want to run git git git": each command's own git processes, as
/// git's trace2 counts them, within its budget. Every case first proves it
/// measured something real (a positive control): the trace holds the git
/// command that ran the hook, and what the command does visibly happened.
/// A hook is measured as the same git command run through ohjaa's
/// dispatcher, minus the same command with ohjaa out of the way. The stub
/// sykli runs no git, so the rest is ohjaa's own.
#[test]
fn every_command_stays_within_its_git_budget() {
    let mut sandbox = Sandbox::new("budget");
    // A sykli that takes the repository's facts: ohjaa hands them over.
    sandbox.stub_contract = "fast";
    let (gated, bare) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    write_executable(
        &own.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho ran >> \"$SANDBOX/own-pre-commit\"\n",
    );
    sandbox.repo("acme", "free", &[]);
    let (local, _) = sandbox.repo("acme", "local-hooks", &[("sykli.json", "{}\n")]);
    sandbox.git(&local, &["config", "core.hooksPath", ".githooks"]);
    let mut report = Vec::new();
    let mut budget = |what: &str, starts: usize, at_most: usize| {
        let unit = if what.ends_with("sykli verify") {
            "sykli verify calls"
        } else {
            "git processes"
        };
        eprintln!("{what:<44} {starts} {unit} (budget {at_most})");
        report.push((what.to_owned(), starts, at_most));
    };
    let root = sandbox.projects();
    let ohjaa_here = |args: &[&str]| {
        let mut command = sandbox.command("ohjaa", &sandbox.dir);
        command.args(args);
        command
    };

    // apply before any entry: git's version and global config, no clone read
    // (nothing can be gated yet), the include added and git/config checked
    // before it is written.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply", "--root", &root]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("ohjaa apply: wrote"),
        "{}",
        text(&output).0
    );
    assert!(
        started(&starts, "--version") && started(&starts, "--global"),
        "{starts:#?}"
    );
    budget(
        "ohjaa apply (no entries yet, first run)",
        starts.len(),
        2 + 2,
    );
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply"]));
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    assert!(started(&starts, "--global"), "{starts:#?}");
    budget("ohjaa apply (no entries yet, no change)", starts.len(), 2);

    // adopt: the repository's config and objects, git's global config, its
    // config again once gated, and a check per file it writes (2).
    let (starts, output) =
        sandbox.git_starts(&mut ohjaa_here(&["adopt", &gated.to_string_lossy()]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("  observed: git runs"),
        "{}",
        text(&output).0
    );
    assert!(sandbox
        .config()
        .join("repos/acme/gated/repo.toml")
        .is_file());
    assert!(started(&starts, "cat-file"), "{starts:#?}");
    budget("ohjaa adopt", starts.len(), 4 + 2);
    sandbox.adopt(&own);

    // apply with two entries, nothing to change: now it reads the clones.
    // Exactly git's version and global config, then one process per clone
    // under the roots (4, gated or not): each clone is read exactly once.
    let (events, output) = sandbox.git_events(&mut ohjaa_here(&["apply"]));
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    for clone in ["gated", "own", "free", "local-hooks"] {
        let worktree = format!(
            "\"worktree\":\"{}\"",
            sandbox.dir.join("projects").join(clone).display()
        );
        let reads = events
            .iter()
            .filter(|line| line.contains("\"event\":\"def_repo\"") && line.contains(&worktree))
            .count();
        assert_eq!(reads, 1, "apply read {clone} {reads} times, not once");
    }
    let starts = events
        .iter()
        .filter(|line| line.contains("\"event\":\"start\""))
        .count();
    assert_eq!(
        starts,
        2 + 4,
        "apply with entries: 2 + 1 per clone, exactly"
    );
    budget(
        "ohjaa apply (2 entries, 4 clones, no change)",
        starts,
        2 + 4,
    );

    // pre-commit, a hook that only chains: `git commit` through the
    // dispatcher, against the same commit with git running the own hook.
    let own_ran = || sandbox.read("own-pre-commit").lines().count();
    let commit = |direct: bool| {
        let mut command = sandbox.command("git", &own);
        if direct {
            command.args([
                "-c",
                &format!("core.hooksPath={}", own.join(".git/hooks").display()),
            ]);
        }
        command.args(["commit", "-q", "--allow-empty", "-m", "measure"]);
        let before = own_ran();
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(output.status.success(), "{}", text(&output).1);
        assert!(
            started(&starts, "commit"),
            "no git commit in the trace: {starts:#?}"
        );
        assert_eq!(own_ran(), before + 1, "the own pre-commit did not run");
        starts.len()
    };
    let through = commit(false);
    let direct = commit(true);
    budget(
        "ohjaa hook pre-commit (chains only)",
        through.saturating_sub(direct),
        0,
    );

    // pre-push: `git push` through the dispatcher, against the same push
    // with no hooks at all. Each pushes one new commit to the observed main.
    let init = sandbox.git(&gated, &["rev-parse", "HEAD"]);
    let calls = || sandbox.read("sykli-calls").lines().count();
    let push = |what: &str, hooks: bool| {
        sandbox.commit(&gated, what);
        let mut command = sandbox.command("git", &gated);
        if !hooks {
            command.args(["-c", "core.hooksPath=/dev/null"]);
        }
        command.args(["push", "-q", "origin", "HEAD:refs/heads/main"]);
        let before = calls();
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(output.status.success(), "{}", text(&output).1);
        assert!(
            started(&starts, "push"),
            "no git push in the trace: {starts:#?}"
        );
        assert_eq!(
            sandbox.git(&bare, &["rev-parse", "main"]),
            sandbox.git(&gated, &["rev-parse", "HEAD"]),
            "it landed"
        );
        (starts.len(), calls() - before, text(&output).1)
    };
    let (through, observe_calls, said) = push("measured", true);
    let (direct, no_calls, _) = push("plain", false);
    assert_eq!(no_calls, 0, "sykli ran without the dispatcher");
    assert!(observe_calls >= 1, "the push was never handed to sykli");
    assert!(said.contains("ohjaa: UNPROVEN push to main ("), "{said}");
    let facts = format!(
        "--repo-root {} --git-common-dir {}",
        gated.display(),
        gated.join(".git").display()
    );
    assert!(
        sandbox
            .read("sykli-calls")
            .contains(&format!("--branch main {facts} origin")),
        "{}",
        sandbox.read("sykli-calls")
    );
    budget(
        "ohjaa hook pre-push (observed)",
        through.saturating_sub(direct),
        1,
    );

    // ohjaa gate: one git process, then sykli's own, with the facts.
    let (starts, output) = sandbox.git_starts(sandbox.command("ohjaa", &gated).arg("gate"));
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("ohjaa gate: passed"),
        "{}",
        text(&output).0
    );
    assert!(sandbox
        .read("projects/gated/.sykli/receipt.json")
        .contains("passed"));
    let contract = sandbox.config().join("repos/acme/gated/sykli.json");
    let calls = sandbox.read("sykli-calls");
    assert!(
        calls.contains(&format!("|run {} {facts} --json", contract.display())),
        "{calls}"
    );
    assert!(calls.contains(&format!("{facts} --commit HEAD")), "{calls}");
    budget("ohjaa gate", starts.len(), 1);

    // doctor: two for the machine, at most two per clone.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["doctor", "--root", &root]));
    let (stdout, _) = text(&output);
    assert!(stdout.contains("ohjaa doctor: "), "{stdout}");
    for clone in [
        "gated (acme/gated)",
        "own (acme/own)",
        "local-hooks: ",
        "free",
    ] {
        assert!(
            stdout.contains(clone),
            "doctor did not look at {clone}: {stdout}"
        );
    }
    assert!(
        started(&starts, "--version") && started(&starts, "--global"),
        "{starts:#?}"
    );
    budget("ohjaa doctor (4 clones)", starts.len(), 2 + 2 * 4);

    // The marks need sykli once: the first commit on main is proven (a
    // receipt sykli kept), the two pushed above it are not.
    receipt(
        &gated.join(".git/sykli/receipts/rcpt_init.json"),
        &sandbox.tree(&gated, &init),
        "passed",
        1_000,
    );
    let verified = || {
        sandbox
            .read("sykli-calls")
            .lines()
            .filter(|line| line.contains("|verify ") && line.contains("--commit"))
            .count()
    };

    // ohjaa run's briefing for a clone that has drifted (a hook of its own
    // with no dispatcher) and whose main is unproven at its tip: a launch
    // with entries, beyond the same launch with no entries at all, which
    // reads no clone.
    write_executable(&gated.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    let launch = |sandbox: &Sandbox| {
        let before = verified();
        let (starts, output) = sandbox.git_starts(sandbox.command("ohjaa", &gated).args([
            "run",
            "claude",
            "--intent",
            "x",
            "--dry-run",
        ]));
        let (_, stderr) = text(&output);
        assert!(
            stderr.contains("ohjaa: dry-run; would run:"),
            "the launch stopped short: {stderr}"
        );
        let logs = starts
            .iter()
            .filter(|line| line.contains("\"log\"") && line.contains("refs/remotes/origin/main"))
            .count();
        (starts.len(), stderr, verified() - before, logs)
    };
    let (with, said, verifies, logs) = launch(&sandbox);
    assert!(
        said.contains("its own pre-commit hook has no dispatcher"),
        "no warning for the drifted clone: {said}"
    );
    assert!(
        said.contains("would run: ")
            && said.contains("--briefing 'FLAG: main of acme/gated is unproven since "),
        "no flag first in the briefing: {said}"
    );
    assert_eq!(logs, 1, "one walk, in one git log");
    assert_eq!(verifies, 1, "sykli verify, once");
    let repos = sandbox.config().join("repos");
    fs::rename(&repos, sandbox.config().join("repos.aside")).unwrap();
    let (without, said, none, _) = launch(&sandbox);
    fs::rename(sandbox.config().join("repos.aside"), &repos).unwrap();
    assert!(
        !said.contains("has no dispatcher") && !said.contains("FLAG"),
        "{said}"
    );
    assert_eq!(none, 0);
    budget(
        "ohjaa run's briefing (drift and marks)",
        with.saturating_sub(without),
        3,
    );
    budget("ohjaa run's briefing: sykli verify", verifies, 1);

    // ohjaa report: the marks of both entries, beyond the same report with
    // no entries; sykli asked only where a receipt says it passed (one).
    let reported = |sandbox: &Sandbox| {
        let before = verified();
        let (starts, output) = sandbox.git_starts(
            sandbox
                .command("ohjaa", &gated)
                .args(["report", "--root", &root]),
        );
        let (stdout, _) = text(&output);
        assert!(stdout.starts_with("ohjaa report, "), "{stdout}");
        let logs = starts
            .iter()
            .filter(|line| {
                line.contains("\"log\"")
                    && line.contains("refs/remotes/origin/main")
                    && line.contains("--max-count=50")
            })
            .count();
        // The clones whose config was read to find their entry.
        let listings = starts
            .iter()
            .filter(|line| line.contains("\"config\",\"-z\",\"--list\""))
            .count();
        (starts.len(), stdout, verified() - before, logs, listings)
    };
    let (with, stdout, verifies, logs, listings) = reported(&sandbox);
    for marked in [
        "- acme/gated main (never fetched here): ? ",
        "- acme/own main (never fetched here): ? ",
    ] {
        assert!(stdout.contains(marked), "no marks {marked:?}: {stdout}");
    }
    assert_eq!(logs, 2, "one walk per entry");
    assert_eq!(listings, 2, "the config of each entry's clone, read once");
    assert_eq!(verifies, 1, "one receipt says it passed");
    fs::rename(&repos, sandbox.config().join("repos.aside")).unwrap();
    let (without, stdout, _, no_logs, no_listings) = reported(&sandbox);
    fs::rename(sandbox.config().join("repos.aside"), &repos).unwrap();
    assert!(!stdout.contains("Marks on each"), "{stdout}");
    assert_eq!(
        (no_logs, no_listings),
        (0, 0),
        "no entry: no clone read for one"
    );
    // The marks' own processes: each entry's clone config, and its walk.
    budget("ohjaa report's marks (2 entries)", logs + listings, 2 * 2);
    // The whole report, beyond the same one with no entries: the gate also
    // spares the window the default-branch lookup and the contract checks.
    budget(
        "ohjaa report with 2 entries, beyond none",
        with.saturating_sub(without),
        2 * 2,
    );
    budget("ohjaa report's marks: sykli verify", verifies, 1);

    let over: Vec<_> = report
        .iter()
        .filter(|(_, starts, at_most)| starts > at_most)
        .collect();
    assert!(over.is_empty(), "over budget: {over:?}");
}

/// N1: what apply finds does not depend on where it runs. With no roots file
/// it does not guess (a home folder whose `.oh-my-zsh` is a repository would
/// pass for the projects folder): it refuses until --root names the folder,
/// writes that down, never removes a dispatcher, and --prune needs the roots
/// written down.
#[test]
fn apply_only_adds_dispatchers_and_writes_the_roots_it_scanned() {
    let sandbox = Sandbox::new("roots");
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    write_executable(&own.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    // A home folder with a repository in it, as ~/.oh-my-zsh is.
    let zsh = sandbox.home().join(".oh-my-zsh");
    fs::create_dir_all(&zsh).unwrap();
    sandbox.git(&zsh, &["init", "-q"]);

    // No roots file and no --root: refused, from anywhere, dry run or not,
    // naming the fix and what it would have used.
    for (cwd, args) in [
        (sandbox.home(), &["apply"][..]),
        (sandbox.home(), &["apply", "--dry-run"][..]),
        (sandbox.dir.join("projects"), &["apply"][..]),
    ] {
        let output = sandbox.ohjaa_in(&cwd, args);
        let (stdout, stderr) = text(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} in {}: {stdout}{stderr}",
            cwd.display()
        );
        assert!(stderr.contains("ohjaa apply --root <dir>"), "{stderr}");
        assert!(
            stderr.contains(&format!("would have used {}", cwd.display())),
            "{stderr}"
        );
    }
    assert!(!sandbox.config().exists(), "nothing written");
    assert!(!sandbox.dir.join("gitconfig").exists(), "nothing written");

    // With --root: the roots are written down.
    let output = sandbox.ohjaa_in(&sandbox.home(), &["apply", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "wrote {} with {}",
            sandbox.config().join("roots").display(),
            sandbox.projects()
        )),
        "{stdout}"
    );
    sandbox.adopt(&own);
    let hooks = sandbox.entry_hooks("acme", "own");
    assert_eq!(names_in(&hooks), ["pre-commit", "pre-push"]);

    // From the home folder: the roots file decides, and nothing is removed.
    let output = sandbox.ohjaa_in(&sandbox.home(), &["apply"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(names_in(&hooks), ["pre-commit", "pre-push"]);
    let output = sandbox.ohjaa_in(&sandbox.home(), &["doctor"]);
    assert!(
        text(&output).0.contains(&format!(
            "roots: {} (from {})",
            sandbox.projects(),
            sandbox.config().join("roots").display()
        )),
        "{}",
        text(&output).0
    );

    // --prune refuses without a roots file; with one, it removes spares.
    fs::rename(
        sandbox.config().join("roots"),
        sandbox.dir.join("roots.aside"),
    )
    .unwrap();
    let output = sandbox.ohjaa(&["apply", "--prune", "--root", &sandbox.projects()]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output).1);
    assert!(text(&output).1.contains("refusing --prune"));
    fs::rename(
        sandbox.dir.join("roots.aside"),
        sandbox.config().join("roots"),
    )
    .unwrap();
    fs::remove_file(own.join(".git/hooks/pre-commit")).unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        names_in(&hooks),
        ["pre-commit", "pre-push"],
        "a spare is kept"
    );
    let output = sandbox.ohjaa(&["doctor"]);
    assert!(
        text(&output).0.contains("spare dispatchers in"),
        "{}",
        text(&output).0
    );
    let output = sandbox.ohjaa(&["apply", "--prune"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(names_in(&hooks), ["pre-push"]);
}

/// The ohjaa the hooks run is the one on PATH, unless --bin names another.
#[test]
fn apply_runs_the_ohjaa_on_path_unless_bin_names_another() {
    let sandbox = Sandbox::new("bin");
    let (repo, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    let other = sandbox.dir.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ohjaa"), other.join("ohjaa")).unwrap();
    let elsewhere = other.join("ohjaa").to_string_lossy().into_owned();
    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply", "--root", &sandbox.projects()])
        .output()
        .unwrap();
    let (_, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("is not the one on PATH"), "{stderr}");
    assert!(
        !sandbox.config().join("git").exists(),
        "nothing half applied"
    );

    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply", "--root", &sandbox.projects(), "--bin", &elsewhere])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let dispatcher =
        fs::read_to_string(sandbox.entry_hooks("acme", "widget").join("pre-push")).unwrap();
    assert!(
        dispatcher.contains(&format!("exec '{elsewhere}' hook pre-push")),
        "{dispatcher}"
    );
    assert!(fs::read_to_string(sandbox.home().join(".zshrc"))
        .unwrap()
        .contains(&format!("[[ -x '{elsewhere}' ]]")));
    assert!(sandbox.gated(&repo));
}

/// A git client without the shell's PATH (a GUI) still has its push observed:
/// the dispatcher names ohjaa and sykli by the paths apply found.
#[test]
fn a_push_from_a_client_without_the_path_is_still_observed() {
    let mut sandbox = Sandbox::new("gui");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    let head = sandbox.commit(&repo, "change");
    let push = |sandbox: &Sandbox| {
        sandbox
            .command("/usr/bin/git", &repo)
            .env("PATH", "/usr/bin:/bin")
            .args(["push", "origin", "main"])
            .output()
            .unwrap()
    };
    let output = push(&sandbox);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (no receipt in this worktree"),
        "sykli was asked: {stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    sandbox.commit(&repo, "proven");
    let output = push(&sandbox);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
}

/// The ignore block holds `.sykli/` and `.teko/`. A block an earlier apply
/// wrote with `.sykli/` only is brought up to date in place, and undo still
/// puts back exactly what was there before the first apply.
#[test]
fn an_older_ignore_block_is_upgraded_in_place_and_undo_is_exact() {
    let sandbox = Sandbox::new("teko");
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    sandbox.apply();
    let ignore = sandbox.read("config/git/ignore");
    assert!(ignore.contains("\n.sykli/\n.teko/\n"), "{ignore}");
    sandbox.git(&repo, &["check-ignore", "-q", ".teko/state.json"]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);

    // As an earlier apply left it: .sykli/ only.
    let older = ignore.replace(".sykli/\n.teko/\n", ".sykli/\n");
    fs::write(sandbox.dir.join("config/git/ignore"), &older).unwrap();
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, _) = text(&output);
    assert!(
        stdout.contains("holds an older ohjaa block; it should ignore .sykli/ .teko/"),
        "{stdout}"
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert!(stdout.contains("added .sykli/ .teko/ to"), "{stdout}");
    assert_eq!(
        sandbox.read("config/git/ignore"),
        ignore,
        "upgraded in place"
    );

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp\n");
}
