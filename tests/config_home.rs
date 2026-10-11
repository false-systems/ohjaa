//! The workflow home from outside the repositories: `apply`, `doctor`,
//! `adopt`, `hook`, `shell-init` and `tools`, run as the real binary against
//! real git.
//!
//! Every command runs with an emptied environment: HOME, XDG_CONFIG_HOME,
//! XDG_STATE_HOME, CARGO_HOME and TMPDIR inside the sandbox,
//! GIT_CONFIG_GLOBAL at a sandbox file, GIT_CONFIG_NOSYSTEM=1, no ZDOTDIR, and
//! a PATH of stubs (sykli, cargo, claude, codex) plus the system directories.
//! `apply` writes git's global config and ~/.zshrc, so nothing here may reach
//! the person's real ones, nor run a real agent or tool.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Never the real sykli. `--help` names `--contract` only with
/// STUB_SYKLI_CONTRACT set. The pre-push gate keeps its stdin and refuses a
/// push to main unless the repository has `.sykli/receipt.json`.
const SYKLI_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$SANDBOX/sykli-calls"
case "$1 $2" in
  "hook pre-push")
    if [ "$3" = --help ]; then
      echo "Usage: sykli hook pre-push [OPTIONS] [REMOTE] [URL]"
      [ -n "${STUB_SYKLI_CONTRACT:-}" ] && echo "      --contract <CONTRACT>  a contract outside the repository"
      exit 0
    fi
    cat > "$SANDBOX/sykli-stdin"
    [ -n "${STUB_SYKLI_CONTRACT:-}" ] || { echo "error: unexpected argument '--contract'" >&2; exit 2; }
    if grep -q " refs/heads/main " "$SANDBOX/sykli-stdin" && [ ! -f .sykli/receipt.json ]; then
      echo "sykli hook pre-push: refusing to push to refs/heads/main: no receipt of its tree" >&2
      exit 1
    fi
    exit 0;;
  "init --help")
    echo "Usage: sykli init [OPTIONS] [PATH]"
    [ -n "${STUB_SYKLI_CONTRACT:-}" ] && echo "      --contract <CONTRACT>  write the contract there"
    exit 0;;
  "init --contract")
    printf '{"version":"1","tasks":{}}\n' > "$3"
    exit 0;;
esac
exit 2
"#;

const CARGO_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$SANDBOX/cargo-calls"
"#;

const CLAUDE_STUB: &str = "#!/bin/sh\necho \"direct $*\"\n";
const CODEX_STUB: &str = "#!/bin/sh\necho \"direct-codex $*\"\n";

