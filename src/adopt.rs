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
    declare_worktree_cache(&top, &common, &dir, &home);
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

/// One of the two places cargo builds in, and the line that puts it in the
/// worktree's own directory, so that a gate never runs a binary or test
/// another worktree built there.
struct Setting {
    /// The variable adopt declares, and its value.
    var: &'static str,
    value: &'static str,
    /// The variables that say where it is. A task that names one of them in
    /// its `env` or `inherit`, or certainly sets one for every cargo in its
    /// command, already says it, and is never given the line.
    said_by: &'static [&'static str],
}

/// The target dir, where cargo leaves what it builds (`CARGO_TARGET_DIR`,
/// else `build.target-dir`, which `CARGO_BUILD_TARGET_DIR` sets), and the
/// build dir, where it keeps intermediate artifacts, test binaries among
/// them (`build.build-dir`).
const SETTINGS: [Setting; 2] = [
    Setting {
        var: "CARGO_TARGET_DIR",
        value: "${worktree_cache}/cargo-target",
        said_by: &["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"],
    },
    Setting {
        var: "CARGO_BUILD_BUILD_DIR",
        value: "${worktree_cache}/cargo-build",
        said_by: &["CARGO_BUILD_BUILD_DIR"],
    },
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

/// A command's simple commands, each as its words, split as sh splits them:
/// at blanks outside quotes, and at the operators that end a simple command
/// (`;`, `&`, `|`, `(`, `)`, a backquote, a newline). Quotes stay in their
/// word (`A='x y'` is one word), and a redirection is dropped with its
/// target.
fn simple_commands(run: &str) -> Vec<Vec<String>> {
    struct Split {
        commands: Vec<Vec<String>>,
        word: String,
        redirected: bool,
    }
    impl Split {
        fn end_word(&mut self) {
            let word = std::mem::take(&mut self.word);
            if word.is_empty() || std::mem::take(&mut self.redirected) {
                return;
            }
            if let Some(command) = self.commands.last_mut() {
                command.push(word);
            }
        }
        fn end_command(&mut self) {
            self.end_word();
            // A redirection has no target past its command (`<(…)`).
            self.redirected = false;
            if self.commands.last().is_some_and(|words| !words.is_empty()) {
                self.commands.push(Vec::new());
            }
        }
    }
    let mut split = Split {
        commands: vec![Vec::new()],
        word: String::new(),
        redirected: false,
    };
    let mut quote = None;
    let mut chars = run.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            split.word.push(c);
            if c == open {
                quote = None;
            } else if c == '\\' && open == '"' {
                split.word.extend(chars.next());
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                split.word.push(c);
            }
            '\\' => {
                split.word.push(c);
                split.word.extend(chars.next());
            }
            ';' | '&' | '|' | '(' | ')' | '`' | '\n' => split.end_command(),
            '<' | '>' => {
                // `2>&1`: the digits name a descriptor, and are no word.
                if split.word.chars().all(|digit| digit.is_ascii_digit()) {
                    split.word.clear();
                } else {
                    split.end_word();
                }
                while chars
                    .next_if(|next| matches!(next, '<' | '>' | '&' | '|'))
                    .is_some()
                {}
                split.redirected = true;
            }
            c if c.is_whitespace() => split.end_word(),
            c => split.word.push(c),
        }
    }
    split.end_command();
    split.commands.retain(|words| !words.is_empty());
    split.commands
}

