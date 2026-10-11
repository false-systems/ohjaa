//! Applying the workflow from outside the repositories: `ohjaa apply`,
//! `ohjaa doctor`, `ohjaa adopt` and `ohjaa shell-init`.
//!
//! `apply` changes four things outside every repository, and only those:
//! the dispatchers in the home's `hooks/`, git's global `core.hooksPath`, a
//! marked block in git's global excludes file, and one line in `~/.zshrc`.
//! `apply --tools` also installs `tools.lock`'s revisions. `doctor` reads the
//! same things and says what has drifted. `adopt` writes a repository's entry
//! into the home. None of them writes into a repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::home::{self, RepoId};
use crate::hook::{dispatcher, HOOK_NAMES};
use crate::process;
use crate::tools::{self, Pin};

/// The launch or the change was refused.
const REFUSED: i32 = 3;
/// `doctor`: something differs from what `apply` would make it.
const DRIFT: i32 = 4;

/// The launcher, printed by `ohjaa shell-init zsh`. Its rules are the
/// person's own: see the comments in the file.
const LAUNCH_ZSH: &str = include_str!("launch.zsh");

pub const LAUNCH_LINE: &str = r#"eval "$(ohjaa shell-init zsh)""#;
const LAUNCH_COMMENT: &str = "# Route claude/codex through ohjaa (written by `ohjaa apply`). \
                              Bypass: command claude, or OHJAA_WRAP=0.";
/// The line that sourced the launcher before it was part of ohjaa.
const OLD_LAUNCHER: &str = "ohjaa-launch.zsh";

const IGNORE_BEGIN: &str =
    "# >>> ohjaa apply: workflow files no repository tracks (edits inside are replaced)";
const IGNORE_END: &str = "# <<< ohjaa apply";
/// What every repository ignores without being told: the workflow's own files.
const IGNORED: &[&str] = &[".sykli/"];

pub fn shell_init(shell: &str) -> Result<i32> {
    if shell != "zsh" {
        eprintln!("ohjaa shell-init: only zsh is supported, not {shell}");
        return Ok(2);
    }
    print!("{LAUNCH_ZSH}");
    Ok(0)
}

/// `current` with the managed block holding exactly [`IGNORED`]: replaced
/// where it is, else appended. An opened block that never closes is an
/// error, since replacing it could take lines the person wrote.
fn ignore_with_block(current: &str) -> Result<String, String> {
    let mut block = vec![IGNORE_BEGIN.to_owned()];
    block.extend(IGNORED.iter().map(|line| (*line).to_owned()));
    block.push(IGNORE_END.to_owned());
    let lines: Vec<&str> = current.lines().collect();
    let begin = lines
        .iter()
        .position(|line| line.trim_end().starts_with("# >>> ohjaa apply"));
    let out: Vec<String> = match begin {
        Some(begin) => {
            let end = lines[begin..]
                .iter()
                .position(|line| line.trim_end() == IGNORE_END)
                .map(|offset| begin + offset)
                .ok_or_else(|| {
                    format!(
                        "line {}: an ohjaa block that never closes with {IGNORE_END:?}; fix it by hand",
                        begin + 1
                    )
                })?;
            let mut out: Vec<String> = lines[..begin].iter().map(|l| (*l).to_owned()).collect();
            out.extend(block);
            out.extend(lines[end + 1..].iter().map(|l| (*l).to_owned()));
            out
        }
        None => {
            let mut out: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
            if out.last().is_some_and(|line| !line.trim().is_empty()) {
                out.push(String::new());
            }
            out.extend(block);
            out
        }
    };
    Ok(out.join("\n") + "\n")
}

/// A `~/.zshrc` that loads the launcher through ohjaa.
#[derive(Debug, PartialEq)]
struct ZshrcPlan {
    contents: String,
    removed: Vec<String>,
    added: Vec<String>,
}

fn sources_old_launcher(line: &str) -> bool {
    !line.trim_start().starts_with('#') && line.contains(OLD_LAUNCHER)
}

