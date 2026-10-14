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
    declare_worktree_cache(&top, &dir, &home);
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

// ---- KATANA D10: each worktree's gate builds in a directory of its own ----

/// The placeholder sykli expands, in a task's `env` value, to a directory of
/// the worktree the run is in (`<git-common-dir>/sykli/worktrees/<id>/cache`).
const WORKTREE_CACHE: &str = "${worktree_cache}";

/// What a task that runs cargo declares, so that a gate never runs a binary
/// or test another worktree built in a shared target dir (or build dir).
const WORKTREE_CACHE_ENV: [(&str, &str); 2] = [
    ("CARGO_TARGET_DIR", "${worktree_cache}/cargo-target"),
    ("CARGO_BUILD_BUILD_DIR", "${worktree_cache}/cargo-build"),
];

const CONTRACT_SCHEMA: &str = "sykli-contract.v1";

/// A JSON value whose objects keep their members in the order written, so a
/// contract gains its lines and is otherwise written back as it was
/// (`serde_json::Value` would sort every object's keys).
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Json;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> std::result::Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, value: bool) -> std::result::Result<Json, E> {
                Ok(Json::Bool(value))
            }
            fn visit_i64<E>(self, value: i64) -> std::result::Result<Json, E> {
                Ok(Json::Number(value.into()))
            }
            fn visit_u64<E>(self, value: u64) -> std::result::Result<Json, E> {
                Ok(Json::Number(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Json, E> {
                serde_json::Number::from_f64(value)
                    .map(Json::Number)
                    .ok_or_else(|| E::custom("a number JSON cannot hold"))
            }
            fn visit_str<E>(self, value: &str) -> std::result::Result<Json, E> {
                Ok(Json::String(value.to_owned()))
            }
            fn visit_string<E>(self, value: String) -> std::result::Result<Json, E> {
                Ok(Json::String(value))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Json, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Json::Array(items))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Json, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry::<String, Json>()? {
                    members.push(member);
                }
                Ok(Json::Object(members))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl serde::Serialize for Json {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Json::Null => serializer.serialize_unit(),
            Json::Bool(value) => serializer.serialize_bool(*value),
            Json::Number(value) => value.serialize(serializer),
            Json::String(value) => serializer.serialize_str(value),
            Json::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Json::Object(members) => {
                let mut map = serializer.serialize_map(Some(members.len()))?;
                for (name, value) in members {
                    map.serialize_entry(name, value)?;
                }
                map.end()
            }
        }
    }
}

/// A shell command's words, split where sh would end a word (blanks,
/// operators, quotes). Read as words, never as a sentence.
fn words(run: &str) -> impl Iterator<Item = &str> {
    run.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ';' | '&' | '|' | '(' | ')' | '`' | '\'' | '"' | '<' | '>'
            )
    })
    .filter(|word| !word.is_empty())
}

/// Whether a task's command runs cargo: a word of it is `cargo`, or a path to
/// it. A `make` that runs cargo inside is not seen; a word `cargo` that is
/// only an argument is (declaring the lines there costs nothing).
fn runs_cargo(run: &str) -> bool {
    words(run).any(|word| word == "cargo" || word.ends_with("/cargo"))
}

/// Whether a task already says where cargo builds: either variable in its
/// `env` or `inherit`, or assigned in its command. Never overridden.
fn declares_build_dirs(task: &Json, run: &str) -> std::result::Result<bool, String> {
    let named = |name: &str| WORKTREE_CACHE_ENV.iter().any(|(var, _)| *var == name);
    let env = match task.get("env") {
        None => false,
        Some(Json::Object(members)) => members.iter().any(|(name, _)| named(name)),
        Some(_) => return Err("a contract with a task whose env is not an object".to_owned()),
    };
    let inherited = match task.get("inherit") {
        None => false,
        Some(Json::Array(names)) => names.iter().filter_map(Json::as_str).any(named),
        Some(_) => return Err("a contract with a task whose inherit is not a list".to_owned()),
    };
    let assigned = words(run).any(|word| word.split_once('=').is_some_and(|(name, _)| named(name)));
    Ok(env || inherited || assigned)
}

