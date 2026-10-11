//! `ohjaa apply` and `ohjaa apply --undo`.
//!
//! `apply` writes the home's generated files (each entry's `hooks/` and
//! `gated.config`, `git/config`, and `roots` on a first run) and changes three
//! things outside it, and only those: one `include.path` in git's global
//! config, a marked block in git's global excludes file, and the launcher line
//! in `~/.zshrc`. Each is recorded in `applied.toml` before it is made (what
//! the zshrc line replaced, whether a file, a blank line or a final newline
//! was there), so `--undo` puts back exactly what was there, even after an
//! apply that failed midway.
//!
//! Every change is planned before any is made: a refusal leaves nothing half
//! applied, and `--dry-run` prints the same plan. Dispatchers are only ever
//! added: a spare one costs a process, a missing one silently skips a check,
//! so only `--prune` (with the roots written down) or `--undo` removes them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::flat_toml;
use crate::gate;
use crate::home;
use crate::hook::{
    dispatcher, dispatcher_target, on_path, quote_path, Dispatch, HOOK_NAMES, NOT_DISPATCHED,
};

/// The change was refused.
const REFUSED: i32 = 3;

/// The unguarded line an earlier `apply` wrote; still recognised, and
/// replaced.
const BARE_LAUNCH_LINE: &str = r#"eval "$(ohjaa shell-init zsh)""#;

/// The launcher line: the ohjaa that ran `apply`, by its path, guarded like
/// the line it replaces, so a shell where that ohjaa is gone starts quietly.
pub fn launch_line(ohjaa: &Path) -> String {
    let quoted = quote_path(ohjaa);
    format!("[[ -x {quoted} ]] && eval \"$({quoted} shell-init zsh)\"")
}

/// The ohjaa a launcher line `apply` wrote runs (`ohjaa` for the unguarded
/// form); `None` for any other line.
pub fn launch_target(line: &str) -> Option<PathBuf> {
    let line = line.trim();
    if line == BARE_LAUNCH_LINE {
        return Some(PathBuf::from("ohjaa"));
    }
    let (path, _) = line.strip_prefix("[[ -x '")?.split_once("' ]] && eval ")?;
    let path = PathBuf::from(path.replace("'\\''", "'"));
    (launch_line(&path) == line).then_some(path)
}

pub const LAUNCH_COMMENT: &str = "# Route claude/codex through ohjaa (written by `ohjaa apply`). \
                                  Bypass: command claude, or OHJAA_WRAP=0.";
/// The line that sourced the launcher before it was part of ohjaa.
const OLD_LAUNCHER: &str = "ohjaa-launch.zsh";

pub const IGNORE_BEGIN: &str =
    "# >>> ohjaa apply: workflow files no repository tracks (edits inside are replaced)";
const IGNORE_END: &str = "# <<< ohjaa apply";
/// What every repository ignores without being told: the workflow's own files.
pub const IGNORED: &[&str] = &[".sykli/", ".teko/"];

const RECORD_SCHEMA: &str = "ohjaa-applied.v1";

pub fn record_path(home: &Path) -> PathBuf {
    home.join("applied.toml")
}

/// What `apply` changed outside the home, key by key.
type Record = BTreeMap<String, String>;

fn read_record(home: &Path) -> Result<Record> {
    let path = record_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Record::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut tables =
        flat_toml::parse(&text).map_err(|why| anyhow::anyhow!("{}: {why}", path.display()))?;
    let mut record = tables.remove("").unwrap_or_default();
    record.remove("schema");
    Ok(record)
}

fn write_record(home: &Path, record: &Record) -> Result<()> {
    let mut text = format!(
        "# What `ohjaa apply` changed outside the home, so that `ohjaa apply --undo`\n\
         # puts back exactly what was there. Written by apply; removed by --undo.\n\
         schema = {}\n",
        flat_toml::quote(RECORD_SCHEMA)
    );
    for (key, value) in record {
        text.push_str(&format!("{key} = {}\n", flat_toml::quote(value)));
    }
    home::write_file(&record_path(home), &text, None)
}

fn flag(value: bool) -> String {
    value.to_string()
}

fn ignore_block() -> Vec<String> {
    let mut block = vec![IGNORE_BEGIN.to_owned()];
    block.extend(IGNORED.iter().map(|line| (*line).to_owned()));
    block.push(IGNORE_END.to_owned());
    block
}

/// Where the managed block is: its first and last line.
fn find_block(lines: &[&str]) -> Result<Option<(usize, usize)>, String> {
    let Some(begin) = lines
        .iter()
        .position(|line| line.trim_end().starts_with("# >>> ohjaa apply"))
    else {
        return Ok(None);
    };
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
    Ok(Some((begin, end)))
}