const OLD_ZSHRC: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
    # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
    [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
    alias ll='ls -l'\n";

const LAUNCH_LINE: &str = r#"eval "$(ohjaa shell-init zsh)""#;

const HOOKS: usize = 28;

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

struct Sandbox {
    dir: PathBuf,
    path: String,
    stub_contract: bool,
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
        write_executable(&bin.join("cargo"), CARGO_STUB);
        write_executable(&bin.join("claude"), CLAUDE_STUB);
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
            stub_contract: false,
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config").join("ohjaa")
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
        if self.stub_contract {
            command.env("STUB_SYKLI_CONTRACT", "1");
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
        let output = self.ohjaa(&["apply"]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }
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
    let hooks = sandbox.config().join("hooks");
    for said in [
        format!(
            "would write {HOOKS} hook dispatchers into {}",
            hooks.display()
        ),
        "pre-push".to_owned(),
        format!(
            "would set core.hooksPath = {} in git's global config (unset now)",
            hooks.display()
        ),
        format!(
            "would add .sykli/ to {}",
            sandbox.dir.join("config/git/ignore").display()
        ),
        format!("would change {}", sandbox.home().join(".zshrc").display()),
        "  - [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh".to_owned(),
        format!("  + {LAUNCH_LINE}"),
        "dry run; nothing was changed".to_owned(),
    ] {
        assert!(stdout.contains(&said), "missing {said:?} in:\n{stdout}");
    }
    assert!(!hooks.exists(), "no dispatcher written");
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
fn apply_applies_once_and_then_has_nothing_to_change() {
    let sandbox = Sandbox::new("apply");
    fs::write(sandbox.home().join(".zshrc"), OLD_ZSHRC).unwrap();
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    sandbox.apply();

    let hooks = sandbox.config().join("hooks");
    let written: Vec<_> = fs::read_dir(&hooks).unwrap().flatten().collect();
    assert_eq!(written.len(), HOOKS);
    for entry in written {
        let name = entry.file_name().to_string_lossy().into_owned();
        let body = fs::read_to_string(entry.path()).unwrap();
        assert!(body.starts_with("#!/bin/sh\n"), "{name}");
        assert!(
            body.ends_with(&format!("exec ohjaa hook {name} \"$@\"\n")),
            "{name}: {body}"
        );
        assert!(executable(&entry.path()), "{name}");
    }
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get", "core.hooksPath"]
        ),
        hooks.to_string_lossy()
    );
    let ignore = sandbox.read("config/git/ignore");
    assert!(ignore.starts_with("*.swp\n"), "{ignore}");
    assert!(ignore.contains("\n.sykli/\n"), "{ignore}");
    // git init and git clone still work: while they create a repository,
    // git runs reference-transaction before rev-parse can see it.
    let (repo, bare) = sandbox.repo("acme", "any", &[]);
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "cloned"],
    );
    // Git reads the file apply wrote: .sykli/ is ignored in every repository.
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);
    let zshrc = fs::read_to_string(sandbox.home().join(".zshrc")).unwrap();
    assert!(!zshrc.contains("ohjaa-launch.zsh"), "{zshrc}");
    assert_eq!(zshrc.matches(LAUNCH_LINE).count(), 1, "{zshrc}");
    assert!(
        zshrc.starts_with("export PATH=\"$HOME/.local/bin:$PATH\"\n"),
        "{zshrc}"
    );
    assert!(
        zshrc.ends_with(&format!("{LAUNCH_LINE}\nalias ll='ls -l'\n")),
        "{zshrc}"
    );

    let before = (
        sandbox.read("gitconfig"),
        sandbox.read("config/git/ignore"),
        zshrc,
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("nothing to change"), "{stdout}");
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
    let output = sandbox.ohjaa(&[
        "doctor",
        "--root",
        &sandbox.dir.join("projects").to_string_lossy(),
    ]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("DRIFT  hook dispatchers edited or not executable: pre-push"),
        "{stdout}"
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert!(stdout.contains("wrote 1 hook dispatcher into"), "{stdout}");
}

#[test]
fn apply_refuses_another_global_hooks_path_unless_forced() {
    let sandbox = Sandbox::new("refuse");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "core.hooksPath", "/elsewhere/hooks"],
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(
        stderr.contains("core.hooksPath is /elsewhere/hooks"),
        "{stderr}"
    );
    assert!(stderr.contains("ohjaa apply --force"), "{stderr}");
    assert!(
        !sandbox.config().join("hooks").exists(),
        "nothing half applied"
    );
    assert!(
        !sandbox.home().join(".zshrc").exists(),
        "nothing half applied"
    );

    let output = sandbox.ohjaa(&["apply", "--force"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("replacing /elsewhere/hooks"), "{stdout}");
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get", "core.hooksPath"]
        ),
        sandbox.config().join("hooks").to_string_lossy()
    );
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
    // Someone's uncommitted work: the gate comes from origin/main, not from it.
    fs::write(repo.join("sykli.json"), "{\"edited\":true}\n").unwrap();
    fs::write(repo.join("notes.txt"), "untracked\n").unwrap();
    fs::create_dir_all(repo.join("target")).unwrap();
    fs::write(repo.join("target/x"), "ignored\n").unwrap();
    let before = sandbox.status(&repo);

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let entry = sandbox.config().join("repos/acme/widget");
    assert!(stdout.contains(&entry.display().to_string()), "{stdout}");
    assert!(stdout.contains("copied from origin/main"), "{stdout}");
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
    assert!(!stderr.contains("changed while adopting"), "{stderr}");

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

    sandbox.stub_contract = true;
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
}