/// `current` with the old `source …/ohjaa-launch.zsh` line (and the comment
/// line just above it) replaced by [`LAUNCH_LINE`], or with that line
/// appended when there was none. `None` when nothing needs to change.
fn zshrc_with_launcher(current: &str) -> Option<ZshrcPlan> {
    let lines: Vec<&str> = current.lines().collect();
    let has_launch = lines.iter().any(|line| line.trim() == LAUNCH_LINE);
    let mut drop = vec![false; lines.len()];
    for (at, line) in lines.iter().enumerate() {
        if sources_old_launcher(line) {
            drop[at] = true;
            if at > 0 {
                let above = lines[at - 1].trim_start();
                if above.starts_with('#') && above.to_lowercase().contains("ohjaa") {
                    drop[at - 1] = true;
                }
            }
        }
    }
    if has_launch && !drop.contains(&true) {
        return None;
    }
    let added: Vec<String> = if has_launch {
        Vec::new()
    } else {
        vec![LAUNCH_COMMENT.to_owned(), LAUNCH_LINE.to_owned()]
    };
    let mut out: Vec<String> = Vec::new();
    let mut removed = Vec::new();
    let mut placed = has_launch;
    for (at, line) in lines.iter().enumerate() {
        if drop[at] {
            removed.push((*line).to_owned());
            if !placed {
                out.extend(added.iter().cloned());
                placed = true;
            }
        } else {
            out.push((*line).to_owned());
        }
    }
    if !placed {
        if out.last().is_some_and(|line| !line.trim().is_empty()) {
            out.push(String::new());
        }
        out.extend(added.iter().cloned());
    }
    Some(ZshrcPlan {
        contents: out.join("\n") + "\n",
        removed,
        added,
    })
}

/// The first `name` on PATH, as a shell would find it.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(path)
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                path.is_file()
            }
        })
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// Dispatchers that are missing, and those that differ from what `apply`
/// writes (edited, or not executable).
fn dispatcher_drift(dir: &Path) -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut missing, mut stale) = (Vec::new(), Vec::new());
    for name in HOOK_NAMES {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Err(_) => missing.push(name),
            Ok(text) => {
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&path)
                        .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
                };
                #[cfg(not(unix))]
                let executable = true;
                if text != dispatcher(name) || !executable {
                    stale.push(name);
                }
            }
        }
    }
    (missing, stale)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

enum Change {
    Dispatchers {
        dir: PathBuf,
        names: Vec<&'static str>,
    },
    HooksPath {
        from: Option<String>,
        to: String,
    },
    Ignore {
        path: PathBuf,
        contents: String,
    },
    Zshrc {
        path: PathBuf,
        plan: ZshrcPlan,
    },
    Install {
        pin: Pin,
        why: String,
    },
}

impl Change {
    fn describe(&self, done: bool) -> String {
        let (will, set, add, change, install) = if done {
            ("wrote", "set", "added", "changed", "installing")
        } else {
            (
                "would write",
                "would set",
                "would add",
                "would change",
                "would install",
            )
        };
        match self {
            Change::Dispatchers { dir, names } => format!(
                "{will} {} hook dispatcher{} into {} (exec ohjaa hook <name> \"$@\"): {}",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                dir.display(),
                names.join(", ")
            ),
            Change::HooksPath { from, to } => format!(
                "{set} core.hooksPath = {to} in git's global config ({})",
                match from {
                    Some(from) => format!("replacing {from}"),
                    None => "unset now".to_owned(),
                }
            ),
            Change::Ignore { path, .. } => format!(
                "{add} {} to {} (git's global excludes), in a block marked \"ohjaa apply\"",
                IGNORED.join(" "),
                path.display()
            ),
            Change::Zshrc { path, plan } => {
                let mut text = format!("{change} {}:", path.display());
                for line in &plan.removed {
                    text.push_str(&format!("\n  - {line}"));
                }
                for line in &plan.added {
                    text.push_str(&format!("\n  + {line}"));
                }
                text
            }
            Change::Install { pin, why } => format!(
                "{install} {} at {} ({why}): {}",
                pin.name,
                short(&pin.rev),
                crate::render_command(&tools::install_command(pin))
            ),
        }
    }
}