fn joined(lines: Vec<String>) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}

/// `current` with the managed block holding exactly [`IGNORED`]: replaced
/// where it is, else appended (after a blank line, the bool, when the file
/// does not end in one). An opened block that never closes is an error,
/// since replacing it could take lines the person wrote.
pub fn ignore_with_block(current: &str) -> Result<(String, bool), String> {
    let lines: Vec<&str> = current.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut blank = false;
    match find_block(&lines)? {
        Some((begin, end)) => {
            out.extend(lines[..begin].iter().map(|l| (*l).to_owned()));
            out.extend(ignore_block());
            out.extend(lines[end + 1..].iter().map(|l| (*l).to_owned()));
        }
        None => {
            out.extend(lines.iter().map(|l| (*l).to_owned()));
            if out.last().is_some_and(|line| !line.trim().is_empty()) {
                out.push(String::new());
                blank = true;
            }
            out.extend(ignore_block());
        }
    }
    Ok((joined(out), blank))
}

/// `current` without the managed block, and without the blank line apply
/// put before it. `None` when there is no block.
fn ignore_without_block(current: &str, blank_added: bool) -> Result<Option<String>, String> {
    let lines: Vec<&str> = current.lines().collect();
    let Some((mut begin, end)) = find_block(&lines)? else {
        return Ok(None);
    };
    if blank_added && begin > 0 && lines[begin - 1].trim().is_empty() {
        begin -= 1;
    }
    let mut out: Vec<String> = lines[..begin].iter().map(|l| (*l).to_owned()).collect();
    out.extend(lines[end + 1..].iter().map(|l| (*l).to_owned()));
    Ok(Some(joined(out)))
}

/// A change to `~/.zshrc`: the new text and what it removes and adds.
#[derive(Debug, PartialEq)]
pub struct ZshrcPlan {
    pub contents: String,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    /// A blank line went in before an appended launcher.
    pub blank: bool,
}

/// A line that sources the old launcher file: `source …ohjaa-launch.zsh` or
/// `. …ohjaa-launch.zsh`, alone or after `&&`. Any other line that names the
/// file (an alias that edits it, a comment) is the person's, and stays.
pub fn sources_old_launcher(line: &str) -> bool {
    let line = line.trim();
    !line.starts_with('#')
        && line.split("&&").map(str::trim).any(|part| {
            (part.starts_with("source ") || part.starts_with(". ")) && part.contains(OLD_LAUNCHER)
        })
}