#[test]
fn a_push_to_main_without_a_receipt_is_refused_through_the_global_dispatcher() {
    let mut sandbox = Sandbox::new("gate");
    sandbox.stub_contract = true;
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    assert_eq!(
        sandbox
            .ohjaa(&["adopt", &repo.to_string_lossy()])
            .status
            .code(),
        Some(0)
    );
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);

    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("refusing to push to refs/heads/main"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ohjaa hook pre-push: the gate for acme/widget"),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", "main"]),
        landed,
        "nothing landed"
    );
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|hook pre-push --contract {} origin {}",
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

    // With a receipt (in .sykli/, which every repository now ignores) it lands.
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
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
fn without_a_sykli_that_takes_a_contract_the_gated_branch_is_refused_and_others_pass() {
    let sandbox = Sandbox::new("no-contract");
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    fs::create_dir_all(sandbox.config().join("repos/acme/widget")).unwrap();
    fs::write(
        sandbox.config().join("repos/acme/widget/sykli.json"),
        "{}\n",
    )
    .unwrap();
    fs::write(
        sandbox.config().join("repos/acme/widget/repo.toml"),
        "branch = \"main\"\n",
    )
    .unwrap();
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);
    sandbox.commit(&repo, "change");

    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("refusing to push"), "{stderr}");
    assert!(
        stderr.contains("refs/heads/main of acme/widget"),
        "{stderr}"
    );
    assert!(stderr.contains("names no --contract"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), landed);

    fs::remove_file(sandbox.dir.join("bin/sykli")).unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("sykli is not on PATH"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), landed);
}

#[test]
fn an_entry_that_cannot_be_read_refuses_the_push() {
    let sandbox = Sandbox::new("broken-entry");
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.apply();
    fs::create_dir_all(sandbox.config().join("repos/acme/widget")).unwrap();
    fs::write(
        sandbox.config().join("repos/acme/widget/sykli.json"),
        "{}\n",
    )
    .unwrap();
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);
    sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/feature"]);
    let (_, stderr) = text(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("cannot be read"), "{stderr}");
    assert!(stderr.contains("no repo.toml"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), landed);
}