pub fn apply(dry_run: bool, force: bool, with_tools: bool) -> Result<i32> {
    let home = absolute(home::ohjaa_home()?);
    let hooks = home::hooks_dir(&home);
    let hooks_text = hooks.to_string_lossy().into_owned();
    let mut changes = Vec::new();

    // Decide everything first: a refusal leaves nothing half applied.
    let current = home::global_path_setting("core.hooksPath")?.filter(|v| !v.is_empty());
    match current {
        Some(path) if home::same_path(Path::new(&path), &hooks) => {}
        Some(path) if !force => {
            eprintln!(
                "ohjaa apply: refusing: git's global core.hooksPath is {path}, not {hooks_text}. \
                 Something else runs every repository's hooks from there; replacing it would \
                 stop that. Move what it does into the repositories' own hooks (ohjaa's \
                 dispatchers chain to them), or replace it with: ohjaa apply --force"
            );
            return Ok(REFUSED);
        }
        from => changes.push(Change::HooksPath {
            from,
            to: hooks_text.clone(),
        }),
    }
    let (missing, stale) = dispatcher_drift(&hooks);
    let names: Vec<&'static str> = HOOK_NAMES
        .into_iter()
        .filter(|name| missing.contains(name) || stale.contains(name))
        .collect();
    if !names.is_empty() {
        changes.insert(
            0,
            Change::Dispatchers {
                dir: hooks.clone(),
                names,
            },
        );
    }
    let ignore = home::global_ignore_file()?;
    let ignore_now = read_or_empty(&ignore)?;
    let ignore_new = ignore_with_block(&ignore_now)
        .map_err(|why| anyhow::anyhow!("{}: {why}", ignore.display()))?;
    if ignore_new != ignore_now {
        changes.push(Change::Ignore {
            path: ignore,
            contents: ignore_new,
        });
    }
    let zshrc = home::zshrc()?;
    if let Some(plan) = zshrc_with_launcher(&read_or_empty(&zshrc)?) {
        changes.push(Change::Zshrc { path: zshrc, plan });
    }
    if with_tools {
        let lock = home::tools_lock(&home);
        let pins = tools::read_lock(&lock)?.with_context(|| {
            format!(
                "no {}: pin the tools first, e.g. ohjaa tools pin sykli --from ~/projects/sykli",
                lock.display()
            )
        })?;
        let (_, installed) = tools::installed()?;
        for pin in pins {
            if let Err(why) = tools::standing(&pin, installed.get(&pin.name)) {
                changes.push(Change::Install { pin, why });
            }
        }
    }

    if changes.is_empty() {
        println!(
            "ohjaa apply: nothing to change; the workflow is applied from {}",
            home.display()
        );
        return Ok(0);
    }
    if dry_run {
        for change in &changes {
            println!("ohjaa apply: {}", change.describe(false));
        }
        println!("ohjaa apply: dry run; nothing was changed");
        return Ok(0);
    }
    let mut failed = Vec::new();
    for change in &changes {
        if let Change::Install { pin, .. } = change {
            println!("ohjaa apply: {}", change.describe(true));
            if let Err(error) = install(pin) {
                eprintln!("ohjaa apply: {} was not installed: {error:#}", pin.name);
                failed.push(pin.name.clone());
            }
            continue;
        }
        match change {
            Change::Dispatchers { dir, names } => {
                for name in names {
                    home::write_file(&dir.join(name), &dispatcher(name), Some(0o755))?;
                }
            }
            Change::HooksPath { to, .. } => home::set_global("core.hooksPath", to)?,
            Change::Ignore { path, contents } => home::write_file(path, contents, None)?,
            Change::Zshrc { path, plan } => home::write_file(path, &plan.contents, None)?,
            Change::Install { .. } => unreachable!("installed above"),
        }
        println!("ohjaa apply: {}", change.describe(true));
    }
    if on_path("ohjaa").is_none() {
        eprintln!(
            "ohjaa apply: warning: ohjaa is not on PATH here; every git hook now runs \
             `ohjaa hook`, and fails where git cannot find it"
        );
    }
    if !failed.is_empty() {
        eprintln!("ohjaa apply: not installed: {}", failed.join(", "));
        return Ok(1);
    }
    Ok(0)
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// `cargo install` from an empty directory of its own, so no repository's
/// `rust-toolchain.toml` or `.cargo/config.toml` decides how it builds.
fn install(pin: &Pin) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("ohjaa-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let status = tools::install_command(pin)
        .current_dir(&dir)
        .status()
        .context("cannot run cargo");
    let _ = std::fs::remove_dir(&dir);
    let status = status?;
    if !status.success() {
        bail!("cargo exited {status}");
    }
    Ok(())
}

/// What `doctor` says, line by line, and how much of it is drift.
struct Findings {
    drift: usize,
}

impl Findings {
    fn ok(&self, what: impl AsRef<str>) {
        println!("  ok     {}", what.as_ref());
    }
    fn drift(&mut self, what: impl AsRef<str>) {
        self.drift += 1;
        println!("  DRIFT  {}", what.as_ref());
    }
    fn note(&self, what: impl AsRef<str>) {
        println!("  note   {}", what.as_ref());
    }
}

pub fn doctor(roots: &[PathBuf], git_root: Option<&Path>) -> Result<i32> {
    let home = absolute(home::ohjaa_home()?);
    let hooks = home::hooks_dir(&home);
    let mut found = Findings { drift: 0 };
    println!("ohjaa doctor: the workflow home is {}", home.display());

    match on_path("ohjaa") {
        Some(path) => found.ok(format!("ohjaa is on PATH ({})", path.display())),
        None => found.drift("ohjaa is not on PATH: every dispatcher execs `ohjaa hook`"),
    }
    match home::global_path_setting("core.hooksPath") {
        Ok(Some(path)) if home::same_path(Path::new(&path), &hooks) => {
            found.ok(format!("git's global core.hooksPath is {path}"))
        }
        Ok(Some(path)) => found.drift(format!(
            "git's global core.hooksPath is {path}, not {}: the dispatchers do not run \
             (ohjaa apply --force)",
            hooks.display()
        )),
        Ok(None) => found.drift(format!(
            "git's global core.hooksPath is unset; it should be {} (ohjaa apply)",
            hooks.display()
        )),
        Err(error) => found.drift(format!(
            "cannot read git's global core.hooksPath: {error:#}"
        )),
    }
    let (missing, stale) = dispatcher_drift(&hooks);
    if missing.is_empty() && stale.is_empty() {
        found.ok(format!(
            "all {} hook dispatchers are current in {}",
            HOOK_NAMES.len(),
            hooks.display()
        ));
    }
    if !missing.is_empty() {
        found.drift(format!(
            "{} of {} hook dispatchers are missing in {}: {} (ohjaa apply)",
            missing.len(),
            HOOK_NAMES.len(),
            hooks.display(),
            missing.join(", ")
        ));
    }
    if !stale.is_empty() {
        found.drift(format!(
            "hook dispatchers edited or not executable: {} (ohjaa apply)",
            stale.join(", ")
        ));
    }
    match home::global_ignore_file() {
        Ok(path) => match read_or_empty(&path).map(|text| (ignore_with_block(&text), text)) {
            Ok((Ok(with), text)) if with == text => found.ok(format!(
                "{} ignores {} for every repository",
                path.display(),
                IGNORED.join(" ")
            )),
            Ok((Ok(_), _)) => found.drift(format!(
                "{} lacks the ohjaa block ({}) (ohjaa apply)",
                path.display(),
                IGNORED.join(" ")
            )),
            Ok((Err(why), _)) => found.drift(format!("{}: {why}", path.display())),
            Err(error) => found.drift(format!("{error:#}")),
        },
        Err(error) => found.drift(format!("cannot find git's global excludes file: {error:#}")),
    }
    match home::zshrc().and_then(|path| read_or_empty(&path).map(|text| (path, text))) {
        Ok((path, text)) => {
            if text.lines().any(|line| line.trim() == LAUNCH_LINE) {
                found.ok(format!(
                    "{} loads the launcher ({LAUNCH_LINE})",
                    path.display()
                ));
            } else {
                found.drift(format!(
                    "{} does not load the launcher: {LAUNCH_LINE} (ohjaa apply)",
                    path.display()
                ));
            }
            for (at, line) in text.lines().enumerate() {
                if sources_old_launcher(line) {
                    found.drift(format!(
                        "{} line {} still loads the old launcher: {} (ohjaa apply)",
                        path.display(),
                        at + 1,
                        line.trim()
                    ));
                }
            }
        }
        Err(error) => found.drift(format!("cannot read ~/.zshrc: {error:#}")),
    }

    let lock = home::tools_lock(&home);
    println!("tools ({}):", lock.display());
    match tools::read_lock(&lock) {
        Ok(None) => found.drift("no tools.lock: no tool revision is pinned (ohjaa tools pin)"),
        Ok(Some(pins)) if pins.is_empty() => found.drift("tools.lock pins no tool"),
        Ok(Some(pins)) => match tools::installed() {
            Ok((record, installed)) => {
                for pin in pins {
                    match tools::standing(&pin, installed.get(&pin.name)) {
                        Ok(()) => found.ok(format!("{} at {}", pin.name, short(&pin.rev))),
                        Err(why) => found.drift(format!(
                            "{} is {why}; tools.lock pins {} (ohjaa apply --tools; read from {})",
                            pin.name,
                            short(&pin.rev),
                            record.display()
                        )),
                    }
                }
            }
            Err(error) => found.drift(format!("cannot read what Cargo installed: {error:#}")),
        },
        Err(error) => found.drift(format!("{error:#}")),
    }

    println!("entries ({}):", home.join("repos").display());
    let mut entries = 0;
    for (slug, dir) in home::entry_dirs(&home)? {
        match home::read_entry(&dir) {
            Ok(Some(entry)) if entry.has_contract() => {
                entries += 1;
                found.ok(format!("{slug}: gates {}", entry.branch));
            }
            Ok(Some(entry)) => found.drift(format!(
                "{slug}: no gate ({} is missing)",
                entry.contract().display()
            )),
            Ok(None) => {}
            Err(error) => found.drift(format!("{slug}: {error:#}")),
        }
    }
    if entries == 0 {
        found.note("no repository has an entry yet (ohjaa adopt <repo>)");
    }

    let roots = crate::roots::load(roots, git_root);
    let repos = crate::roots::git_repos(&roots)?;
    println!(
        "repositories under {}:",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut plain = Vec::new();
    for repo in &repos {
        let label = repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.display().to_string());
        let id = home::git_value(&["remote", "get-url", "origin"], Some(repo))
            .ok()
            .flatten()
            .and_then(|url| RepoId::parse(&url));
        let entry = id.as_ref().map(|id| home::find_entry(&home, id));
        let local_hooks = home::git_value(
            &["config", "--local", "--get", "core.hooksPath"],
            Some(repo),
        )
        .ok()
        .flatten();
        let named = match &id {
            Some(id) => format!("{label} ({})", id.slug()),
            None => label.clone(),
        };
        match (&entry, &local_hooks) {
            (Some(Ok(Some(_))), Some(own)) => found.drift(format!(
                "{named}: has an entry, but its own core.hooksPath ({own}) overrides the \
                 global one: workflow gate not applied here: the repo's own hooks run"
            )),
            (_, Some(own)) => found.note(format!(
                "{named}: core.hooksPath is {own} in the repo: workflow gate not applied \
                 here: the repo's own hooks run"
            )),
            (Some(Ok(Some(entry))), None) if entry.has_contract() => {
                found.ok(format!("{named}: gated on {}", entry.branch))
            }
            (Some(Ok(Some(_))), None) => {
                found.drift(format!("{named}: has an entry without a gate"))
            }
            (Some(Err(error)), None) => found.drift(format!("{named}: {error:#}")),
            (None, None) => found.note(format!(
                "{named}: no origin remote, so no entry can match it"
            )),
            (Some(Ok(None)), None) if repo.join("sykli.json").is_file() => found.note(format!(
                "{named}: an older in-repo sykli.json and no entry (ohjaa adopt {})",
                repo.display()
            )),
            (Some(Ok(None)), None) => plain.push(label),
        }
    }
    if !plain.is_empty() {
        println!("  -      no entry: {}", plain.join(", "));
    }
    if found.drift == 0 {
        println!("ohjaa doctor: healthy");
        Ok(0)
    } else {
        println!(
            "ohjaa doctor: {} drift{}",
            found.drift,
            if found.drift == 1 { "" } else { "s" }
        );
        Ok(DRIFT)
    }
}

/// Run git in `cwd` and return its stdout as it is (a file's bytes).
fn git_bytes(args: &[&str], cwd: &Path) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    let output = process::run(&mut command, None, process::LOCAL).context("cannot run git")?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), output.failure_detail());
    }
    Ok(output.stdout)
}