/// A contract's tasks that run cargo, and of those the ones that declare
/// neither variable: the ones adopt gives the lines to.
#[derive(Debug, Default, PartialEq)]
pub struct CargoTasks {
    pub cargo: Vec<String>,
    pub missing: Vec<String>,
}

/// The contract and its cargo tasks, when it is a `sykli-contract.v1`
/// contract ohjaa can read; why not, when it is not.
fn read_contract(text: &str) -> std::result::Result<(Json, CargoTasks), String> {
    let contract: Json =
        serde_json::from_str(text).map_err(|error| format!("not JSON ({error})"))?;
    if contract.get("schema").and_then(Json::as_str) != Some(CONTRACT_SCHEMA) {
        return Err(format!("not a {CONTRACT_SCHEMA} contract"));
    }
    let Some(Json::Array(tasks)) = contract.get("tasks") else {
        return Err("a contract whose tasks are not a list".to_owned());
    };
    let mut found = CargoTasks::default();
    for task in tasks {
        let (Some(name), Some(run)) = (
            task.get("name").and_then(Json::as_str),
            task.get("run").and_then(Json::as_str),
        ) else {
            return Err("a contract with a task that has no name or no run".to_owned());
        };
        if !runs_cargo(run) {
            continue;
        }
        found.cargo.push(name.to_owned());
        if !declares_build_dirs(task, run)? {
            found.missing.push(name.to_owned());
        }
    }
    Ok((contract, found))
}

/// The cargo tasks of the contract at `path`, or why they cannot be read.
pub fn cargo_tasks(path: &Path) -> std::result::Result<CargoTasks, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    read_contract(&text)
        .map(|(_, tasks)| tasks)
        .map_err(|why| format!("{} is {why}", path.display()))
}

/// The lines added to each task named in `missing`: at the end of its own
/// `env`, or as a new `env` where sykli's emitter puts one (after `run`, or
/// after `workdir`).
fn declare_in(contract: &mut Json, missing: &[String]) {
    let Json::Object(members) = contract else {
        return;
    };
    let Some((_, Json::Array(tasks))) = members.iter_mut().find(|(name, _)| name == "tasks") else {
        return;
    };
    for task in tasks {
        if !task
            .get("name")
            .and_then(Json::as_str)
            .is_some_and(|name| missing.iter().any(|m| m == name))
        {
            continue;
        }
        let Json::Object(fields) = task else {
            continue;
        };
        let lines = WORKTREE_CACHE_ENV
            .iter()
            .map(|(name, value)| ((*name).to_owned(), Json::String((*value).to_owned())));
        match fields.iter().position(|(name, _)| name == "env") {
            Some(at) => {
                if let (_, Json::Object(env)) = &mut fields[at] {
                    env.extend(lines);
                }
            }
            None => {
                let at = fields
                    .iter()
                    .rposition(|(name, _)| name == "run" || name == "workdir")
                    .map_or(fields.len(), |at| at + 1);
                fields.insert(at, ("env".to_owned(), Json::Object(lines.collect())));
            }
        }
    }
}

/// Whether every sykli that runs an entry's gate expands `${worktree_cache}`:
/// the one on PATH (adopt itself, `ohjaa gate`, the integrated validation)
/// and the one `apply` recorded for the pre-push dispatcher. A sykli that
/// does names the placeholder in `sykli run --help`; it is matched as that
/// token, the contract's own syntax, never as a sentence (sentences get
/// reworded). An older sykli passes it to the task as written, and cargo then
/// builds in a `${worktree_cache}/` folder inside the work tree. `Err` says
/// which sykli does not.
pub fn sykli_expands_worktree_cache(home: &Path) -> std::result::Result<(), String> {
    let Some(on_path) = crate::hook::on_path("sykli") else {
        return Err("no sykli is on PATH".to_owned());
    };
    let recorded = crate::apply::recorded_sykli(home)
        .map_err(|error| format!("apply's record cannot be read ({error:#})"))?;
    let mut asked = vec![(on_path.clone(), "the sykli on PATH")];
    if let Some(recorded) = recorded.filter(|recorded| *recorded != on_path) {
        asked.push((recorded, "the sykli the pre-push dispatcher runs"));
    }
    for (sykli, which) in asked {
        let help = Command::new(&sykli)
            .args(["run", "--help"])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| format!("{which} ({}) cannot be run: {error}", sykli.display()))?;
        if !String::from_utf8_lossy(&help.stdout).contains(WORKTREE_CACHE) {
            return Err(format!(
                "{which} ({}) does not name {WORKTREE_CACHE} in `sykli run --help`",
                sykli.display()
            ));
        }
    }
    Ok(())
}