/// Whether `word` is a shell variable's name.
fn is_name(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The name an assignment word sets (`NAME=…`, the name unquoted).
fn assigned(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    is_name(name).then_some(name)
}

/// Words that open or close a compound command, and so stand before a
/// simple command's own words.
const RESERVED: [&str; 13] = [
    "!", "{", "}", "if", "then", "elif", "else", "fi", "do", "done", "while", "until", "time",
];

/// Recognize only declarations whose scope is evident without interpreting
/// shell code. Anything uncertain gets defaults; a command's own assignment
/// still wins. Never infer exports from conditionals, subshells or wrappers.
fn names_set_in(run: &str) -> Vec<String> {
    if run.contains(['#', '`', '\\', '(', ')'])
        || run.contains("<<")
        || run
            .split("${")
            .skip(1)
            .any(|part| part.split_once('}').is_none_or(|(name, _)| !is_name(name)))
    {
        return Vec::new();
    }
    let commands = simple_commands(run);
    // A leading unconditional export, separated only by ; or newline.
    let mut remaining = run.trim_start();
    let mut exported = Vec::new();
    let mut prefix = 0;
    let mut quote = None;
    // Separators inside quoted values are data, not another export. The
    // early check above excludes escapes and command substitutions.
    while let Some(end) = remaining.char_indices().find_map(|(index, c)| {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' => quote = Some(c),
                ';' | '\n' => return Some(index),
                _ => {}
            }
        }
        None
    }) {
        let line = &remaining[..end];
        if line.contains(['&', '|', '<', '>']) {
            break;
        }
        let parts = simple_commands(line);
        let Some(words) = parts.first().filter(|_| parts.len() == 1) else {
            break;
        };
        if words.first().is_some_and(|word| word == "export")
            && words.len() > 1
            && words[1..].iter().all(|word| assigned(word).is_some())
        {
            exported.extend(
                words[1..]
                    .iter()
                    .filter_map(|word| assigned(word))
                    .map(str::to_owned),
            );
        } else if words.iter().map(String::as_str).collect::<Vec<_>>() != ["set", "-e"] {
            break;
        }
        prefix += 1;
        remaining = remaining[end + 1..].trim_start();
    }
    // Unknown commands can unset variables or run hidden cargo.
    if commands.iter().skip(prefix).any(|command| {
        let program = command.iter().find(|word| assigned(word).is_none());
        !program.is_some_and(|word| word == "cd" || word == "cargo" || word.ends_with("/cargo"))
    }) {
        exported.clear();
    }
    let mut every: Option<Vec<String>> = None;
    for command in commands {
        let mut words = command
            .iter()
            .map(String::as_str)
            .skip_while(|word| RESERVED.contains(word))
            .peekable();
        let mut here = exported.clone();
        while let Some(name) = words.peek().and_then(|word| assigned(word)) {
            here.push(name.to_owned());
            words.next();
        }
        let program = words.next().unwrap_or_default();
        if program != "cargo" && !program.ends_with("/cargo") {
            // A quoted or wrapped cargo is not covered by other invocations.
            if command.iter().any(|word| runs_cargo(word)) {
                return Vec::new();
            }
            continue;
        }
        // Expansion can insert `--` before an apparent Cargo flag. Do not
        // credit flags in such a command; prefix assignments still apply.
        let args: Vec<_> = words.collect();
        let literal_flags = args.iter().all(|word| {
            if word.contains(['$', '*', '?', '[']) {
                return false;
            }
            // One enclosing quote pair is clear; concatenated fragments
            // could spell an otherwise invisible option terminator.
            match word.chars().next() {
                Some(quote @ ('\'' | '"')) => word
                    .strip_prefix(quote)
                    .and_then(|inner| inner.strip_suffix(quote))
                    .is_some_and(|inner| !inner.contains(quote)),
                _ => !word.contains(['\'', '"']),
            }
        });
        let mut args = args
            .into_iter()
            .take_while(|word| literal_flags && word.trim_matches(['\'', '"']) != "--")
            .peekable();
        while let Some(arg) = args.next() {
            if arg == "--target-dir" && args.peek().is_some()
                || arg
                    .strip_prefix("--target-dir=")
                    .is_some_and(|value| !value.is_empty())
            {
                here.push("CARGO_TARGET_DIR".to_owned());
            }
            let config = if arg == "--config" {
                args.next()
            } else {
                arg.strip_prefix("--config=")
            };
            if let Some(config) = config {
                let config = config.trim_matches(['\'', '"']);
                for (key, var) in [
                    ("build.target-dir=", "CARGO_TARGET_DIR"),
                    ("build.build-dir=", "CARGO_BUILD_BUILD_DIR"),
                ] {
                    if config
                        .strip_prefix(key)
                        .is_some_and(|value| !value.is_empty())
                    {
                        here.push(var.to_owned());
                    }
                }
            }
        }
        // Different spellings still describe the same target-directory choice.
        if here.iter().any(|name| name == "CARGO_BUILD_TARGET_DIR") {
            here.push("CARGO_TARGET_DIR".to_owned());
        }
        every = Some(match every {
            None => here,
            Some(previous) => previous
                .into_iter()
                .filter(|name| here.contains(name))
                .collect(),
        });
    }
    every.unwrap_or_default()
}