/// A file as commit `rev` holds it, `None` when it holds none.
fn committed(root: &Path, rev: &str, file: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("{rev}:{file}");
    match home::git_value(&["rev-parse", "--verify", "--quiet", &spec], Some(root))? {
        Some(blob) => Ok(Some(git_bytes(&["cat-file", "blob", &blob], root)?)),
        None => Ok(None),
    }
}

/// What `git status` says, ignored paths too: adopt must leave it as it was.
/// Untracked and ignored directories are named, not listed file by file, so
/// a large ignored tree (`node_modules`) costs nothing.
fn status_of(root: &Path) -> Result<String> {
    Ok(home::git_value(
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignored",
        ],
        Some(root),
    )?
    .unwrap_or_default())
}

fn sykli_init_takes_a_contract() -> bool {
    Command::new("sykli")
        .args(["init", "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("--contract"))
}

pub fn adopt(path: &Path, branch: Option<&str>, force: bool) -> Result<i32> {
    let root = PathBuf::from(
        home::git_value(&["rev-parse", "--show-toplevel"], Some(path))
            .with_context(|| format!("{} is not inside a git repository", path.display()))?
            .with_context(|| format!("{} is not inside a git repository", path.display()))?,
    );
    let url = home::git_value(&["remote", "get-url", "origin"], Some(&root))
        .ok()
        .flatten()
        .with_context(|| format!("{} has no origin remote to name it by", root.display()))?;
    let id = RepoId::parse(&url)
        .with_context(|| format!("origin's URL {url:?} names no owner and repository"))?;
    let branch = match branch {
        Some(branch) => branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch)
            .to_owned(),
        None => home::git_value(
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            Some(&root),
        )?
        .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))
        .with_context(|| {
            format!(
                "origin/HEAD is not set in {}, so its default branch is unknown here; \
                 name it with --branch, or set it with: git remote set-head origin --auto",
                root.display()
            )
        })?,
    };
    let home = absolute(home::ohjaa_home()?);
    let dir = home::entry_dir(&home, &id);
    if dir.join("repo.toml").exists() && !force {
        eprintln!(
            "ohjaa adopt: refusing: {} is already adopted at {}; rewrite it with --force",
            id.slug(),
            dir.display()
        );
        return Ok(REFUSED);
    }
    let before = status_of(&root)?;
    let created = !dir.exists();
    let result = write_entry(&root, &id, &dir, &branch, force);
    if result.is_err() && created {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let source = result?;
    let after = status_of(&root)?;
    println!("ohjaa adopt: {} -> {}", id, dir.display());
    println!("  branch = {branch:?} (repo.toml)");
    println!("  {source}");
    if after != before {
        eprintln!(
            "ohjaa adopt: warning: git status in {} changed while adopting (another session, \
             or sykli); before:\n{before}\nafter:\n{after}",
            root.display()
        );
    }
    Ok(0)
}

