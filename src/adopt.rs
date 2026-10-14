//! `ohjaa adopt`: write a repository's entry into the home and regenerate
//! the home's git config. It never touches git's global config, and never
//! writes into a repository (it takes no lock there either: no `git
//! status`).

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::gitconfig::{self, fetch_url, repo_at};
use crate::home::{self, absolute, RepoId};
use crate::process;

/// The change was refused.
const REFUSED: i32 = 3;

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// An object `git cat-file --batch` found: its id and content.
type Object = (String, Vec<u8>);

/// What one `git cat-file --batch` says of each object asked for: its id
/// and content, or `None` when missing.
fn objects(top: &Path, names: &[String]) -> Result<Vec<Option<Object>>> {
    let mut command = Command::new("git");
    command.args(["cat-file", "--batch"]).current_dir(top);
    let input = names.join("\n") + "\n";
    let output = process::run(&mut command, Some(input.as_bytes()), process::LOCAL)
        .context("cannot run git cat-file")?;
    if !output.status.success() {
        bail!("git cat-file --batch failed: {}", output.failure_detail());
    }
    let mut out = output.stdout.as_slice();
    let mut found = Vec::new();
    for _ in names {
        let end = out
            .iter()
            .position(|byte| *byte == b'\n')
            .context("git cat-file --batch said less than asked")?;
        let header = String::from_utf8_lossy(&out[..end]).into_owned();
        out = &out[end + 1..];
        let fields: Vec<&str> = header.split(' ').collect();
        match fields[..] {
            [oid, _, size] if size.parse::<usize>().is_ok() => {
                let size: usize = size.parse().unwrap_or(0);
                if out.len() < size + 1 {
                    bail!("git cat-file --batch cut an object short");
                }
                found.push(Some((oid.to_owned(), out[..size].to_vec())));
                out = &out[size + 1..];
            }
            _ => found.push(None),
        }
    }
    Ok(found)
}

/// The branch `refs/remotes/origin/HEAD` names: the loose symbolic ref, else
/// (a reftable repository) asked of git.
fn origin_head(top: &Path, common: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(common.join("refs/remotes/origin/HEAD")) {
        Ok(text) => Ok(text
            .trim()
            .strip_prefix("ref: refs/remotes/origin/")
            .map(str::to_owned)),
        Err(_) if common.join("reftable").exists() => Ok(home::git_value(
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            Some(top),
        )?
        .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))),
        Err(_) => Ok(None),
    }
}

fn sykli_init_takes_a_contract() -> bool {
    Command::new("sykli")
        .args(["init", "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|output| {
            process::names_flag(&String::from_utf8_lossy(&output.stdout), "--contract")
        })
}

pub fn adopt(path: &Path, branch: Option<&str>, force: bool) -> Result<i32> {
    let (top, common) = repo_at(path)?;
    let listing = gitconfig::clone_listing(&top)?;
    let url = fetch_url(&listing, "origin")
        .with_context(|| format!("{} has no origin remote to name it by", top.display()))?;
    let id = RepoId::parse(&url)
        .with_context(|| format!("origin's URL {url:?} names no owner and repository"))?;
    let branch = match branch {
        Some(branch) => branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch)
            .to_owned(),
        None => origin_head(&top, &common)?.with_context(|| {
            format!(
                "origin/HEAD is not set in {}, so its default branch is unknown here; \
                 name it with --branch, or set it with: git remote set-head origin --auto",
                top.display()
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
    let created = !dir.exists();
    let result = write_entry(&top, &id, &dir, &branch, force);
    if result.is_err() && created {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let source = result?;
    println!("ohjaa adopt: {} -> {}", id, dir.display());
    println!("  branch = {branch:?} (repo.toml)");
    println!("  {source}");
    // The home's git config now names this repository too; git's global
    // config is not touched (it includes the home's file once, from apply).
    let global = gitconfig::GlobalFacts::read()?;
    let own = gitconfig::own_hooks_in(&common);
    for said in crate::apply::regenerate(&home, &dir, &own, &global.rewrites())? {
        println!("  {said}");
    }
    match gitconfig::gated_clone(&home, &top)? {
        Some(clone) => {
            let drift = gitconfig::clone_drift(&home, &clone);
            if drift.is_empty() {
                println!(
                    "  observed: git runs {} here",
                    clone.hooks_path.as_deref().unwrap_or_default()
                );
            }
            for line in drift {
                eprintln!("ohjaa adopt: warning: {}: {line}", top.display());
            }
        }
        None => eprintln!(
            "ohjaa adopt: warning: no remote of {} names {} after all",
            top.display(),
            id.slug()
        ),
    }
    Ok(0)
}

/// Write the gate and `repo.toml` into `dir`; say where the gate came from.
/// The gate is read as `origin/<branch>` holds it (else `HEAD`), in one `git
/// cat-file --batch`.
fn write_entry(top: &Path, id: &RepoId, dir: &Path, branch: &str, force: bool) -> Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let revs = [format!("refs/remotes/origin/{branch}"), "HEAD".to_owned()];
    let files = ["sykli.json", "sykli.lock", "sykli.rs"];
    let mut names = Vec::new();
    for rev in &revs {
        names.push(rev.clone());
        names.extend(files.iter().map(|file| format!("{rev}:{file}")));
    }
    let found = objects(top, &names)?;
    let at = if found[0].is_some() {
        0
    } else {
        files.len() + 1
    };
    let rev = if at == 0 {
        format!("origin/{branch}")
    } else {
        "HEAD".to_owned()
    };
    let commit = found[at]
        .as_ref()
        .map(|(oid, _)| short(oid).to_owned())
        .unwrap_or_default();
    let blob = |n: usize| found[at + 1 + n].as_ref().map(|(_, bytes)| bytes.clone());
    let contract = dir.join("sykli.json");
    let lock = dir.join("sykli.lock");
    let source = if let Some(json) = blob(0) {
        std::fs::write(&contract, json)
            .with_context(|| format!("cannot write {}", contract.display()))?;
        match blob(1) {
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
    } else if blob(2).is_some() {
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
            .current_dir(top);
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
    // Only repo.toml: what a person wrote about sharing Judgements
    // (sharing.toml beside it) is theirs, and never touched here.
    home::write_file(&dir.join("repo.toml"), &home::repo_toml(id, branch), None)?;
    Ok(source)
}