/// A task with an explicit target directory keeps its whole layout. Otherwise
/// supply only the settings absent from env, inherit and certain command
/// declarations (see names_set_in).
fn unsaid(task: &Json, run: &str) -> std::result::Result<Vec<&'static Setting>, String> {
    let mut said = names_set_in(run);
    match task.get("env") {
        None => {}
        Some(Json::Object(members)) => said.extend(members.iter().map(|(name, _)| name.clone())),
        Some(_) => return Err("a contract with a task whose env is not an object".to_owned()),
    }
    match task.get("inherit") {
        None => {}
        Some(Json::Array(names)) => {
            said.extend(names.iter().filter_map(Json::as_str).map(str::to_owned));
        }
        Some(_) => return Err("a contract with a task whose inherit is not a list".to_owned()),
    }
    // Cargo's build directory defaults under an explicitly chosen target
    // directory. Do not split a task's own layout by supplying the other line.
    if SETTINGS[0]
        .said_by
        .iter()
        .any(|var| said.iter().any(|name| name == var))
    {
        return Ok(Vec::new());
    }
    Ok(SETTINGS
        .iter()
        .filter(|setting| {
            !setting
                .said_by
                .iter()
                .any(|var| said.iter().any(|name| name == var))
        })
        .collect())
}

/// Whether a string anywhere in `value` names `${worktree_cache}`.
fn names_placeholder(value: &Json) -> bool {
    match value {
        Json::String(text) => text.contains(WORKTREE_CACHE),
        Json::Array(items) => items.iter().any(names_placeholder),
        Json::Object(members) => members.iter().any(|(_, value)| names_placeholder(value)),
        Json::Null | Json::Bool(_) | Json::Number(_) => false,
    }
}

/// A cargo task that leaves a setting unsaid, and the variables adopt gives
/// it for them.
#[derive(Debug, PartialEq)]
pub struct Unsaid {
    pub task: String,
    pub vars: Vec<&'static str>,
}

/// A contract's tasks that run cargo, those of them that leave a setting
/// unsaid (the ones adopt gives lines to), and the tasks that name
/// `${worktree_cache}`.
#[derive(Debug, Default, PartialEq)]
pub struct CargoTasks {
    pub cargo: Vec<String>,
    pub missing: Vec<Unsaid>,
    pub placeholder: Vec<String>,
}