/// Write the gate and `repo.toml` into `dir`; say where the gate came from.
fn write_entry(root: &Path, id: &RepoId, dir: &Path, branch: &str, force: bool) -> Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tracking = format!("refs/remotes/origin/{branch}");
    let rev = match home::git_value(&["rev-parse", "--verify", "--quiet", &tracking], Some(root))? {
        Some(_) => format!("origin/{branch}"),
        None => "HEAD".to_owned(),
    };
    let contract = dir.join("sykli.json");
    let lock = dir.join("sykli.lock");
    let source = if let Some(json) = committed(root, &rev, "sykli.json")? {
        std::fs::write(&contract, json)
            .with_context(|| format!("cannot write {}", contract.display()))?;
        let commit =
            home::git_value(&["rev-parse", "--short=12", &rev], Some(root))?.unwrap_or_default();
        match committed(root, &rev, "sykli.lock")? {
            Some(bytes) => {
                std::fs::write(&lock, bytes)
                    .with_context(|| format!("cannot write {}", lock.display()))?;
                format!("sykli.json and sykli.lock copied from {rev} ({commit})")
            }
            None => {
                let _ = std::fs::remove_file(&lock);
                format!(
                    "sykli.json copied from {rev} ({commit}), which has no sykli.lock; \
                     pin it with: sykli lock {}",
                    contract.display()
                )
            }
        }
    } else if committed(root, &rev, "sykli.rs")?.is_some() {
        bail!(
            "{} gates with sykli.rs at {rev}; an entry holds a sykli.json contract, so write \
             one (sykli init --contract {}) and adopt again",
            id.slug(),
            contract.display()
        );
    } else {
        if !sykli_init_takes_a_contract() {
            bail!(
                "{} has no sykli.json at {rev}, and the sykli on PATH cannot write a contract \
                 outside the repository (`sykli init --help` names no --contract); nothing \
                 was adopted",
                id.slug()
            );
        }
        let mut init = Command::new("sykli");
        init.args(["init", "--contract"])
            .arg(&contract)
            .current_dir(root);
        if force && contract.exists() {
            init.arg("--force");
        }
        let output = process::run(&mut init, None, process::LOCAL).context("cannot run sykli")?;
        if !output.status.success() {
            bail!("sykli init failed: {}", output.failure_detail());
        }
        if !contract.is_file() {
            bail!("sykli init did not write {}", contract.display());
        }
        format!(
            "sykli.json written by sykli init --contract (no sykli.json at {rev}){}",
            if lock.is_file() {
                ", with sykli.lock"
            } else {
                ""
            }
        )
    };
    home::write_file(&dir.join("repo.toml"), &home::repo_toml(id, branch), None)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
        # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
        [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
        alias ll='ls -l'\n";

    #[test]
    fn the_old_launcher_line_and_its_comment_are_replaced_in_place() {
        let plan = zshrc_with_launcher(OLD).unwrap();
        assert_eq!(
            plan.contents,
            format!(
                "export PATH=\"$HOME/.local/bin:$PATH\"\n{LAUNCH_COMMENT}\n{LAUNCH_LINE}\nalias ll='ls -l'\n"
            )
        );
        assert_eq!(plan.removed.len(), 2);
        assert_eq!(plan.added, [LAUNCH_COMMENT, LAUNCH_LINE]);
        assert_eq!(
            zshrc_with_launcher(&plan.contents),
            None,
            "applied twice: no change"
        );
    }

    #[test]
    fn without_an_old_line_the_launcher_is_appended() {
        let plan = zshrc_with_launcher("alias ll='ls -l'").unwrap();
        assert_eq!(
            plan.contents,
            format!("alias ll='ls -l'\n\n{LAUNCH_COMMENT}\n{LAUNCH_LINE}\n")
        );
        assert!(plan.removed.is_empty());
        let empty = zshrc_with_launcher("").unwrap();
        assert_eq!(empty.contents, format!("{LAUNCH_COMMENT}\n{LAUNCH_LINE}\n"));
    }

    #[test]
    fn a_commented_out_old_line_is_left_alone() {
        let text = format!("# source ~/projects/ohjaa-launch.zsh\n{LAUNCH_LINE}\n");
        assert_eq!(zshrc_with_launcher(&text), None);
    }

    #[test]
    fn the_ignore_block_is_added_once_and_replaced_in_place() {
        let block = format!("{IGNORE_BEGIN}\n.sykli/\n{IGNORE_END}\n");
        assert_eq!(ignore_with_block("").unwrap(), block);
        let mine = "*.swp\n.DS_Store\n";
        let added = ignore_with_block(mine).unwrap();
        assert_eq!(added, format!("{mine}\n{block}"));
        assert_eq!(ignore_with_block(&added).unwrap(), added, "applied twice");
        let old = format!("*.swp\n{IGNORE_BEGIN}\n.old/\n{IGNORE_END}\n.DS_Store\n");
        assert_eq!(
            ignore_with_block(&old).unwrap(),
            format!("*.swp\n{block}.DS_Store\n")
        );
        let open = format!("*.swp\n{IGNORE_BEGIN}\n.sykli/\n");
        assert!(ignore_with_block(&open)
            .unwrap_err()
            .contains("never closes"));
    }

    #[test]
    fn the_launcher_is_the_one_the_person_wrote() {
        // The rules are verbatim; only the header says how it is loaded now.
        for rule in [
            "[[ ${OHJAA_WRAP:-1} == 0 || -n $OHJAA_SESSION ]] && return 1",
            "_ohjaa_at_terminal || return 1",
            "git rev-parse --git-dir >/dev/null 2>&1 || [[ $PWD == $HOME/projects* ]] || return 1",
            "claude:-p|claude:--print|claude:-c|claude:--continue|claude:-r|claude:--resume) return 1 ;;",
            "_ohjaa_task=(--task \"$1\")",
            "command claude \"$@\"",
        ] {
            assert!(LAUNCH_ZSH.contains(rule), "{rule}");
        }
        assert!(LAUNCH_ZSH.contains(LAUNCH_LINE));
        assert!(!LAUNCH_ZSH.contains("source ~/projects/ohjaa-launch.zsh"));
    }
}