/// `sykli lock <contract>`, run in the repository the contract judges.
fn lock_again(top: &Path, contract: &Path) -> std::result::Result<(), String> {
    let mut lock = Command::new("sykli");
    lock.arg("lock").arg(contract).current_dir(top);
    let output =
        process::run(&mut lock, None, process::LOCAL).map_err(|error| format!("{error:#}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output.failure_detail())
    }
}

/// KATANA D10: each cargo task of the entry's gate that says nothing of
/// where cargo builds gains `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR`
/// under `${worktree_cache}`, and a gate that had a lock is locked again.
/// Only when every sykli that runs the gate expands the placeholder. Says
/// what it did; never fails the adoption: a gate it cannot change is kept as
/// copied.
fn declare_worktree_cache(top: &Path, dir: &Path, home: &Path) {
    let contract = dir.join("sykli.json");
    let lock = dir.join("sykli.lock");
    let text = match std::fs::read_to_string(&contract) {
        Ok(text) => text,
        Err(error) => {
            eprintln!(
                "ohjaa adopt: warning: cannot read {} ({error}), so nothing declares \
                 {WORKTREE_CACHE}",
                contract.display()
            );
            return;
        }
    };
    let (mut json, tasks) = match read_contract(&text) {
        Ok(read) => read,
        Err(why) => {
            println!(
                "  {WORKTREE_CACHE}: nothing declared: {} is {why}",
                contract.display()
            );
            return;
        }
    };
    if tasks.missing.is_empty() {
        return;
    }
    let names = tasks.missing.join(", ");
    if let Err(why) = sykli_expands_worktree_cache(home) {
        println!(
            "  {WORKTREE_CACHE}: not declared for {names}: {why}, and an older sykli passes it \
             on as written, to build inside the work tree; gate runs may share a target dir \
             across worktrees until it is updated (then: ohjaa adopt --force)"
        );
        return;
    }
    declare_in(&mut json, &tasks.missing);
    let mut declared = match serde_json::to_string_pretty(&json) {
        Ok(declared) => declared,
        Err(error) => {
            eprintln!("ohjaa adopt: warning: cannot write the gate back ({error})");
            return;
        }
    };
    if text.ends_with('\n') {
        declared.push('\n');
    }
    let copied_lock = std::fs::read(&lock).ok();
    let written =
        home::write_file(&contract, &declared, None).map_err(|error| format!("{error:#}"));
    let locked = match (&written, &copied_lock) {
        (Err(why), _) => Err(why.clone()),
        (Ok(()), Some(_)) => lock_again(top, &contract)
            .map_err(|why| format!("sykli lock {} failed ({why})", contract.display())),
        (Ok(()), None) => Ok(()),
    };
    if let Err(why) = locked {
        let mut restored =
            home::write_file(&contract, &text, None).map_err(|error| format!("{error:#}"));
        if let (Ok(()), Some(bytes)) = (&restored, &copied_lock) {
            restored = std::fs::write(&lock, bytes)
                .map_err(|error| format!("cannot write {}: {error}", lock.display()));
        }
        match restored {
            Ok(()) => eprintln!(
                "ohjaa adopt: warning: {why}, so the gate is kept as copied, without \
                 {WORKTREE_CACHE}: gate runs may share a target dir across worktrees"
            ),
            Err(error) => eprintln!(
                "ohjaa adopt: warning: {why}, and the gate as copied could not be put back \
                 ({error}): adopt again with --force"
            ),
        }
        return;
    }
    let kept: Vec<&str> = tasks
        .cargo
        .iter()
        .filter(|name| !tasks.missing.contains(name))
        .map(String::as_str)
        .collect();
    println!(
        "  {WORKTREE_CACHE}: {} declared for {names}, {}{}",
        WORKTREE_CACHE_ENV.map(|(name, _)| name).join(" and "),
        if copied_lock.is_some() {
            "and sykli.lock locked again"
        } else {
            "with no sykli.lock to lock again"
        },
        if kept.is_empty() {
            String::new()
        } else {
            format!("; kept as they declare it: {}", kept.join(", "))
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_runs_cargo_when_a_word_of_its_command_is_cargo() {
        for run in [
            "cargo fmt --check",
            "RUSTDOCFLAGS='-D warnings' cargo doc --no-deps",
            "mise exec -- cargo test",
            "cd native && cargo build --locked",
            "~/.cargo/bin/cargo test",
            "test -n \"$(cargo metadata --format-version 1)\"",
        ] {
            assert!(runs_cargo(run), "{run}");
        }
        for run in [
            "make build",
            "env REBAR_CACHE_DIR=$PWD/_build/rebar-cache rebar3 fmt --check",
            "cargo-deny check bans",
            "scripts/check-cargo.sh",
        ] {
            assert!(!runs_cargo(run), "{run}");
        }
    }

    #[test]
    fn a_build_dir_declared_anywhere_is_never_overridden() {
        let task = |json: &str| serde_json::from_str::<Json>(json).unwrap();
        for (json, run) in [
            (r#"{"env":{"CARGO_TARGET_DIR":"t"}}"#, "cargo test"),
            (r#"{"env":{"CARGO_BUILD_BUILD_DIR":"b"}}"#, "cargo test"),
            (r#"{"inherit":["CARGO_TARGET_DIR"]}"#, "cargo test"),
            ("{}", "CARGO_TARGET_DIR=\"$PWD/t\" cargo test"),
            ("{}", "export CARGO_BUILD_BUILD_DIR=b; cargo test"),
        ] {
            assert_eq!(
                declares_build_dirs(&task(json), run),
                Ok(true),
                "{json} {run}"
            );
        }
        for (json, run) in [
            ("{}", "cargo test"),
            (
                r#"{"env":{"RUST_BACKTRACE":"1"},"inherit":["HOME"]}"#,
                "cargo test --target-dir=t",
            ),
        ] {
            assert_eq!(
                declares_build_dirs(&task(json), run),
                Ok(false),
                "{json} {run}"
            );
        }
        assert!(declares_build_dirs(&task(r#"{"env":["x"]}"#), "cargo test").is_err());
    }

    #[test]
    fn a_contract_ohjaa_cannot_read_is_said_so_never_changed() {
        for text in [
            "not json",
            "{}",
            r#"{"version":"1","tasks":{"test":{}}}"#,
            r#"{"schema":"sykli-contract.v2","tasks":[]}"#,
            r#"{"schema":"sykli-contract.v1","tasks":{}}"#,
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"t"}]}"#,
        ] {
            assert!(read_contract(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_lines_go_where_sykli_puts_env_and_nothing_else_moves() {
        let text = r#"{"schema":"sykli-contract.v1","tasks":[{"inputs":["a"],"name":"t","run":"cargo test","outputs":["o"]},{"name":"u","run":"true"}],"z":1.5}"#;
        let (mut contract, tasks) = read_contract(text).unwrap();
        assert_eq!(tasks.missing, ["t"]);
        declare_in(&mut contract, &tasks.missing);
        assert_eq!(
            serde_json::to_string(&contract).unwrap(),
            r#"{"schema":"sykli-contract.v1","tasks":[{"inputs":["a"],"name":"t","run":"cargo test","env":{"CARGO_TARGET_DIR":"${worktree_cache}/cargo-target","CARGO_BUILD_BUILD_DIR":"${worktree_cache}/cargo-build"},"outputs":["o"]},{"name":"u","run":"true"}],"z":1.5}"#
        );
    }
}