/// `current` with the line `desired` (from [`launch_line`]): it replaces the
/// old `source …/ohjaa-launch.zsh` line, or a launcher line an earlier apply
/// wrote for another path (and the ohjaa comment line just above either),
/// or is appended when there was none. `None` when nothing needs to change.
pub fn zshrc_with_launcher(current: &str, desired: &str) -> Option<ZshrcPlan> {
    let lines: Vec<&str> = current.lines().collect();
    let has_launch = lines.iter().any(|line| line.trim() == desired);
    let mut drop = vec![false; lines.len()];
    for (at, line) in lines.iter().enumerate() {
        let stale_launch = launch_target(line).is_some() && line.trim() != desired;
        if sources_old_launcher(line) || stale_launch {
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
        vec![LAUNCH_COMMENT.to_owned(), desired.to_owned()]
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
    let mut blank = false;
    if !placed {
        if out.last().is_some_and(|line| !line.trim().is_empty()) {
            out.push(String::new());
            blank = true;
        }
        out.extend(added.iter().cloned());
    }
    Some(ZshrcPlan {
        contents: joined(out),
        removed,
        added,
        blank,
    })
}

/// `current` with the launcher line (and apply's comment above it) taken
/// out and `replaced`, the lines it once replaced, put back in its place.
/// `None` when the launcher line is not there.
fn zshrc_without_launcher(current: &str, replaced: &[String], blank: bool) -> Option<ZshrcPlan> {
    let lines: Vec<&str> = current.lines().collect();
    let at = lines
        .iter()
        .position(|line| launch_target(line).is_some())?;
    let mut begin = at;
    if begin > 0 && lines[begin - 1] == LAUNCH_COMMENT {
        begin -= 1;
    }
    let removed: Vec<String> = lines[begin..=at].iter().map(|l| (*l).to_owned()).collect();
    if replaced.is_empty() && blank && begin > 0 && lines[begin - 1].trim().is_empty() {
        begin -= 1;
    }
    let mut out: Vec<String> = lines[..begin].iter().map(|l| (*l).to_owned()).collect();
    out.extend(replaced.iter().cloned());
    out.extend(lines[at + 1..].iter().map(|l| (*l).to_owned()));
    Some(ZshrcPlan {
        contents: joined(out),
        removed,
        added: replaced.to_vec(),
        blank,
    })
}

pub fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write `contents`, or remove the file when it is empty and `remove_empty`.
fn write_or_remove(path: &Path, contents: &str, remove_empty: bool) -> Result<()> {
    if remove_empty && contents.is_empty() {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("cannot remove {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    home::write_file(path, contents, None)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("cannot remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// A planned change: what it is (as "would …" and as done), what to record
/// once it is made, and the making.
struct Step {
    will: String,
    did: String,
    record: Vec<(String, String)>,
    run: Box<dyn FnOnce() -> Result<()>>,
}

fn step(
    verb: (&str, &str),
    what: String,
    record: Vec<(String, String)>,
    run: impl FnOnce() -> Result<()> + 'static,
) -> Step {
    Step {
        will: format!("would {} {what}", verb.0),
        did: format!("{} {what}", verb.1),
        record,
        run: Box::new(run),
    }
}

fn zshrc_lines(plan: &ZshrcPlan) -> String {
    let mut text = String::new();
    for line in &plan.removed {
        text.push_str(&format!("\n  - {line}"));
    }
    for line in &plan.added {
        text.push_str(&format!("\n  + {line}"));
    }
    text
}

pub struct Options<'a> {
    pub dry_run: bool,
    pub force: bool,
    /// Also remove dispatchers no clone under the roots needs.
    pub prune: bool,
    /// The ohjaa the hooks and the launcher run, instead of the one on PATH.
    pub bin: Option<&'a Path>,
}

/// The ohjaa the dispatchers and the launcher line run: `--bin`, else the
/// `ohjaa` on PATH as found (a link kept a link). A different binary running
/// apply (a `cargo run`, a `target/` build) would re-point every dispatcher
/// at itself, so that is refused unless `--bin` names it.
fn chosen_ohjaa(bin: Option<&Path>) -> std::result::Result<PathBuf, String> {
    if let Some(bin) = bin {
        let bin = std::path::absolute(bin).unwrap_or_else(|_| bin.to_path_buf());
        return if gate::executable(&bin) {
            Ok(bin)
        } else {
            Err(format!("--bin {} is not an executable file", bin.display()))
        };
    }
    let Some(found) = on_path("ohjaa") else {
        return Err(
            "ohjaa is not on PATH, so the hooks would have no ohjaa to run; \
                    install it, or name the binary with --bin <path>"
                .to_owned(),
        );
    };
    let running = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("cannot tell which ohjaa is running: {error}"))?;
    if std::fs::canonicalize(&found).ok().as_deref() != Some(running.as_path()) {
        return Err(format!(
            "this ohjaa ({}) is not the one on PATH ({}); every dispatcher and the launcher \
             line would run it. Run the ohjaa on PATH, or name the binary with --bin <path>",
            running.display(),
            found.display()
        ));
    }
    Ok(found)
}

/// The record entries a step makes, kept before the step runs, so an apply
/// that fails midway can still be undone. The first value of an undo key is
/// the one undo needs; `ohjaa` and `sykli` say what the dispatchers run now.
fn keep_record(home: &Path, entries: &[(String, String)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut record = read_record(home)?;
    for (key, value) in entries {
        if key == "ohjaa" || key == "sykli" {
            record.insert(key.clone(), value.clone());
        } else {
            record.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    write_record(home, &record)
}

/// Whether text that is not empty ends without a newline (undo keeps it so).
fn lacks_final_newline(text: &str) -> bool {
    !text.is_empty() && !text.ends_with('\n')
}

fn roots_text(roots: &[PathBuf]) -> String {
    let mut text = String::from(
        "# Where ohjaa looks for repositories (apply, doctor, fleet, report): one\n\
         # directory per line. Written by `ohjaa apply` on its first run.\n",
    );
    for root in roots {
        text.push_str(&format!("{}\n", root.display()));
    }
    text
}

/// Read git's global config once; when an unreadable generated file breaks
/// that (every git command fails then, apply's own among them), remove what
/// git cannot read and read again: apply writes it anew below.
fn global_facts(home: &Path, repair: bool) -> Result<gate::GlobalFacts> {
    match gate::GlobalFacts::read() {
        Ok(facts) => Ok(facts),
        Err(error) if repair => {
            let mut generated = vec![gate::include_file(home)];
            generated.extend(
                gate::entry_names(home)?
                    .into_iter()
                    .map(|entry| gate::entry_gated(&entry.dir)),
            );
            let mut removed = false;
            for path in generated {
                if path.is_file() && gate::check_git_config(&path).is_err() {
                    std::fs::remove_file(&path)
                        .with_context(|| format!("cannot remove {}", path.display()))?;
                    println!(
                        "ohjaa apply: removed {}, which git could not read; it is written again below",
                        path.display()
                    );
                    removed = true;
                }
            }
            if !removed {
                return Err(error);
            }
            gate::GlobalFacts::read()
        }
        Err(error) => Err(error),
    }
}

pub fn apply(
    options: Options<'_>,
    explicit_roots: &[PathBuf],
    git_root: Option<&Path>,
) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let version = gate::require_git()?;
    let global = global_facts(&home, !options.dry_run)?;
    let mut steps: Vec<Step> = Vec::new();

    let ohjaa = match chosen_ohjaa(options.bin) {
        Ok(ohjaa) => ohjaa,
        Err(why) => {
            eprintln!("ohjaa apply: refusing: {why}");
            return Ok(REFUSED);
        }
    };
    let sykli = on_path("sykli");
    if sykli.is_none() {
        eprintln!(
            "ohjaa apply: warning: sykli is not on PATH; pushes to an entry's branch are \
             refused until it is (and apply runs again)"
        );
    }
    let dispatch = Dispatch {
        ohjaa: ohjaa.clone(),
        sykli: sykli.clone(),
    };
    let mut dispatch_record = vec![("ohjaa".to_owned(), ohjaa.to_string_lossy().into_owned())];
    if let Some(sykli) = &sykli {
        dispatch_record.push(("sykli".to_owned(), sykli.to_string_lossy().into_owned()));
    }

    // A global core.hooksPath runs in every repository and overrides the one
    // ohjaa scopes to gated ones: refused unless --force says to remove it.
    if let Some((path, direct, origin)) = global.hooks_path() {
        let ours = home::same_path(Path::new(&path), &home::hooks_dir(&home));
        if !ours && !options.force {
            eprintln!(
                "ohjaa apply: refusing: git's global core.hooksPath is {path}. It runs in every \
                 repository and overrides the core.hooksPath ohjaa gives the gated ones, so \
                 the gate would not run. Move what it does into the repositories' own hooks \
                 (ohjaa's dispatchers chain to them), or remove it with: ohjaa apply --force"
            );
            return Ok(REFUSED);
        }
        if !direct {
            eprintln!(
                "ohjaa apply: refusing: git's global core.hooksPath ({path}) is set in {origin}, \
                 a file git's global config includes; ohjaa edits only the global file \
                 itself, so remove it there"
            );
            return Ok(REFUSED);
        }
        let record = if ours {
            Vec::new()
        } else {
            vec![("hooks_path_removed".to_owned(), path.clone())]
        };
        steps.push(step(
            ("remove", "removed"),
            format!("git's global core.hooksPath ({path})"),
            record,
            || home::unset_global("core.hooksPath", None),
        ));
    }

    // The roots: written down on the first run, so the hooks apply finds
    // never depend on where it is run from.
    let roots_file = crate::roots::config_path().context("no config home")?;
    let configured = roots_file.is_file();
    let roots: Vec<PathBuf> = crate::roots::load(explicit_roots, git_root)
        .into_iter()
        .map(|root| std::path::absolute(&root).unwrap_or(root))
        .collect();
    let repos = crate::roots::git_repos(&roots)?;
    let roots_said = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if options.prune && !configured {
        eprintln!(
            "ohjaa apply: refusing --prune: it removes dispatchers no clone under the roots \
             needs, so the roots must be written down, and {} does not exist (apply writes it \
             on a run that finds repositories)",
            roots_file.display()
        );
        return Ok(REFUSED);
    }
    if !configured {
        if repos.is_empty() {
            eprintln!(
                "ohjaa apply: warning: {roots_said} holds no repositories, and there is no {}: \
                 the hooks clones have of their own are not known; run apply from your \
                 projects folder, or pass --root <dir>",
                roots_file.display()
            );
        } else {
            let (target, text) = (roots_file.clone(), roots_text(&roots));
            steps.push(step(
                ("write", "wrote"),
                format!(
                    "{} with {roots_said}, the roots apply and doctor scan (edit it to change them)",
                    roots_file.display()
                ),
                Vec::new(),
                move || home::write_file(&target, &text, None),
            ));
        }
    }

    // Each entry's dispatchers: pre-push, and each hook its clones have of
    // their own. Only ever added here; --prune (or --undo) removes.
    let gated = gate::gated_clones(&home, &repos)?;
    for (entry, needed) in gate::needed_hooks(&home, &gated)? {
        let hooks = gate::entry_hooks(&entry);
        let mut write = Vec::new();
        let mut spare = Vec::new();
        for name in HOOK_NAMES {
            let path = hooks.join(name);
            let current = std::fs::read_to_string(&path).ok();
            if needed.contains(name) {
                if current.as_deref() != Some(dispatcher(name, &dispatch).as_str())
                    || !gate::executable(&path)
                {
                    write.push(name);
                }
            } else if current.is_some_and(|text| dispatcher_target(name, &text).is_some()) {
                spare.push(name);
            }
        }
        if !write.is_empty() {
            let (dir, target, names) = (hooks.clone(), dispatch.clone(), write.clone());
            steps.push(step(
                ("write", "wrote"),
                format!(
                    "{} dispatcher{} into {} (exec {}): {}",
                    write.len(),
                    if write.len() == 1 { "" } else { "s" },
                    hooks.display(),
                    ohjaa.display(),
                    write.join(", ")
                ),
                dispatch_record.clone(),
                move || {
                    for name in names {
                        home::write_file(&dir.join(name), &dispatcher(name, &target), Some(0o755))?;
                    }
                    Ok(())
                },
            ));
        }
        if !spare.is_empty() && options.prune {
            let (dir, names) = (hooks.clone(), spare.clone());
            steps.push(step(
                ("remove", "removed"),
                format!(
                    "dispatchers no clone under the roots needs from {}: {}",
                    hooks.display(),
                    spare.join(", ")
                ),
                Vec::new(),
                move || {
                    names
                        .iter()
                        .try_for_each(|name| remove_if_present(&dir.join(name)))
                },
            ));
        }
    }

    // The generated git config: one includeIf per entry remote pattern, each
    // including that entry's gated.config.
    for (path, text) in gate::generated(&home, &global.rewrites())? {
        if read_or_empty(&path)? != text {
            let (target, contents) = (path.clone(), text);
            steps.push(step(
                ("write", "wrote"),
                path.display().to_string(),
                Vec::new(),
                move || gate::write_generated(&target, &contents),
            ));
        }
    }
    let include = gate::include_file(&home);
    if !global.includes(&include) {
        let target = gate::global_write_target()?;
        let value = include.to_string_lossy().into_owned();
        steps.push(step(
            ("add", "added"),
            format!(
                "include.path = {value} to git's global config, {} (git {}.{}: repositories \
                 whose remote names an entry run ohjaa's hooks, no other does)",
                target.display(),
                version.0,
                version.1
            ),
            vec![
                ("include".to_owned(), value.clone()),
                (
                    "gitconfig".to_owned(),
                    target.to_string_lossy().into_owned(),
                ),
                ("gitconfig_created".to_owned(), flag(!target.exists())),
            ],
            move || home::add_global("include.path", &value),
        ));
    }

    let ignore = global.excludes_file()?;
    let ignore_now = read_or_empty(&ignore)?;
    let (ignore_new, blank) = ignore_with_block(&ignore_now)
        .map_err(|why| anyhow::anyhow!("{}: {why}", ignore.display()))?;
    if ignore_new != ignore_now {
        let record = vec![
            (
                "ignore_file".to_owned(),
                ignore.to_string_lossy().into_owned(),
            ),
            ("ignore_created".to_owned(), flag(!ignore.exists())),
            ("ignore_blank".to_owned(), flag(blank)),
            (
                "ignore_newline".to_owned(),
                flag(!lacks_final_newline(&ignore_now)),
            ),
        ];
        let target = ignore.clone();
        steps.push(step(
            ("add", "added"),
            format!(
                "{} to {} (git's global excludes), in a block marked \"ohjaa apply\"",
                IGNORED.join(" "),
                ignore.display()
            ),
            record,
            move || home::write_file(&target, &ignore_new, None),
        ));
    }

    let zshrc = home::zshrc()?;
    let zshrc_now = read_or_empty(&zshrc)?;
    if let Some(plan) = zshrc_with_launcher(&zshrc_now, &launch_line(&ohjaa)) {
        let record = vec![
            ("zshrc".to_owned(), zshrc.to_string_lossy().into_owned()),
            ("zshrc_created".to_owned(), flag(!zshrc.exists())),
            ("zshrc_replaced".to_owned(), plan.removed.join("\n")),
            ("zshrc_blank".to_owned(), flag(plan.blank)),
            (
                "zshrc_newline".to_owned(),
                flag(!lacks_final_newline(&zshrc_now)),
            ),
        ];
        let (target, contents) = (zshrc.clone(), plan.contents.clone());
        steps.push(step(
            ("change", "changed"),
            format!("{}:{}", zshrc.display(), zshrc_lines(&plan)),
            record,
            move || home::write_file(&target, &contents, None),
        ));
    }

    // Which ohjaa and sykli the dispatchers run: adopt writes an entry's
    // dispatchers with them, whatever else this run changes.
    if !options.dry_run {
        let record = read_record(&home)?;
        if dispatch_record
            .iter()
            .any(|(key, value)| record.get(key) != Some(value))
        {
            keep_record(&home, &dispatch_record)?;
        }
    }
    if steps.is_empty() {
        println!(
            "ohjaa apply: nothing to change; the workflow is applied from {}",
            home.display()
        );
        return Ok(0);
    }
    if options.dry_run {
        for step in &steps {
            println!("ohjaa apply: {}", step.will);
        }
        println!("ohjaa apply: dry run; nothing was changed");
        return Ok(0);
    }
    for step in steps {
        // Recorded before it is made: a failure after it can still be undone.
        keep_record(&home, &step.record)?;
        (step.run)()?;
        println!("ohjaa apply: {}", step.did);
    }
    Ok(0)
}

/// Text as it was before apply: without a final newline when it had none.
fn as_it_was(text: String, newline: bool) -> String {
    if newline {
        text
    } else {
        text.strip_suffix('\n').map(str::to_owned).unwrap_or(text)
    }
}

/// `ohjaa apply --undo`: take back exactly what apply changed, and remove
/// what it generated. `repos/` entries and `roots` stay.
pub fn undo(dry_run: bool) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let record = read_record(&home)?;
    let recorded = |key: &str| record.get(key).cloned();
    let yes = |key: &str| record.get(key).is_some_and(|value| value == "true");
    // A key absent from the record says nothing was changed: keep as it is.
    let newline = |key: &str| record.get(key).is_none_or(|value| value == "true");
    let mut steps: Vec<Step> = Vec::new();
    let global = gate::GlobalFacts::read()?;

    let include = recorded("include")
        .map(PathBuf::from)
        .unwrap_or_else(|| gate::include_file(&home));
    if global.includes(&include) {
        let value = include.to_string_lossy().into_owned();
        let created = yes("gitconfig_created");
        let file = recorded("gitconfig").map(PathBuf::from);
        steps.push(step(
            ("remove", "removed"),
            format!("include.path = {value} from git's global config"),
            Vec::new(),
            move || {
                home::unset_global("include.path", Some(&value))?;
                // A global config that apply created is removed, not left empty.
                match file {
                    Some(file) if created && read_or_empty(&file)?.trim().is_empty() => {
                        remove_if_present(&file)
                    }
                    _ => Ok(()),
                }
            },
        ));
    }
    if let Some(path) = recorded("hooks_path_removed") {
        if global.hooks_path().is_none() {
            steps.push(step(
                ("restore", "restored"),
                format!(
                    "git's global core.hooksPath = {path}, which apply --force removed (at \
                     the end of [core])"
                ),
                Vec::new(),
                move || home::set_global("core.hooksPath", &path),
            ));
        }
    }

    let ignore = match recorded("ignore_file") {
        Some(file) => PathBuf::from(file),
        None => global.excludes_file()?,
    };
    let ignore_now = read_or_empty(&ignore)?;
    if let Some(without) = ignore_without_block(&ignore_now, yes("ignore_blank"))
        .map_err(|why| anyhow::anyhow!("{}: {why}", ignore.display()))?
    {
        let without = as_it_was(without, newline("ignore_newline"));
        let remove_empty = yes("ignore_created");
        let target = ignore.clone();
        steps.push(step(
            ("remove", "removed"),
            format!("the \"ohjaa apply\" block from {}", ignore.display()),
            Vec::new(),
            move || write_or_remove(&target, &without, remove_empty),
        ));
    }

    let zshrc = recorded("zshrc")
        .map(PathBuf::from)
        .map_or_else(home::zshrc, Ok)?;
    let replaced: Vec<String> = recorded("zshrc_replaced")
        .filter(|text| !text.is_empty())
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    if let Some(plan) =
        zshrc_without_launcher(&read_or_empty(&zshrc)?, &replaced, yes("zshrc_blank"))
    {
        if record.is_empty() {
            eprintln!(
                "ohjaa apply --undo: no record of what the launcher line replaced \
                 ({} is missing); nothing is put back in its place",
                record_path(&home).display()
            );
        }
        let contents = as_it_was(plan.contents.clone(), newline("zshrc_newline"));
        let remove_empty = yes("zshrc_created");
        let target = zshrc.clone();
        steps.push(step(
            ("change", "changed"),
            format!("{}:{}", zshrc.display(), zshrc_lines(&plan)),
            Vec::new(),
            move || write_or_remove(&target, &contents, remove_empty),
        ));
    }

    // What apply generated: each entry's dispatchers (only files that still
    // are one) and gated.config, the git config, the record; then the
    // folders, if nothing else is in them.
    let mut generated: Vec<PathBuf> = Vec::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    for entry in gate::entry_names(&home)? {
        let hooks = gate::entry_hooks(&entry.dir);
        generated.extend(
            HOOK_NAMES
                .into_iter()
                .filter(|name| {
                    std::fs::read_to_string(hooks.join(name))
                        .is_ok_and(|text| dispatcher_target(name, &text).is_some())
                })
                .map(|name| hooks.join(name)),
        );
        generated.push(gate::entry_gated(&entry.dir));
        folders.push(hooks);
    }
    generated.extend([gate::include_file(&home), record_path(&home)]);
    folders.push(gate::git_dir(&home));
    generated.retain(|path| path.exists());
    if !generated.is_empty() {
        steps.push(step(
            ("remove", "removed"),
            format!(
                "what apply generated in {}: {}",
                home.display(),
                generated
                    .iter()
                    .map(|path| path
                        .strip_prefix(&home)
                        .unwrap_or(path)
                        .display()
                        .to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Vec::new(),
            move || {
                generated
                    .iter()
                    .try_for_each(|path| remove_if_present(path))?;
                for folder in folders.into_iter().filter(|folder| folder.exists()) {
                    if let Err(error) = std::fs::remove_dir(&folder) {
                        eprintln!("ohjaa apply --undo: kept {}: {error}", folder.display());
                    }
                }
                Ok(())
            },
        ));
    }

    if steps.is_empty() {
        println!("ohjaa apply --undo: nothing to undo");
        return Ok(0);
    }
    for step in steps {
        if dry_run {
            println!("ohjaa apply --undo: {}", step.will);
            continue;
        }
        (step.run)()?;
        println!("ohjaa apply --undo: {}", step.did);
    }
    if dry_run {
        println!("ohjaa apply --undo: dry run; nothing was changed");
    } else {
        println!(
            "ohjaa apply --undo: kept the entries in {} and the roots",
            home.join("repos").display()
        );
    }
    Ok(0)
}

/// For `adopt`: the generated git files brought up to date, and, once apply
/// has run (its record names the ohjaa the dispatchers run), the entry's own
/// dispatchers: pre-push and each hook of `own` it lacks. Says what it
/// changed.
pub fn regenerate(
    home: &Path,
    entry: &Path,
    own: &[&'static str],
    rewrites: &[gate::Rewrite],
) -> Result<Vec<String>> {
    let mut said = Vec::new();
    for (path, text) in gate::generated(home, rewrites)? {
        if read_or_empty(&path)? != text {
            gate::write_generated(&path, &text)?;
            said.push(format!("wrote {}", path.display()));
        }
    }
    let record = read_record(home)?;
    let Some(ohjaa) = record.get("ohjaa").map(PathBuf::from) else {
        said.push("no dispatchers yet: `ohjaa apply` has not run".to_owned());
        return Ok(said);
    };
    let dispatch = Dispatch {
        ohjaa,
        sykli: record.get("sykli").map(PathBuf::from),
    };
    let hooks = gate::entry_hooks(entry);
    let mut wanted: BTreeSet<&str> = own
        .iter()
        .copied()
        .filter(|name| !NOT_DISPATCHED.contains(name))
        .collect();
    wanted.insert("pre-push");
    let missing: Vec<&str> = wanted
        .into_iter()
        .filter(|name| !hooks.join(name).is_file())
        .collect();
    for name in &missing {
        home::write_file(&hooks.join(name), &dispatcher(name, &dispatch), Some(0o755))?;
    }
    if !missing.is_empty() {
        said.push(format!(
            "wrote dispatchers into {}: {}",
            hooks.display(),
            missing.join(", ")
        ));
    }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
        # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
        [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
        alias el='vi ~/projects/ohjaa-launch.zsh'\n";

    fn line() -> String {
        launch_line(Path::new("/Users/x/.cargo/bin/ohjaa"))
    }

    #[test]
    fn the_launcher_line_is_guarded_and_names_its_ohjaa() {
        assert_eq!(
            line(),
            "[[ -x '/Users/x/.cargo/bin/ohjaa' ]] && eval \"$('/Users/x/.cargo/bin/ohjaa' shell-init zsh)\""
        );
        assert_eq!(
            launch_target(&line()).as_deref(),
            Some(Path::new("/Users/x/.cargo/bin/ohjaa"))
        );
        assert_eq!(
            launch_target(BARE_LAUNCH_LINE).as_deref(),
            Some(Path::new("ohjaa"))
        );
        assert_eq!(launch_target("eval \"$(other shell-init zsh)\""), None);
    }

    #[test]
    fn only_the_line_that_sources_the_old_launcher_is_replaced_and_undo_puts_it_back() {
        let plan = zshrc_with_launcher(OLD, &line()).unwrap();
        assert_eq!(
            plan.contents,
            format!(
                "export PATH=\"$HOME/.local/bin:$PATH\"\n{LAUNCH_COMMENT}\n{}\nalias el='vi ~/projects/ohjaa-launch.zsh'\n",
                line()
            ),
            "the alias that names the file stays"
        );
        assert_eq!(plan.removed.len(), 2);
        assert!(!plan.blank);
        assert_eq!(
            zshrc_with_launcher(&plan.contents, &line()),
            None,
            "applied twice"
        );
        let undone = zshrc_without_launcher(&plan.contents, &plan.removed, plan.blank).unwrap();
        assert_eq!(undone.contents, OLD, "undo is exact");
        for sources in [
            "source ~/projects/ohjaa-launch.zsh",
            ". \"$HOME/projects/ohjaa-launch.zsh\"",
            "[[ -r ~/projects/ohjaa-launch.zsh ]] && . ~/projects/ohjaa-launch.zsh",
        ] {
            assert!(sources_old_launcher(sources), "{sources}");
        }
        for stays in [
            "# source ~/projects/ohjaa-launch.zsh",
            "alias e='vi ~/projects/ohjaa-launch.zsh'",
        ] {
            assert!(!sources_old_launcher(stays), "{stays}");
        }
    }

    #[test]
    fn a_launcher_line_for_another_ohjaa_or_the_bare_form_is_rewritten() {
        let earlier = format!("{LAUNCH_COMMENT}\n{BARE_LAUNCH_LINE}\n");
        let plan = zshrc_with_launcher(&earlier, &line()).unwrap();
        assert_eq!(plan.contents, format!("{LAUNCH_COMMENT}\n{}\n", line()));
        let moved = launch_line(Path::new("/opt/ohjaa"));
        let plan = zshrc_with_launcher(&plan.contents, &moved).unwrap();
        assert_eq!(plan.contents, format!("{LAUNCH_COMMENT}\n{moved}\n"));
    }

    #[test]
    fn without_an_old_line_the_launcher_is_appended_and_undo_takes_it_out() {
        let plan = zshrc_with_launcher("alias ll='ls -l'", &line()).unwrap();
        assert_eq!(
            plan.contents,
            format!("alias ll='ls -l'\n\n{LAUNCH_COMMENT}\n{}\n", line())
        );
        assert!(plan.removed.is_empty() && plan.blank);
        let undone = zshrc_without_launcher(&plan.contents, &[], true).unwrap();
        assert_eq!(undone.contents, "alias ll='ls -l'\n");
        let empty = zshrc_with_launcher("", &line()).unwrap();
        assert_eq!(empty.contents, format!("{LAUNCH_COMMENT}\n{}\n", line()));
        assert_eq!(
            zshrc_without_launcher(&empty.contents, &[], false)
                .unwrap()
                .contents,
            ""
        );
        assert_eq!(zshrc_without_launcher("alias x=y\n", &[], false), None);
    }

    #[test]
    fn the_ignore_block_is_added_once_replaced_in_place_and_taken_out() {
        let block = format!("{IGNORE_BEGIN}\n.sykli/\n.teko/\n{IGNORE_END}\n");
        assert_eq!(ignore_with_block("").unwrap(), (block.clone(), false));
        let mine = "*.swp\n.DS_Store\n";
        let (added, blank) = ignore_with_block(mine).unwrap();
        assert_eq!(added, format!("{mine}\n{block}"));
        assert!(blank);
        assert_eq!(ignore_with_block(&added).unwrap().0, added, "applied twice");
        assert_eq!(
            ignore_without_block(&added, true).unwrap().as_deref(),
            Some(mine)
        );
        let old = format!("*.swp\n{IGNORE_BEGIN}\n.old/\n{IGNORE_END}\n.DS_Store\n");
        assert_eq!(
            ignore_with_block(&old).unwrap().0,
            format!("*.swp\n{block}.DS_Store\n")
        );
        assert_eq!(
            ignore_without_block(&old, false).unwrap().as_deref(),
            Some("*.swp\n.DS_Store\n")
        );
        assert_eq!(ignore_without_block(mine, true).unwrap(), None);
        let open = format!("*.swp\n{IGNORE_BEGIN}\n.sykli/\n");
        assert!(ignore_with_block(&open)
            .unwrap_err()
            .contains("never closes"));
    }
}