#[test]
fn a_repo_without_an_entry_pushes_freely() {
    let sandbox = Sandbox::new("free");
    let (repo, bare) = sandbox.repo("acme", "free", &[("sykli.json", "{}\n")]);
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
    sandbox.stub_contract = true;
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
    assert_eq!(
        sandbox
            .ohjaa(&["adopt", &repo.to_string_lossy()])
            .status
            .code(),
        Some(0)
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

    // pre-push: the gate and the own hook read the same stdin.
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
fn doctor_reports_drift_before_and_health_after() {
    let sandbox = Sandbox::new("doctor");
    let projects = sandbox.dir.join("projects");
    let root = projects.to_string_lossy().into_owned();
    let (gated, _) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own-hooks", &[]);
    sandbox.git(&own, &["config", "core.hooksPath", ".githooks"]);
    sandbox.repo("acme", "older", &[("sykli.json", "{}\n")]);
    sandbox.repo("acme", "plain", &[]);

    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    for said in [
        "DRIFT  git's global core.hooksPath is unset",
        &format!("DRIFT  {HOOKS} of {HOOKS} hook dispatchers are missing"),
        "lacks the ohjaa block (.sykli/)",
        "does not load the launcher",
        "DRIFT  no tools.lock",
        "note   own-hooks (acme/own-hooks): core.hooksPath is .githooks in the repo: \
         workflow gate not applied here: the repo's own hooks run",
        "note   older (acme/older): an older in-repo sykli.json and no entry",
        "note   gated (acme/gated): an older in-repo sykli.json and no entry",
        "no entry: plain",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    sandbox.apply();
    assert_eq!(
        sandbox
            .ohjaa(&["adopt", &gated.to_string_lossy()])
            .status
            .code(),
        Some(0)
    );
    let sha = "95bc5921a1b2c3d4e5f60718293a4b5c6d7e8f90";
    let output = sandbox.ohjaa(&[
        "tools",
        "pin",
        "sykli",
        "--git",
        "ssh://git@example.invalid/acme/sykli.git",
        "--rev",
        sha,
        "--toolchain",
        "1.98",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    fs::write(
        sandbox.dir.join("cargo/.crates.toml"),
        format!(
            "[v1]\n\"sykli 0.6.0 (git+ssh://git@example.invalid/acme/sykli.git?rev={sha}#{sha})\" = [\"sykli\"]\n"
        ),
    )
    .unwrap();
    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for said in [
        "ok     git's global core.hooksPath is",
        &format!("ok     all {HOOKS} hook dispatchers are current"),
        "ok     sykli at 95bc5921a1b2",
        "ok     acme/gated: gates main",
        "ok     gated (acme/gated): gated on main",
        "ohjaa doctor: healthy",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    // An entry for a repository whose own core.hooksPath wins is drift.
    let entry = sandbox.config().join("repos/acme/own-hooks");
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("repo.toml"), "branch = \"main\"\n").unwrap();
    fs::write(entry.join("sykli.json"), "{}\n").unwrap();
    // A path install is not the pinned commit.
    fs::write(
        sandbox.dir.join("cargo/.crates.toml"),
        "[v1]\n\"sykli 0.6.0 (path+file:///src/sykli)\" = [\"sykli\"]\n",
    )
    .unwrap();
    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(stdout.contains(
        "DRIFT  own-hooks (acme/own-hooks): has an entry, but its own core.hooksPath (.githooks) \
         overrides the global one: workflow gate not applied here: the repo's own hooks run"
    ), "{stdout}");
    assert!(
        stdout.contains("DRIFT  sykli is unpinned: installed from the path /src/sykli"),
        "{stdout}"
    );
}

#[test]
fn tools_are_pinned_from_a_clone_and_installed_with_their_toolchain() {
    let sandbox = Sandbox::new("tools");
    let clone = sandbox.dir.join("projects/sykli");
    fs::create_dir_all(&clone).unwrap();
    sandbox.git(&clone, &["init", "-q", "-b", "main"]);
    fs::write(
        clone.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.98\"\ncomponents = [\"rustfmt\"]\n",
    )
    .unwrap();
    sandbox.git(&clone, &["add", "-A"]);
    sandbox.git(&clone, &["commit", "-q", "-m", "init"]);
    sandbox.git(
        &clone,
        &[
            "remote",
            "add",
            "origin",
            "git@example.invalid:acme/sykli.git",
        ],
    );
    sandbox.git(&clone, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    sandbox.git(
        &clone,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let sha = sandbox.git(&clone, &["rev-parse", "HEAD"]);

    let output = sandbox.ohjaa(&["tools", "pin", "sykli", "--from", &clone.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let lock = fs::read_to_string(sandbox.config().join("tools.lock")).unwrap();
    for line in [
        "schema = \"ohjaa-tools-lock.v1\"",
        "[tools.sykli]",
        "git = \"ssh://git@example.invalid/acme/sykli.git\"",
        &format!("rev = \"{sha}\""),
        "toolchain = \"1.98\"",
    ] {
        assert!(lock.contains(line), "missing {line:?} in:\n{lock}");
    }
    let output = sandbox.ohjaa(&[
        "tools",
        "pin",
        "toimija",
        "--git",
        "ssh://h/o/toimija",
        "--rev",
        "main",
        "--toolchain",
        "1.98",
    ]);
    assert_ne!(output.status.code(), Some(0));
    assert!(text(&output).1.contains("not a full commit id"));

    sandbox.apply();
    let install = format!(
        "+1.98 install --git ssh://git@example.invalid/acme/sykli.git --rev {sha} --locked --force sykli"
    );
    let output = sandbox.ohjaa(&["apply", "--tools", "--dry-run"]);
    let (stdout, _) = text(&output);
    assert!(stdout.contains("would install sykli at"), "{stdout}");
    assert!(
        stdout.contains(&format!("(not installed): cargo {install}")),
        "{stdout}"
    );
    assert_eq!(
        sandbox.read("cargo-calls"),
        "",
        "a dry run installs nothing"
    );

    let output = sandbox.ohjaa(&["apply", "--tools"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let calls = sandbox.read("cargo-calls");
    let (cwd, args) = calls.trim().split_once('|').unwrap();
    assert_eq!(args, install);
    assert!(
        cwd.starts_with(&sandbox.dir.join("tmp").to_string_lossy().into_owned()),
        "cargo runs from an empty directory of its own, not a repository: {cwd}"
    );

    // Installed at the pin: nothing to do.
    fs::write(
        sandbox.dir.join("cargo/.crates.toml"),
        format!("[v1]\n\"sykli 0.6.0 (git+ssh://git@example.invalid/acme/sykli.git?rev={sha}#{sha})\" = [\"sykli\"]\n"),
    )
    .unwrap();
    let output = sandbox.ohjaa(&["apply", "--tools"]);
    assert!(text(&output).0.contains("nothing to change"));
    assert_eq!(sandbox.read("cargo-calls").lines().count(), 1);

    let output = sandbox.ohjaa(&["tools", "unpin", "sykli"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(!fs::read_to_string(sandbox.config().join("tools.lock"))
        .unwrap()
        .contains("[tools.sykli]"));
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
