//! `ohjaa apply` and `ohjaa apply --undo`.
//!
//! `apply` writes the home's generated files (`hooks/`, `git/config`,
//! `git/gated.config`) and changes three things outside it, and only those:
//! one `include.path` in git's global config, a marked block in git's global
//! excludes file, and the launcher line in `~/.zshrc`. Each of the three is
//! recorded in `applied.toml` as it is made (what the zshrc line replaced,
//! whether a file or a blank line was added), so `--undo` puts back exactly
//! what was there. `--tools` also installs `tools.lock`'s revisions.
//!
//! Every change is planned before any is made: a refusal leaves nothing half
//! applied, and `--dry-run` prints the same plan.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::flat_toml;
use crate::gate;
use crate::home;
use crate::hook::{dispatcher, dispatcher_target, invoked_ohjaa, quote_path, HOOK_NAMES};
use crate::tools::{self, Pin};

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
pub const IGNORED: &[&str] = &[".sykli/"];

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

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
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

pub struct Options {
    pub dry_run: bool,
    pub force: bool,
    pub tools: bool,
}

pub fn apply(options: Options, roots: &[PathBuf]) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let hooks = home::hooks_dir(&home);
    let version = gate::require_git()?;
    // An invalid generated file breaks every git command, the ones apply
    // runs next among them: put it right first.
    if !options.dry_run {
        for path in [gate::include_file(&home), gate::gated_file(&home)] {
            if path.is_file() && gate::check_git_config(&path).is_err() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("cannot remove {}", path.display()))?;
                println!(
                    "ohjaa apply: removed {}, which git could not read; it is written again below",
                    path.display()
                );
            }
        }
    }
    let mut steps: Vec<Step> = Vec::new();

    // A global core.hooksPath runs in every repository and overrides the one
    // ohjaa scopes to gated ones: refused unless it is ohjaa's own (from
    // before hooks were scoped) or --force says to remove it.
    // Read with includes followed: a file the global config includes counts.
    if let Some(path) = home::global_path_setting("core.hooksPath")?.filter(|v| !v.is_empty()) {
        let ours = home::same_path(Path::new(&path), &hooks);
        if !ours && !options.force {
            eprintln!(
                "ohjaa apply: refusing: git's global core.hooksPath is {path}. It runs in every \
                 repository and overrides the core.hooksPath ohjaa gives the gated ones, so \
                 the gate would not run. Move what it does into the repositories' own hooks \
                 (ohjaa's dispatchers chain to them), or remove it with: ohjaa apply --force"
            );
            return Ok(REFUSED);
        }
        if !home::global_sets_directly("core.hooksPath")? {
            let origin = home::global_origin("core.hooksPath")?.unwrap_or_default();
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

    // Dispatchers: pre-push, and each hook a gated clone has of its own.
    let gated = gate::gated_clones(&home, roots)?;
    let needed = gate::needed_hooks(&gated);
    let ohjaa = invoked_ohjaa()?;
    let mut write = Vec::new();
    let mut remove = Vec::new();
    for name in HOOK_NAMES {
        let path = hooks.join(name);
        let current = std::fs::read_to_string(&path).ok();
        if needed.contains(name) {
            if current.as_deref() != Some(dispatcher(name, &ohjaa).as_str())
                || !gate::executable(&path)
            {
                write.push(name);
            }
        } else if current.is_some_and(|text| dispatcher_target(name, &text).is_some()) {
            remove.push(name);
        }
    }
    if !write.is_empty() {
        let (dir, target, names) = (hooks.clone(), ohjaa.clone(), write.clone());
        steps.insert(
            0,
            step(
                ("write", "wrote"),
                format!(
                    "{} hook dispatcher{} into {} (exec {}): {}",
                    write.len(),
                    if write.len() == 1 { "" } else { "s" },
                    hooks.display(),
                    target.display(),
                    write.join(", ")
                ),
                Vec::new(),
                move || {
                    for name in names {
                        home::write_file(&dir.join(name), &dispatcher(name, &target), Some(0o755))?;
                    }
                    Ok(())
                },
            ),
        );
    }
    if !remove.is_empty() {
        let (dir, names) = (hooks.clone(), remove.clone());
        steps.push(step(
            ("remove", "removed"),
            format!(
                "dispatchers no gated clone needs from {}: {}",
                hooks.display(),
                remove.join(", ")
            ),
            Vec::new(),
            move || {
                names
                    .iter()
                    .try_for_each(|name| remove_if_present(&dir.join(name)))
            },
        ));
    }

    // The generated git config: one includeIf per entry remote pattern.
    for (path, text) in gate::generated(&home)? {
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
    let include = gate::include_file(&home).to_string_lossy().into_owned();
    let included = home::global_paths("include.path")?
        .iter()
        .any(|path| home::same_path(Path::new(path), Path::new(&include)));
    if !included {
        let value = include.clone();
        steps.push(step(
            ("add", "added"),
            format!(
                "include.path = {include} to git's global config (git {}.{}: repositories \
                 whose remote names an entry run ohjaa's hooks, no other does)",
                version.0, version.1
            ),
            vec![("include".to_owned(), include.clone())],
            move || home::add_global("include.path", &value),
        ));
    }

    let ignore = home::global_ignore_file()?;
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
    if let Some(plan) = zshrc_with_launcher(&read_or_empty(&zshrc)?, &launch_line(&ohjaa)) {
        let record = vec![
            ("zshrc".to_owned(), zshrc.to_string_lossy().into_owned()),
            ("zshrc_created".to_owned(), flag(!zshrc.exists())),
            ("zshrc_replaced".to_owned(), plan.removed.join("\n")),
            ("zshrc_blank".to_owned(), flag(plan.blank)),
        ];
        let (target, contents) = (zshrc.clone(), plan.contents.clone());
        steps.push(step(
            ("change", "changed"),
            format!("{}:{}", zshrc.display(), zshrc_lines(&plan)),
            record,
            move || home::write_file(&target, &contents, None),
        ));
    }

    let mut installs: Vec<(Pin, String)> = Vec::new();
    if options.tools {
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
                installs.push((pin, why));
            }
        }
    }

    if steps.is_empty() && installs.is_empty() {
        println!(
            "ohjaa apply: nothing to change; the workflow is applied from {}",
            home.display()
        );
        return Ok(0);
    }
    let install_line = |pin: &Pin, why: &str| {
        format!(
            "{} at {} ({why}): {}",
            pin.name,
            short(&pin.rev),
            crate::render_command(&tools::install_command(pin))
        )
    };
    if options.dry_run {
        for step in &steps {
            println!("ohjaa apply: {}", step.will);
        }
        for (pin, why) in &installs {
            println!("ohjaa apply: would install {}", install_line(pin, why));
        }
        println!("ohjaa apply: dry run; nothing was changed");
        return Ok(0);
    }
    let mut record = read_record(&home)?;
    let mut changed_outside = false;
    for step in steps {
        (step.run)()?;
        println!("ohjaa apply: {}", step.did);
        changed_outside |= !step.record.is_empty();
        for (key, value) in step.record {
            // The first apply's record is the one undo needs.
            record.entry(key).or_insert(value);
        }
    }
    if changed_outside {
        write_record(&home, &record)?;
    }
    let mut failed = Vec::new();
    for (pin, why) in &installs {
        println!("ohjaa apply: installing {}", install_line(pin, why));
        if let Err(error) = install(pin) {
            eprintln!("ohjaa apply: {} was not installed: {error:#}", pin.name);
            failed.push(pin.name.clone());
        }
    }
    if !failed.is_empty() {
        eprintln!("ohjaa apply: not installed: {}", failed.join(", "));
        return Ok(1);
    }
    Ok(0)
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

/// `ohjaa apply --undo`: take back exactly what apply changed, and remove
/// what it generated. `repos/` and `tools.lock` stay.
pub fn undo(dry_run: bool) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let record = read_record(&home)?;
    let recorded = |key: &str| record.get(key).cloned();
    let yes = |key: &str| record.get(key).is_some_and(|value| value == "true");
    let mut steps: Vec<Step> = Vec::new();

    let include = recorded("include")
        .unwrap_or_else(|| gate::include_file(&home).to_string_lossy().into_owned());
    if home::global_paths("include.path")?
        .iter()
        .any(|path| home::same_path(Path::new(path), Path::new(&include)))
    {
        let value = include.clone();
        steps.push(step(
            ("remove", "removed"),
            format!("include.path = {include} from git's global config"),
            Vec::new(),
            move || home::unset_global("include.path", Some(&value)),
        ));
    }
    if let Some(path) = recorded("hooks_path_removed") {
        if home::global_path_setting("core.hooksPath")?.is_none() {
            steps.push(step(
                ("restore", "restored"),
                format!("git's global core.hooksPath = {path}, which apply --force removed"),
                Vec::new(),
                move || home::set_global("core.hooksPath", &path),
            ));
        }
    }

    let ignore = recorded("ignore_file")
        .map(PathBuf::from)
        .map_or_else(home::global_ignore_file, Ok)?;
    let ignore_now = read_or_empty(&ignore)?;
    if let Some(without) = ignore_without_block(&ignore_now, yes("ignore_blank"))
        .map_err(|why| anyhow::anyhow!("{}: {why}", ignore.display()))?
    {
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
        let remove_empty = yes("zshrc_created");
        let (target, contents) = (zshrc.clone(), plan.contents.clone());
        steps.push(step(
            ("change", "changed"),
            format!("{}:{}", zshrc.display(), zshrc_lines(&plan)),
            Vec::new(),
            move || write_or_remove(&target, &contents, remove_empty),
        ));
    }

    // What apply generated: dispatchers (only files that still are one), the
    // git files, the record; then the folders, if nothing else is in them.
    let hooks = home::hooks_dir(&home);
    let dispatchers: Vec<&str> = HOOK_NAMES
        .into_iter()
        .filter(|name| {
            std::fs::read_to_string(hooks.join(name))
                .is_ok_and(|text| dispatcher_target(name, &text).is_some())
        })
        .collect();
    let mut generated: Vec<PathBuf> = dispatchers.iter().map(|name| hooks.join(name)).collect();
    generated.extend(
        [
            gate::include_file(&home),
            gate::gated_file(&home),
            record_path(&home),
        ]
        .into_iter()
        .filter(|path| path.exists()),
    );
    if !generated.is_empty() {
        let folders = [hooks.clone(), gate::git_dir(&home)];
        steps.push(step(
            ("remove", "removed"),
            format!(
                "what apply generated in {}: {}",
                home.display(),
                generated
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&home)
                            .unwrap_or(path)
                            .display()
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Vec::new(),
            move || {
                generated
                    .iter()
                    .try_for_each(|path| remove_if_present(path))?;
                for folder in folders {
                    if let Err(error) = std::fs::remove_dir(&folder) {
                        if folder.exists() {
                            eprintln!("ohjaa apply --undo: kept {}: {error}", folder.display());
                        }
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
            "ohjaa apply --undo: kept {} and {}",
            home.join("repos").display(),
            home::tools_lock(&home).display()
        );
    }
    Ok(0)
}

/// For `adopt`: the generated git files brought up to date, and a dispatcher
/// for each of `own` that lacks one (execing the ohjaa the existing pre-push
/// dispatcher execs). Says what it changed.
pub fn regenerate(home: &Path, own: &[&'static str]) -> Result<Vec<String>> {
    let mut said = Vec::new();
    for (path, text) in gate::generated(home)? {
        if read_or_empty(&path)? != text {
            gate::write_generated(&path, &text)?;
            said.push(format!("wrote {}", path.display()));
        }
    }
    let hooks = home::hooks_dir(home);
    let Some(ohjaa) = std::fs::read_to_string(hooks.join("pre-push"))
        .ok()
        .and_then(|text| dispatcher_target("pre-push", &text))
    else {
        return Ok(said);
    };
    let missing: BTreeSet<&str> = own
        .iter()
        .copied()
        .filter(|name| !hooks.join(name).is_file())
        .collect();
    for name in &missing {
        home::write_file(&hooks.join(name), &dispatcher(name, &ohjaa), Some(0o755))?;
    }
    if !missing.is_empty() {
        said.push(format!(
            "wrote dispatchers for its own hooks into {}: {}",
            hooks.display(),
            missing.into_iter().collect::<Vec<_>>().join(", ")
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
        let block = format!("{IGNORE_BEGIN}\n.sykli/\n{IGNORE_END}\n");
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
