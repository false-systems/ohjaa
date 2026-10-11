//! The workflow from outside the repositories: `ohjaa doctor`, `ohjaa adopt`
//! and `ohjaa shell-init` (`ohjaa apply` is in `apply`).
//!
//! `doctor` reads what `apply` writes and, clone by clone, whether git really
//! runs ohjaa's hooks where an entry says it should. `adopt` writes a
//! repository's entry into the home and regenerates the home's git config;
//! it never touches git's global config, and never writes into a repository.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::apply::{
    ignore_with_block, launch_target, read_or_empty, sources_old_launcher, IGNORED,
};
use crate::gate;
use crate::home::{self, RepoId};
use crate::hook::{dispatcher_target, HOOK_NAMES};
use crate::process;
use crate::tools;

/// The change was refused.
const REFUSED: i32 = 3;
/// `doctor`: something differs from what `apply` would make it.
const DRIFT: i32 = 4;

/// The launcher, printed by `ohjaa shell-init zsh`. Its rules are the
/// person's own: see the comments in the file.
const LAUNCH_ZSH: &str = include_str!("launch.zsh");

pub fn shell_init(shell: &str) -> Result<i32> {
    if shell != "zsh" {
        eprintln!("ohjaa shell-init: only zsh is supported, not {shell}");
        return Ok(2);
    }
    print!("{LAUNCH_ZSH}");
    Ok(0)
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
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

/// The hooks folder: which dispatchers there are, which are edited, and
/// which ohjaa they exec, by name.
fn check_dispatchers(found: &mut Findings, hooks: &Path, needed: &[&'static str]) {
    let mut present = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    for name in HOOK_NAMES {
        let Ok(text) = std::fs::read_to_string(hooks.join(name)) else {
            continue;
        };
        match dispatcher_target(name, &text) {
            Some(target) => {
                present.push(name);
                if !targets.contains(&target) {
                    targets.push(target);
                }
            }
            None => found.drift(format!(
                "{} is not one of apply's dispatchers (edited?) (ohjaa apply)",
                hooks.join(name).display()
            )),
        }
        if !gate::executable(&hooks.join(name)) {
            found.drift(format!(
                "{} is not executable (ohjaa apply)",
                hooks.join(name).display()
            ));
        }
    }
    let missing: Vec<&str> = needed
        .iter()
        .copied()
        .filter(|name| !present.contains(name))
        .collect();
    let extra: Vec<&str> = present
        .iter()
        .copied()
        .filter(|name| !needed.contains(name))
        .collect();
    if !missing.is_empty() {
        found.drift(format!(
            "dispatchers missing in {}: {} (ohjaa apply)",
            hooks.display(),
            missing.join(", ")
        ));
    }
    if !extra.is_empty() {
        found.drift(format!(
            "dispatchers no gated clone needs in {}: {} (ohjaa apply removes them)",
            hooks.display(),
            extra.join(", ")
        ));
    }
    for target in &targets {
        if gate::executable(target) {
            found.ok(format!(
                "dispatchers in {} ({}) exec {}",
                hooks.display(),
                present.join(", "),
                target.display()
            ));
        } else {
            found.drift(format!(
                "the dispatchers exec {}, which is gone: every gated push fails (ohjaa apply)",
                target.display()
            ));
        }
    }
}

pub fn doctor(roots: &[PathBuf], git_root: Option<&Path>) -> Result<i32> {
    let home = absolute(home::ohjaa_home()?);
    let hooks = home::hooks_dir(&home);
    let mut found = Findings { drift: 0 };
    println!("ohjaa doctor: the workflow home is {}", home.display());

    match gate::git_version() {
        Ok(version) if version >= gate::MIN_GIT => found.ok(format!(
            "git {}.{} scopes hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gate::MIN_GIT.0,
            gate::MIN_GIT.1
        )),
        Ok(version) => found.drift(format!(
            "git {}.{} cannot scope hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gate::MIN_GIT.0,
            gate::MIN_GIT.1
        )),
        Err(error) => found.drift(format!("{error:#}")),
    }
    let include = gate::include_file(&home);
    match home::global_paths("include.path") {
        Ok(paths)
            if paths
                .iter()
                .any(|p| home::same_path(Path::new(p), &include)) =>
        {
            found.ok(format!(
                "git's global config includes {}",
                include.display()
            ))
        }
        Ok(_) => found.drift(format!(
            "git's global config does not include {}: no repository runs ohjaa's hooks \
             (ohjaa apply)",
            include.display()
        )),
        Err(error) => found.drift(format!("cannot read git's global include.path: {error:#}")),
    }
    match home::global_path_setting("core.hooksPath") {
        Ok(None) => found.ok("git's global core.hooksPath is unset"),
        Ok(Some(path)) => found.drift(format!(
            "git's global core.hooksPath is {path}: it runs in every repository and \
             overrides ohjaa's per-repository one (ohjaa apply --force removes it)"
        )),
        Err(error) => found.drift(format!(
            "cannot read git's global core.hooksPath: {error:#}"
        )),
    }
    // Each generated file on its own first: an invalid one breaks every git
    // command, generating the expected text among them.
    let mut readable = true;
    for path in [gate::include_file(&home), gate::gated_file(&home)] {
        if !path.is_file() {
            found.drift(format!(
                "{} is missing: the gate is off (ohjaa apply)",
                path.display()
            ));
        } else if let Err(error) = gate::check_git_config(&path) {
            readable = false;
            found.drift(format!(
                "{} cannot be read by git, which then fails in every repository (ohjaa \
                 apply): {}",
                path.display(),
                error.root_cause()
            ));
        }
    }
    if readable {
        match gate::generated(&home) {
            Ok(files) => {
                for (path, text) in files.iter().filter(|(path, _)| path.is_file()) {
                    if read_or_empty(path).is_ok_and(|now| &now == text) {
                        found.ok(format!("{} is current", path.display()));
                    } else {
                        found.drift(format!(
                            "{} is stale: it does not match repos/ (ohjaa apply)",
                            path.display()
                        ));
                    }
                }
            }
            Err(error) => found.drift(format!("cannot generate the git config: {error:#}")),
        }
    }

    let roots = crate::roots::load(roots, git_root);
    let repos = crate::roots::git_repos(&roots)?;
    let gated = match gate::gated_clones(&home, &roots) {
        Ok(gated) => gated,
        Err(error) => {
            found.drift(format!("cannot read the clones under the roots: {error:#}"));
            Vec::new()
        }
    };
    let needed: Vec<&'static str> = gate::needed_hooks(&gated).into_iter().collect();
    check_dispatchers(&mut found, &hooks, &needed);

    match home::global_ignore_file() {
        Ok(path) => match read_or_empty(&path).map(|text| (ignore_with_block(&text), text)) {
            Ok((Ok((with, _)), text)) if with == text => found.ok(format!(
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
            let targets: Vec<PathBuf> = text.lines().filter_map(launch_target).collect();
            if targets.is_empty() {
                found.drift(format!(
                    "{} does not load the launcher (ohjaa apply)",
                    path.display()
                ));
            }
            for target in targets {
                if target == Path::new("ohjaa") {
                    found.drift(format!(
                        "{} loads the launcher unguarded, through PATH (ohjaa apply)",
                        path.display()
                    ));
                } else if gate::executable(&target) {
                    found.ok(format!(
                        "{} loads the launcher from {}",
                        path.display(),
                        target.display()
                    ));
                } else {
                    found.drift(format!(
                        "{} loads the launcher from {}, which is gone (ohjaa apply)",
                        path.display(),
                        target.display()
                    ));
                }
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
        if let Some(clone) = gated.iter().find(|clone| &clone.path == repo) {
            let named = format!("{label} ({})", clone.slugs.join(", "));
            let drift = gate::clone_drift(&home, clone);
            if drift.is_empty() {
                found.ok(format!(
                    "{named}: gated; git runs {} here{}",
                    hooks.display(),
                    if clone.own.is_empty() {
                        String::new()
                    } else {
                        format!(", chaining to its own {}", clone.own.join(", "))
                    }
                ));
            }
            for line in drift {
                found.drift(format!("{named}: {line}"));
            }
        } else if repo.join("sykli.json").is_file() {
            found.note(format!(
                "{label}: an older in-repo sykli.json and no entry (ohjaa adopt {})",
                repo.display()
            ));
        } else {
            plain.push(label);
        }
    }
    if !plain.is_empty() {
        println!(
            "  -      not gated (no entry; git never runs ohjaa there): {}",
            plain.join(", ")
        );
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

/// The gate of the repository at `root`: the contract of the entry origin's
/// push URL names, else the repository's own `sykli.json`; with whose it is.
fn gate_of(root: &Path) -> Result<Option<(PathBuf, String)>> {
    let home = absolute(home::ohjaa_home()?);
    let url = home::git_value(&["remote", "get-url", "--push", "origin"], Some(root))
        .ok()
        .flatten();
    if let Some(id) = url.as_deref().and_then(RepoId::parse) {
        match home::find_entry(&home, &id)? {
            Some(entry) if entry.has_contract() => {
                return Ok(Some((
                    entry.contract(),
                    format!("the entry for {} (branch {})", id.slug(), entry.branch),
                )))
            }
            Some(entry) => bail!(
                "{} has an entry but no gate: {} is missing (ohjaa adopt --force {})",
                id.slug(),
                entry.contract().display(),
                root.display()
            ),
            None => {}
        }
    }
    let own = root.join("sykli.json");
    Ok(own
        .is_file()
        .then(|| (own, "the repository's own sykli.json (no entry)".to_owned())))
}

/// `ohjaa gate`: run the gate for the repository here, the way a push will
/// judge it, and leave the receipt the push looks for.
pub fn gate_here(print_only: bool) -> Result<i32> {
    let root = PathBuf::from(
        home::git_value(&["rev-parse", "--show-toplevel"], None)
            .context("not inside a git repository")?
            .context("not inside a git repository")?,
    );
    let Some((contract, whose)) = gate_of(&root)? else {
        eprintln!(
            "ohjaa gate: {} has no gate: origin names no entry (ohjaa adopt), and there is \
             no sykli.json in the repository",
            root.display()
        );
        return Ok(2);
    };
    let quoted = crate::hook::quote_path(&contract);
    let run = format!("sykli run {quoted} --json > .sykli/receipt.json");
    let verify = format!("sykli verify .sykli/receipt.json --contract {quoted} --commit HEAD");
    println!("ohjaa gate: the gate is {} ({whose})", contract.display());
    if print_only {
        println!(
            "cd {} && mkdir -p .sykli && {run} && {verify}",
            crate::hook::quote_path(&root)
        );
        return Ok(0);
    }
    println!("ohjaa gate: in {}", root.display());
    let sykli_dir = root.join(".sykli");
    std::fs::create_dir_all(&sykli_dir)
        .with_context(|| format!("cannot create {}", sykli_dir.display()))?;
    println!("$ {run}");
    let receipt_path = sykli_dir.join("receipt.json");
    let receipt = std::fs::File::create(&receipt_path)
        .with_context(|| format!("cannot write {}", receipt_path.display()))?;
    let status = Command::new("sykli")
        .arg("run")
        .arg(&contract)
        .arg("--json")
        .current_dir(&root)
        .stdout(receipt)
        .status()
        .context("cannot run sykli")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        println!("ohjaa gate: failed: sykli run exited {code}; the receipt says why");
        return Ok(code);
    }
    println!("$ {verify}");
    let status = Command::new("sykli")
        .args(["verify", ".sykli/receipt.json", "--contract"])
        .arg(&contract)
        .args(["--commit", "HEAD"])
        .current_dir(&root)
        .status()
        .context("cannot run sykli")?;
    let code = status.code().unwrap_or(1);
    if status.success() {
        println!("ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so it can be pushed");
    } else {
        println!("ohjaa gate: failed: sykli verify exited {code}");
    }
    Ok(code)
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
    println!("ohjaa adopt: {} -> {}", id, dir.display());
    println!("  branch = {branch:?} (repo.toml)");
    println!("  {source}");
    // The home's git config now names this repository too; git's global
    // config is not touched (it includes the home's file once, from apply).
    for said in crate::apply::regenerate(&home, &gate::own_hooks(&root)?)? {
        println!("  {said}");
    }
    match gate::gated_clone(&home, &root)? {
        Some(clone) => {
            let drift = gate::clone_drift(&home, &clone);
            if drift.is_empty() {
                println!(
                    "  gated: git runs {} here",
                    home::hooks_dir(&home).display()
                );
            }
            for line in drift {
                eprintln!("ohjaa adopt: warning: {}: {line}", root.display());
            }
        }
        None => eprintln!(
            "ohjaa adopt: warning: no remote of {} names {} after all",
            root.display(),
            id.slug()
        ),
    }
    let after = status_of(&root)?;
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
        assert!(LAUNCH_ZSH.contains("shell-init zsh"));
        assert!(!LAUNCH_ZSH.contains("source ~/projects/ohjaa-launch.zsh"));
    }
}