impl CargoTasks {
    /// The tasks that leave a setting unsaid, as adopt and doctor name them:
    /// one that says the other names what it lacks (`own (no
    /// CARGO_BUILD_BUILD_DIR)`).
    pub fn lacking(&self) -> String {
        self.missing
            .iter()
            .map(|unsaid| {
                if unsaid.vars.len() == SETTINGS.len() {
                    unsaid.task.clone()
                } else {
                    format!("{} (no {})", unsaid.task, unsaid.vars.join(", no "))
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What adopt gives them, the tasks given the same variables together:
    /// `CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR declared for fmt, test;
    /// CARGO_TARGET_DIR for inherited`.
    fn given(&self) -> String {
        let mut groups: Vec<(&[&str], Vec<&str>)> = Vec::new();
        for unsaid in &self.missing {
            match groups
                .iter_mut()
                .find(|(vars, _)| *vars == unsaid.vars.as_slice())
            {
                Some((_, tasks)) => tasks.push(&unsaid.task),
                None => groups.push((&unsaid.vars, vec![&unsaid.task])),
            }
        }
        groups
            .iter()
            .enumerate()
            .map(|(at, (vars, tasks))| {
                format!(
                    "{}{} for {}",
                    vars.join(" and "),
                    if at == 0 { " declared" } else { "" },
                    tasks.join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The cargo tasks that say both settings, kept as they say them.
    fn kept(&self) -> Vec<&str> {
        self.cargo
            .iter()
            .filter(|name| !self.missing.iter().any(|unsaid| &unsaid.task == *name))
            .map(String::as_str)
            .collect()
    }
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
        if names_placeholder(task) {
            found.placeholder.push(name.to_owned());
        }
        if !runs_cargo(run) {
            continue;
        }
        found.cargo.push(name.to_owned());
        let vars: Vec<&'static str> = unsaid(task, run)?
            .into_iter()
            .map(|setting| setting.var)
            .collect();
        if !vars.is_empty() {
            found.missing.push(Unsaid {
                task: name.to_owned(),
                vars,
            });
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

/// The lines each task in `missing` is given: at the end of its own `env`,
/// or as a new `env` where sykli's emitter puts one (after `run`, or after
/// `workdir`).
fn declare_in(contract: &mut Json, missing: &[Unsaid]) {
    let Json::Object(members) = contract else {
        return;
    };
    let Some((_, Json::Array(tasks))) = members.iter_mut().find(|(name, _)| name == "tasks") else {
        return;
    };
    for task in tasks {
        let Some(unsaid) = task
            .get("name")
            .and_then(Json::as_str)
            .and_then(|name| missing.iter().find(|unsaid| unsaid.task == name))
        else {
            continue;
        };
        let lines: Vec<(String, Json)> = SETTINGS
            .iter()
            .filter(|setting| unsaid.vars.contains(&setting.var))
            .map(|setting| {
                (
                    setting.var.to_owned(),
                    Json::String(setting.value.to_owned()),
                )
            })
            .collect();
        let Json::Object(fields) = task else {
            continue;
        };
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
                fields.insert(at, ("env".to_owned(), Json::Object(lines)));
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
/// builds in a `${worktree_cache}/` folder inside the work tree. `Err` names
/// each sykli that does not.
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
    let mut not = Vec::new();
    for (sykli, which) in asked {
        match Command::new(&sykli)
            .args(["run", "--help"])
            .stdin(std::process::Stdio::null())
            .output()
        {
            Err(error) => not.push(format!(
                "{which} ({}) cannot be run: {error}",
                sykli.display()
            )),
            Ok(help) if !String::from_utf8_lossy(&help.stdout).contains(WORKTREE_CACHE) => {
                not.push(format!(
                    "{which} ({}) does not name {WORKTREE_CACHE} in `sykli run --help`",
                    sykli.display()
                ));
            }
            Ok(_) => {}
        }
    }
    if not.is_empty() {
        Ok(())
    } else {
        Err(not.join(", and "))
    }
}

/// `sykli <command> <contract>` (`validate`, `lock`), run in the repository
/// the contract judges and handed its facts when that command takes them
/// (`sykli <command> --help` names `--repo-root` and `--git-common-dir`), so
/// sykli asks git nothing ohjaa already knows.
fn sykli_on(
    command: &str,
    top: &Path,
    common: &Path,
    contract: &Path,
) -> std::result::Result<process::Output, String> {
    let help = crate::gate::sykli_help(command);
    let mut sykli = Command::new("sykli");
    sykli
        .arg(command)
        .arg(contract)
        .args(crate::hook::facts_for_sykli(&help, Some(top), common))
        .current_dir(top);
    process::run(&mut sykli, None, process::LOCAL).map_err(|error| format!("{error:#}"))
}

/// KATANA D10: each cargo task of the entry's gate is given, under
/// `${worktree_cache}`, the variable of each setting it leaves unsaid
/// (`CARGO_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`), and a gate that had a lock
/// is locked again. Only when every sykli that runs the gate expands the
/// placeholder, and only when the copied lock pins the gate as copied: a
/// lock that does not means the repository changed its gate without `sykli
/// lock`, and adopt never locks what the repository did not. Says what it
/// did; never fails the adoption: a gate it cannot change is kept as copied.
fn declare_worktree_cache(top: &Path, common: &Path, dir: &Path, home: &Path) {
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
    if let Err(why) = sykli_expands_worktree_cache(home) {
        println!(
            "  {WORKTREE_CACHE}: not declared for {}: {why}, and an older sykli passes it on as \
             written, to build inside the work tree; gate runs may share a target dir across \
             worktrees until it is updated (then: ohjaa adopt --force)",
            tasks.lacking()
        );
        return;
    }
    let copied_lock = std::fs::read(&lock).ok();
    if copied_lock.is_some() {
        let kept = format!("the gate is kept as copied, without {WORKTREE_CACHE}");
        let refused = match sykli_on("validate", top, common, &contract) {
            Ok(output) if output.status.success() => None,
            Ok(output) if output.status.code() == Some(1) => Some(format!(
                "the repository's sykli.lock does not validate its sykli.json; {kept}. \
                 Validate the contract and run sykli lock in the repository, land both on its \
                 observed branch, then ohjaa adopt --force <its clone>. Sykli says: {}",
                output.failure_detail()
            )),
            Ok(output) => Some(format!(
                "sykli validate {} failed ({}), so {kept}",
                contract.display(),
                output.failure_detail()
            )),
            Err(why) => Some(format!(
                "sykli validate {} could not run ({why}), so {kept}",
                contract.display()
            )),
        };
        if let Some(why) = refused {
            eprintln!("ohjaa adopt: warning: {why}");
            return;
        }
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
    let written =
        home::write_file(&contract, &declared, None).map_err(|error| format!("{error:#}"));
    let locked = match (&written, &copied_lock) {
        (Err(why), _) => Err(why.clone()),
        (Ok(()), Some(_)) => match sykli_on("lock", top, common, &contract) {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(output.failure_detail()),
            Err(why) => Err(why),
        }
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
    let kept = tasks.kept();
    println!(
        "  {WORKTREE_CACHE}: {}, {}{}",
        tasks.given(),
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

    /// What adopt gives the one task of a contract whose command is `run`
    /// and whose other members are `members` (JSON, without the braces).
    fn given(members: &str, run: &str) -> Vec<&'static str> {
        let contract = format!(
            r#"{{"schema":"sykli-contract.v1","tasks":[{{"name":"t","run":{}{}{members}}}]}}"#,
            serde_json::to_string(run).unwrap(),
            if members.is_empty() { "" } else { "," }
        );
        let (_, tasks) = read_contract(&contract).unwrap();
        tasks
            .missing
            .into_iter()
            .flat_map(|unsaid| unsaid.vars)
            .collect()
    }

    const BOTH: &[&str] = &["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"];
    const TARGET_ONLY: &[&str] = &["CARGO_TARGET_DIR"];
    const NOTHING: &[&str] = &[];

    /// The 45 constructs of the review of #48 (finding 1), each naming both
    /// variables, and what adopt gives the task: `(case, wrong on e366124,
    /// command, given)`. Only a setting certain to reach every cargo counts:
    /// 27 (an `export` at the top level, before the first cargo) and 41 (an
    /// assignment on the cargo word itself) say the target dir, so they are
    /// given nothing. The other 43 are given both lines, and where their
    /// command does set the variables when it runs (18, 29, 30, 37, …), its
    /// own setting overrides the line. The 24 marked wrong were read as
    /// declared on e366124 while a cargo ran with neither.
    const REVIEW_48: [(u8, bool, &str, &[&str]); 45] = [
        (1, true, "(export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y); cargo test", BOTH),
        (2, true, "(unset CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR); cargo test", BOTH),
        (3, true, "true || export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (4, true, "[ -n \"$CI\" ] && export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (5, true, "if [ -n \"$CI\" ]; then export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; fi; cargo test", BOTH),
        (6, true, "f() { export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; }; cargo test", BOTH),
        (7, false, "sh -c 'export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y'; cargo test", BOTH),
        (8, false, "echo \"export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y\"; cargo test", BOTH),
        (9, true, "X=$(export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y); cargo test", BOTH),
        (10, true, "echo `export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y`; cargo test", BOTH),
        (11, true, "cat <<EOF\nexport CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y\nEOF\ncargo test", BOTH),
        (12, true, "# old; export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y\ncargo test", BOTH),
        (13, false, "cargo test # CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y", BOTH),
        (14, true, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y cargo fmt --check # don't\ncargo test # it's slow", BOTH),
        (15, true, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y cargo fmt --check; \\\ncargo test", BOTH),
        (16, false, "CARGO_TARGET_DIRX=1 CARGO_BUILD_BUILD_DIRX=1 cargo test", BOTH),
        (17, false, "echo test | CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y xargs cargo", BOTH),
        (18, false, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y nice cargo test", BOTH),
        (19, false, "nice env CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y cargo test", BOTH),
        (20, true, "readonly CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (21, true, "declare CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (22, true, "{ export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; } | cat; cargo test", BOTH),
        (23, true, "case \"$CI\" in yes) export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y;; esac; cargo test", BOTH),
        (24, true, "while false; do export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; done; cargo test", BOTH),
        (25, false, "env -i PATH=\"$PATH\" cargo test", BOTH),
        (26, true, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y echo hi; sh -c 'cargo test'", BOTH),
        (27, false, "export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", NOTHING),
        (28, false, "unset CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR; cargo test", BOTH),
        (29, false, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (30, false, "export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y && cargo test", BOTH),
        (31, false, "for d in a; do export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; done; cargo test", BOTH),
        (32, true, "test -z \"$CI\" || export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (33, false, "export \"CARGO_TARGET_DIR=x\" \"CARGO_BUILD_BUILD_DIR=y\"; cargo test", BOTH),
        (34, true, "local CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (35, true, "typeset CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (36, true, "export -n CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (37, false, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y exec cargo test", BOTH),
        (38, false, "command export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (39, false, "eval 'export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y'; cargo test", BOTH),
        (40, false, "set -a; CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y; cargo test", BOTH),
        (41, false, "cd . && CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y cargo test", NOTHING),
        (42, false, "CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y timeout 600 cargo test", BOTH),
        (43, false, "cargo test && CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y cargo doc", BOTH),
        (44, true, "(export CARGO_TARGET_DIR=t CARGO_BUILD_BUILD_DIR=b; cargo build -p x); cargo test\n", BOTH),
        (45, true, "cd sub && export CARGO_TARGET_DIR=x CARGO_BUILD_BUILD_DIR=y | true; cargo test\n", BOTH),
    ];

    /// More commands, beyond the review's: what certainly says where every
    /// cargo builds, and what only nearly does.
    const MORE: [(&str, &[&str]); 39] = [
        // Certain, for every cargo.
        ("cargo test --target-dir t", NOTHING),
        (
            "cargo build --target-dir=t && cargo test --config 'build.target-dir=\"t\"'",
            NOTHING,
        ),
        ("cargo test --config=build.target-dir=t", NOTHING),
        ("export CARGO_TARGET_DIR=\"$PWD/t\"; cargo test", NOTHING),
        (
            "export CARGO_TARGET_DIR=${PWD}/t\ncd sub && cargo test",
            NOTHING,
        ),
        (
            "set -e; export CARGO_BUILD_TARGET_DIR=t; cargo build && cargo test",
            NOTHING,
        ),
        (
            "CARGO_TARGET_DIR=t cargo build; CARGO_BUILD_TARGET_DIR=t cargo test",
            NOTHING,
        ),
        (
            "RUSTDOCFLAGS='-D warnings' CARGO_TARGET_DIR=\"$PWD/t\" cargo doc",
            NOTHING,
        ),
        ("if true; then CARGO_TARGET_DIR=t cargo test; fi", NOTHING),
        ("CARGO_TARGET_DIR=t cargo test 2>&1 | tee log", NOTHING),
        // The build dir only: the target dir's line.
        ("cd sub && CARGO_BUILD_BUILD_DIR=b cargo test", TARGET_ONLY),
        ("cargo test --config build.build-dir=b", TARGET_ONLY),
        ("export CARGO_BUILD_BUILD_DIR=b; cargo test", TARGET_ONLY),
        // Not certain: both lines.
        ("cargo test -- --target-dir t", BOTH),
        ("cargo test '--' --target-dir t", BOTH),
        ("cargo test '-''-' --target-dir t", BOTH),
        ("ARGS=--; cargo test $ARGS --target-dir t", BOTH),
        ("cargo build --target-dir t; cargo test", BOTH),
        ("cargo test # --target-dir t", BOTH),
        ("echo cargo --target-dir t; sh -c 'cargo test'", BOTH),
        ("nice cargo test --target-dir t", BOTH),
        (
            "export CARGO_TARGET_DIR=t; unset CARGO_TARGET_DIR; cargo test",
            BOTH,
        ),
        (
            "export CARGO_TARGET_DIR=t; env -u CARGO_TARGET_DIR cargo test",
            BOTH,
        ),
        ("export CARGO_TARGET_DIR=t # here\ncargo test", BOTH),
        ("export CARGO_TARGET_DIR=t >/dev/null; cargo test", BOTH),
        ("export CARGO_TARGET_DIR=\"$(pwd)/t\"; cargo test", BOTH),
        ("export -- CARGO_TARGET_DIR=t; cargo test", BOTH),
        ("declare -x CARGO_TARGET_DIR=t; cargo test", BOTH),
        (
            "cargo fmt --check\nexport CARGO_TARGET_DIR=t\ncargo test",
            BOTH,
        ),
        ("echo CARGO_TARGET_DIR=x; cargo test", BOTH),
        ("\"CARGO_TARGET_DIR\"=t cargo test", BOTH),
        ("cargo test 2>&1 | CARGO_TARGET_DIR=t tee log", BOTH),
        ("CARGO_TARGET_DIR=t sh -c 'cargo test'", BOTH),
        (
            "export NOTE='example; export CARGO_TARGET_DIR=chosen'; cargo test",
            BOTH,
        ),
        (
            "export NOTE=\"example; export CARGO_TARGET_DIR=chosen\"; cargo test",
            BOTH,
        ),
        (
            "export NOTE='example\nexport CARGO_TARGET_DIR=chosen'\ncargo test",
            BOTH,
        ),
        (
            "export NOTE=\"example\nexport CARGO_TARGET_DIR=chosen\"\ncargo test",
            BOTH,
        ),
        (
            "export CARGO_TARGET_DIR='chosen;directory'; cargo test",
            NOTHING,
        ),
        (
            "export CARGO_TARGET_DIR=\"chosen\ndirectory\"\ncargo test",
            NOTHING,
        ),
    ];

    #[test]
    fn a_command_says_where_cargo_builds_only_where_that_is_certain() {
        let mut wrong = Vec::new();
        for (case, _, run, expected) in REVIEW_48 {
            if given("", run) != expected {
                wrong.push(format!("{case}: {run:?} is given {:?}", given("", run)));
            }
        }
        for (run, expected) in MORE {
            if given("", run) != expected {
                wrong.push(format!("{run:?} is given {:?}", given("", run)));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A cargo that writes down, one line a run, whether it was told where
    /// to build: the target dir (`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`,
    /// or `--target-dir` or `--config build.target-dir=…` before `--`) and the
    /// build dir (`CARGO_BUILD_BUILD_DIR`, or `--config build.build-dir=…`).
    fn fake_cargo(log: &Path) -> String {
        format!(
            r#"#!/bin/sh
target=${{CARGO_TARGET_DIR+yes}}${{CARGO_BUILD_TARGET_DIR+yes}}
build=${{CARGO_BUILD_BUILD_DIR+yes}}
config=
for arg in "$@"; do
  [ "$arg" = -- ] && break
  [ -n "$config" ] && arg="--config=$arg"
  config=
  case "$arg" in
    --target-dir|--target-dir=*|--config=build.target-dir*) target=yes;;
    --config=build.build-dir*) build=yes;;
    --config) config=yes;;
  esac
done
echo "target=${{target:-no}} build=${{build:-no}}" >> '{}'
"#,
            log.display()
        )
    }

    /// What adopt counts as said holds under sh: each command, run as sykli
    /// runs a task (`sh -c`, with only PATH, HOME and TMPDIR) and given no
    /// line, tells every cargo that runs the target dir when adopt gives no
    /// `CARGO_TARGET_DIR`, and the build dir when adopt gives that alone. (A
    /// line adopt does give either reaches cargo or is overridden by the
    /// command's own setting.)
    #[test]
    fn what_adopt_counts_as_said_holds_under_sh() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("ohjaa-under-sh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (bin, work, log) = (dir.join("bin"), dir.join("work"), dir.join("cargo-log"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(bin.join("cargo"), fake_cargo(&log)).unwrap();
        std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let runs = REVIEW_48
            .iter()
            .map(|(_, _, run, _)| *run)
            .chain(MORE.iter().map(|(run, _)| *run));
        let mut wrong = Vec::new();
        let mut ran = 0;
        for run in runs {
            let _ = std::fs::remove_file(&log);
            Command::new("/bin/sh")
                .args(["-c", "--", run])
                .current_dir(&work)
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("HOME", &dir)
                .env("TMPDIR", &dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            let given = given("", run);
            for told in std::fs::read_to_string(&log).unwrap_or_default().lines() {
                ran += 1;
                let target = told.contains("target=yes");
                let build = told.contains("build=yes");
                if !given.contains(&"CARGO_TARGET_DIR") && !target
                    || given == ["CARGO_TARGET_DIR"] && !build
                {
                    wrong.push(format!(
                        "{run:?} is given {given:?}, and a cargo ran: {told}"
                    ));
                }
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(ran > 60, "the fake cargo ran {ran} times");
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The review of #48, finding 2: a task that certainly says where cargo
    /// builds (its target dir, by its env, its inherit, its command or a
    /// flag) is given nothing, not even the build dir's line: cargo's build
    /// dir defaults to that target dir, so the task's layout stays as it
    /// meant (test binaries under `<its target dir>/debug/deps`). A task that
    /// says only its build dir is given the target dir's line, and one that
    /// says neither both.
    #[test]
    fn a_task_that_says_its_target_dir_is_given_nothing() {
        let cases: &[(&str, &str, &[&str])] = &[
            ("", "cargo test", BOTH),
            (
                r#""env":{"RUST_BACKTRACE":"1"},"inherit":["HOME"]"#,
                "cargo test",
                BOTH,
            ),
            // The target dir, said: nothing.
            (r#""env":{"CARGO_TARGET_DIR":"t"}"#, "cargo test", NOTHING),
            (
                r#""env":{"CARGO_BUILD_TARGET_DIR":"t"}"#,
                "cargo test",
                NOTHING,
            ),
            (r#""inherit":["CARGO_TARGET_DIR"]"#, "cargo test", NOTHING),
            (
                r#""inherit":["CARGO_BUILD_TARGET_DIR"]"#,
                "cargo test",
                NOTHING,
            ),
            ("", "CARGO_TARGET_DIR=t cargo test", NOTHING),
            ("", "export CARGO_BUILD_TARGET_DIR=t; cargo test", NOTHING),
            ("", "cargo test --target-dir=t", NOTHING),
            // A CARGO_TARGET_DIR line would override this one too.
            ("", "cargo test --config 'build.target-dir=\"t\"'", NOTHING),
            (
                r#""env":{"CARGO_BUILD_BUILD_DIR":"b"}"#,
                "cargo test --target-dir t",
                NOTHING,
            ),
            // Only the build dir, said: the target dir's line.
            (
                r#""env":{"CARGO_BUILD_BUILD_DIR":"b"}"#,
                "cargo test",
                TARGET_ONLY,
            ),
            (
                r#""inherit":["CARGO_BUILD_BUILD_DIR"]"#,
                "cargo test",
                TARGET_ONLY,
            ),
            ("", "CARGO_BUILD_BUILD_DIR=b cargo test", TARGET_ONLY),
            ("", "cargo test --config build.build-dir=b", TARGET_ONLY),
        ];
        let mut wrong = Vec::new();
        for &(members, run, expected) in cases {
            if given(members, run) != expected {
                wrong.push(format!(
                    "{{{members}}} {run:?} is given {:?}",
                    given(members, run)
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        let task = |json: &str| serde_json::from_str::<Json>(json).unwrap();
        assert!(unsaid(&task(r#"{"env":["x"]}"#), "cargo test").is_err());
        assert!(unsaid(&task(r#"{"inherit":"x"}"#), "cargo test").is_err());
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
        let text = r#"{"schema":"sykli-contract.v1","tasks":[{"inputs":["a"],"name":"t","run":"cargo test","outputs":["o"]},{"name":"u","run":"true"},{"name":"v","run":"cargo test","env":{"CARGO_BUILD_BUILD_DIR":"own"}},{"name":"w","run":"cargo test","env":{"CARGO_TARGET_DIR":"own"}}],"z":1.5}"#;
        let (mut contract, tasks) = read_contract(text).unwrap();
        assert_eq!(tasks.lacking(), "t, v (no CARGO_TARGET_DIR)");
        assert_eq!(
            tasks.given(),
            "CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR declared for t; CARGO_TARGET_DIR for v"
        );
        declare_in(&mut contract, &tasks.missing);
        assert_eq!(
            serde_json::to_string(&contract).unwrap(),
            r#"{"schema":"sykli-contract.v1","tasks":[{"inputs":["a"],"name":"t","run":"cargo test","env":{"CARGO_TARGET_DIR":"${worktree_cache}/cargo-target","CARGO_BUILD_BUILD_DIR":"${worktree_cache}/cargo-build"},"outputs":["o"]},{"name":"u","run":"true"},{"name":"v","run":"cargo test","env":{"CARGO_BUILD_BUILD_DIR":"own","CARGO_TARGET_DIR":"${worktree_cache}/cargo-target"}},{"name":"w","run":"cargo test","env":{"CARGO_TARGET_DIR":"own"}}],"z":1.5}"#
        );
    }

    #[test]
    fn a_task_names_the_placeholder_wherever_a_string_of_it_does() {
        let (_, tasks) = read_contract(
            r#"{"schema":"sykli-contract.v1","tasks":[{"name":"a","run":"cargo test","env":{"CARGO_TARGET_DIR":"${worktree_cache}/t"}},{"name":"b","run":"make","env":{"X":"${worktree_cache}"}},{"name":"c","run":"cargo test"}]}"#,
        )
        .unwrap();
        assert_eq!(tasks.placeholder, ["a", "b"]);
        assert_eq!(tasks.lacking(), "c");
    }
}
