//! `ohjaa apply` and `ohjaa apply --undo`.
//!
//! `apply` changes these things outside the home, and only those: a marked
//! block in git's global excludes file, the terminal's line in `~/.zshrc`,
//! and the knowledge block in each agent's own global instruction file that
//! exists ([`crate::instructions`]). Each is recorded in `applied.toml`
//! before it is made (what the zshrc line replaced, whether a file, a blank
//! line or a final newline was there), so `--undo` puts back exactly what
//! was there, even after an apply that failed midway. What it changed is
//! then one commit in the home's history ([`home::commit`]); a failed commit
//! is said, and the change stands.
//!
//! Every change is planned before any is made: a refusal leaves nothing half
//! applied, and `--dry-run` prints the same plan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::flat_toml;
use crate::gitconfig;
use crate::home;

/// The change was refused.
const REFUSED: i32 = 3;

/// The launcher line: the ohjaa that ran `apply`, by its path, guarded like
/// the line it replaces, so a shell where that ohjaa is gone starts quietly.
pub fn launch_line(ohjaa: &Path) -> String {
    let quoted = home::quote_path(ohjaa);
    format!("[[ -x {quoted} ]] && eval \"$({quoted} shell-init zsh)\"")
}

/// The ohjaa a launcher line `apply` wrote runs; `None` for any other line.
pub fn launch_target(line: &str) -> Option<PathBuf> {
    let line = line.trim();
    let (path, _) = line.strip_prefix("[[ -x '")?.split_once("' ]] && eval ")?;
    let path = PathBuf::from(path.replace("'\\''", "'"));
    (launch_line(&path) == line).then_some(path)
}

pub const LAUNCH_COMMENT: &str =
    "# What needs your attention, as a terminal opens (written by `ohjaa apply`).";
/// A line that sources a launcher script of this name is replaced by apply's own.
const OLD_LAUNCHER: &str = "ohjaa-launch.zsh";

pub const IGNORE_BEGIN: &str =
    "# >>> ohjaa apply: workflow files no repository tracks (edits inside are replaced)";
const IGNORE_END: &str = "# <<< ohjaa apply";
/// What every repository ignores without being told: the workflow's own files.
/// `.sykli/` holds sykli's receipts. `.ohjaa/` holds accepted lessons that sit
/// in a checkout; untracked, they would count in sykli's tree digest, and no
/// receipt would match a commit.
pub const IGNORED: &[&str] = &[".sykli/", ".ohjaa/"];

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

/// A comment that names ohjaa: above apply's line, it is apply's own.
fn ohjaa_comment(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with('#') && line.to_lowercase().contains("ohjaa")
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
    let launch = lines.iter().position(|line| line.trim() == desired);
    let has_launch = launch.is_some();
    let mut drop = vec![false; lines.len()];
    for (at, line) in lines.iter().enumerate() {
        let stale_launch = launch_target(line).is_some() && line.trim() != desired;
        if sources_old_launcher(line) || stale_launch {
            drop[at] = true;
            if at > 0 && ohjaa_comment(lines[at - 1]) {
                drop[at - 1] = true;
            }
        }
    }
    // Apply's own comment above its line, in other words than these: said
    // anew in place, as apply's, never kept as a line it replaced.
    let reworded = launch
        .filter(|&at| at > 0 && ohjaa_comment(lines[at - 1]) && lines[at - 1] != LAUNCH_COMMENT)
        .map(|at| at - 1);
    if has_launch && !drop.contains(&true) && reworded.is_none() {
        return None;
    }
    let insert: Vec<String> = if has_launch {
        Vec::new()
    } else {
        vec![LAUNCH_COMMENT.to_owned(), desired.to_owned()]
    };
    let mut added = insert.clone();
    let mut out: Vec<String> = Vec::new();
    let mut removed = Vec::new();
    let mut placed = has_launch;
    for (at, line) in lines.iter().enumerate() {
        if drop[at] {
            removed.push((*line).to_owned());
            if !placed {
                out.extend(insert.iter().cloned());
                placed = true;
            }
        } else if reworded == Some(at) {
            removed.push((*line).to_owned());
            added.push(LAUNCH_COMMENT.to_owned());
            out.push(LAUNCH_COMMENT.to_owned());
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
        out.extend(insert.iter().cloned());
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
    if begin > 0 && ohjaa_comment(lines[begin - 1]) {
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
    /// `--root`: where the repositories are, to write down in the roots file.
    pub roots: &'a [PathBuf],
    /// The ohjaa the terminal's line runs, instead of the one on PATH.
    pub bin: Option<&'a Path>,
}

/// The ohjaa the terminal's line runs: `--bin`, else the `ohjaa` on PATH as
/// found (a link kept a link). A different binary running apply (a `cargo
/// run`, a `target/` build) would point the line at itself, so that is
/// refused unless `--bin` names it.
fn chosen_ohjaa(bin: Option<&Path>) -> std::result::Result<PathBuf, String> {
    if let Some(bin) = bin {
        let bin = std::path::absolute(bin).unwrap_or_else(|_| bin.to_path_buf());
        return if gitconfig::executable(&bin) {
            Ok(bin)
        } else {
            Err(format!("--bin {} is not an executable file", bin.display()))
        };
    }
    let Some(found) = home::on_path("ohjaa") else {
        return Err(
            "ohjaa is not on PATH, so the terminal's line would have no ohjaa to run; \
             install it, or name the binary with --bin <path>"
                .to_owned(),
        );
    };
    let running = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| format!("cannot tell which ohjaa is running: {error}"))?;
    if std::fs::canonicalize(&found).ok().as_deref() != Some(running.as_path()) {
        return Err(format!(
            "this ohjaa ({}) is not the one on PATH ({}); the terminal's line would run \
             it. Run the ohjaa on PATH, or name the binary with --bin <path>",
            running.display(),
            found.display()
        ));
    }
    Ok(found)
}

/// The record entries a step makes, kept before the step runs, so an apply
/// that fails midway can still be undone. The first value of an undo key is
/// the one undo needs.
fn keep_record(home: &Path, entries: &[(String, String)]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut record = read_record(home)?;
    for (key, value) in entries {
        record.entry(key.clone()).or_insert_with(|| value.clone());
    }
    write_record(home, &record)
}

/// The roots file: one directory per line, under a comment.
fn roots_text(roots: &[PathBuf]) -> String {
    let mut text = String::from(
        "# Where ohjaa looks for repositories: one directory per line. Written by\n\
         # `ohjaa apply --root`; edit it to change them.\n",
    );
    for root in roots {
        text.push_str(&format!("{}\n", root.display()));
    }
    text
}

/// Whether text that is not empty ends without a newline (undo keeps it so).
fn lacks_final_newline(text: &str) -> bool {
    !text.is_empty() && !text.ends_with('\n')
}

pub fn apply(options: Options<'_>) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let ohjaa = match chosen_ohjaa(options.bin) {
        Ok(ohjaa) => ohjaa,
        Err(why) => {
            eprintln!("ohjaa apply: refusing: {why}");
            return Ok(REFUSED);
        }
    };
    // Past the refusal: what apply plans, makes and commits is one change to
    // the home, never interleaved with a decision's. A dry run writes
    // nothing, the lock's file included.
    let lock = (!options.dry_run).then(|| home::lock(&home)).flatten();
    let global = gitconfig::GlobalFacts::read()?;
    let mut steps: Vec<Step> = Vec::new();

    // Where the repositories are, written down when named: collect and
    // lessons read it, wherever they run from. Kept by --undo.
    let roots_file = crate::roots::config_path().context("no config home")?;
    if !options.roots.is_empty() {
        let roots: Vec<PathBuf> = options
            .roots
            .iter()
            .map(|root| std::path::absolute(root).unwrap_or_else(|_| root.clone()))
            .collect();
        if crate::roots::parse_config(&read_or_empty(&roots_file)?) != roots {
            let said = roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let (target, text) = (roots_file.clone(), roots_text(&roots));
            steps.push(step(
                ("write", "wrote"),
                format!(
                    "{} with {said}, where ohjaa looks for repositories",
                    roots_file.display()
                ),
                Vec::new(),
                move || home::write_file(&target, &text, None),
            ));
        }
    } else if !roots_file.is_file() {
        eprintln!(
            "ohjaa apply: note: {} does not exist, so `ohjaa collect` and `ohjaa lessons` \
             look for repositories from where they run; name the folder that holds them \
             once: ohjaa apply --root <dir>",
            roots_file.display()
        );
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

    // Each agent's file apply could not write: the rest is applied, but
    // apply is not done while one stays unwritten, and every run says so.
    let (instruction, not_written) = instruction_steps(&read_record(&home)?)?;
    steps.extend(instruction);
    let except = (!not_written.is_empty()).then(|| {
        format!(
            "applied, except {} agent file{} not written: {}",
            not_written.len(),
            if not_written.len() == 1 { "" } else { "s" },
            not_written.join("; ")
        )
    });
    let code = if except.is_some() {
        crate::INCOMPLETE
    } else {
        0
    };

    if steps.is_empty() {
        match &except {
            None => println!(
                "ohjaa apply: nothing to change; the workflow is applied from {}",
                home.display()
            ),
            Some(except) => println!("ohjaa apply: nothing else to change; {except}"),
        }
        return Ok(code);
    }
    if options.dry_run {
        for step in &steps {
            println!("ohjaa apply: {}", step.will);
        }
        println!("ohjaa apply: dry run; nothing was changed");
        if let Some(except) = &except {
            println!("ohjaa apply: it would be {except}");
        }
        return Ok(code);
    }
    let mut done = Vec::new();
    for step in steps {
        // Recorded before it is made: a failure after it can still be undone.
        keep_record(&home, &step.record)?;
        (step.run)()?;
        println!("ohjaa apply: {}", step.did);
        done.push(step.did);
    }
    let mut message = history_message("apply", &done);
    if let Some(except) = &except {
        let (subject, body) = message.split_once('\n').unwrap_or((&message, ""));
        message = format!(
            "{subject}; {} agent file{} not written\n{body}\n{}",
            not_written.len(),
            if not_written.len() == 1 { "" } else { "s" },
            not_written
                .iter()
                .map(|file| format!("not written: {file}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        home::commit_and_say(&home, &message, "ohjaa apply", lock.as_ref());
        println!("ohjaa apply: {except}");
    } else {
        home::commit_and_say(&home, &message, "ohjaa apply", lock.as_ref());
    }
    Ok(code)
}

/// The commit that records what apply (or `--undo`) did, change by change.
fn history_message(what: &str, done: &[String]) -> String {
    format!(
        "{what}: {} change{}\n\n{}",
        done.len(),
        if done.len() == 1 { "" } else { "s" },
        done.join("\n")
    )
}

/// The record's slots for the agent `key`'s files, in the order they were
/// recorded: `instructions_<key>` for the first file apply wrote a block
/// into, then `instructions_<key>_2`, `_3`, … for each other one (the agent's
/// folder moved by its variable between applies). Each slot keeps its own
/// `_blank` and `_newline`.
fn instruction_slots(record: &Record, key: &str) -> Vec<(String, PathBuf)> {
    let first = format!("instructions_{key}");
    let mut slots: Vec<(u32, String, PathBuf)> = record
        .iter()
        .filter_map(|(slot, file)| {
            let n = if *slot == first {
                1
            } else {
                let n = slot.strip_prefix(&first)?.strip_prefix('_')?;
                if !n.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                n.parse().ok()?
            };
            Some((n, slot.clone(), PathBuf::from(file)))
        })
        .collect();
    slots.sort();
    slots
        .into_iter()
        .map(|(_, slot, file)| (slot, file))
        .collect()
}

/// The slot that records `file` for the agent `key`: the one that already
/// names it, by whatever name (a link to its folder, or the folder a
/// variable names: [`home::same_path`]), else the next free one.
fn instruction_slot(record: &Record, key: &str, file: &Path) -> String {
    let slots = instruction_slots(record, key);
    if let Some((slot, _)) = slots
        .iter()
        .find(|(_, recorded)| home::same_path(recorded, file))
    {
        return slot.clone();
    }
    let first = format!("instructions_{key}");
    (1..)
        .map(|n| {
            if n == 1 {
                first.clone()
            } else {
                format!("{first}_{n}")
            }
        })
        .find(|slot| !record.contains_key(slot))
        .unwrap_or(first)
}

/// The knowledge block in each agent's own instruction file: added, or
/// rewritten where it is, in each file that exists outside every repository.
/// Where an agent is set up but has no such file, or its file is inside a
/// repository, that is said, and nothing is written. Also each file apply
/// should write and cannot (`<file> (<agent>'s own instructions: <why>)`).
/// Every file a block is written into is recorded, each in a slot of its own
/// ([`instruction_slots`]), so undo takes each back wherever the agent's
/// folder is when it runs.
fn instruction_steps(record: &Record) -> Result<(Vec<Step>, Vec<String>)> {
    let (compact, broken) = crate::instructions::home_knowledge()?;
    for broken in &broken {
        eprintln!(
            "ohjaa apply: warning: unreadable knowledge {}: {}; it is not in the agents' block",
            broken.path.display(),
            broken.reason
        );
    }
    if compact.more > 0 || compact.text.len() > crate::knowledge::DEFAULT_BUDGET {
        eprintln!(
            "ohjaa apply: warning: the knowledge for agents' own files (the team's, mine and machine) is over the {} characters an \
             agent's own instruction file holds; the block holds {}",
            crate::knowledge::DEFAULT_BUDGET,
            compact.summary()
        );
    }
    let mut steps = Vec::new();
    let mut not_written = Vec::new();
    for target in crate::instructions::targets() {
        let Some(file) = target.file.clone() else {
            if target.installed {
                println!(
                    "ohjaa apply: {} reads {}, which does not exist; apply creates no \
                     instruction file (create it, then apply again, to give {} the knowledge too)",
                    target.agent,
                    target.usual.display(),
                    target.agent
                );
            }
            continue;
        };
        if let Some(root) = crate::instructions::inside_repository(&file) {
            println!(
                "ohjaa apply: {} ({}'s own instructions) is inside the repository {}; one \
                 person's knowledge never goes into a file a repository shares, so it gets no \
                 block",
                file.display(),
                target.agent,
                root.display()
            );
            continue;
        }
        // A file that cannot be read, or whose block cannot be told, is said
        // and skipped: one agent's file never stops the rest of apply.
        let planned = read_or_empty(&file).and_then(|now| {
            let held = crate::knowledge::held(&now)?;
            let (new, placed) = crate::knowledge::with_block(
                &now,
                crate::knowledge::INSTRUCTIONS_BEGIN,
                &compact.text,
            )?;
            Ok((now, held, new, placed))
        });
        let (now, held, new, placed) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                eprintln!(
                    "ohjaa apply: warning: not writing {} ({}'s own instructions): {error:#}; \
                     the rest is applied",
                    file.display(),
                    target.agent
                );
                not_written.push(format!(
                    "{} ({}'s own instructions: {error:#})",
                    file.display(),
                    target.agent
                ));
                continue;
            }
        };
        let (verb, what) = match held {
            crate::knowledge::Held::Other => {
                println!(
                    "ohjaa apply: {} holds a knowledge block apply did not write (`ohjaa brief \
                     --write`'s, or the person's): it is theirs, so apply leaves it and writes \
                     none there (take it out, then apply again, for apply's)",
                    file.display()
                );
                continue;
            }
            _ if new == now => continue,
            crate::knowledge::Held::Applied => (("rewrite", "rewrote"), "in"),
            crate::knowledge::Held::None => (("write", "wrote"), "into"),
        };
        let slot = instruction_slot(record, target.key, &file);
        let record = vec![
            (slot.clone(), file.to_string_lossy().into_owned()),
            (format!("{slot}_blank"), flag(placed.blank)),
            (format!("{slot}_newline"), flag(placed.newline)),
        ];
        let said = format!(
            "the knowledge block ({}) {what} {}, {}'s own instructions",
            compact.summary(),
            file.display(),
            target.agent
        );
        steps.push(step(verb, said, record, move || {
            home::write_file(&file, &new, None)
        }));
    }
    Ok((steps, not_written))
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
/// its record. The home's history stays.
pub fn undo(dry_run: bool) -> Result<i32> {
    let home = std::path::absolute(home::ohjaa_home()?)?;
    let lock = (!dry_run).then(|| home::lock(&home)).flatten();
    let record = read_record(&home)?;
    let recorded = |key: &str| record.get(key).cloned();
    let yes = |key: &str| record.get(key).is_some_and(|value| value == "true");
    // A key absent from the record says nothing was changed: keep as it is.
    let newline = |key: &str| record.get(key).is_none_or(|value| value == "true");
    let mut steps: Vec<Step> = Vec::new();
    let global = gitconfig::GlobalFacts::read()?;
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

    // The knowledge block apply wrote, from each agent's own file: each one
    // it wrote into (recorded, wherever the agent's folder was then), and the
    // one the agent reads now. A block apply did not write is the person's,
    // and stays. A recorded file, unreadable as a whole, refuses undo, as any
    // change it cannot take back exactly; any other is said and skipped.
    let mut files: Vec<(PathBuf, crate::knowledge::Placed, bool)> = Vec::new();
    for target in crate::instructions::targets() {
        // One file is taken back once, by the first name it was recorded
        // under (a record from before slots were matched by the file itself
        // may name it twice).
        let known = |files: &[(PathBuf, crate::knowledge::Placed, bool)], file: &Path| {
            files.iter().any(|(known, ..)| home::same_path(known, file))
        };
        for (slot, file) in instruction_slots(&record, target.key) {
            if known(&files, &file) {
                continue;
            }
            let placed = crate::knowledge::Placed {
                blank: yes(&format!("{slot}_blank")),
                newline: newline(&format!("{slot}_newline")),
            };
            files.push((file, placed, true));
        }
        if let Some(file) = target.file.filter(|file| !known(&files, file)) {
            files.push((file, crate::knowledge::Placed::IN_PLACE, false));
        }
    }
    for (file, placed, recorded) in files {
        let planned = read_or_empty(&file).and_then(|now| {
            Ok(match crate::knowledge::held(&now)? {
                crate::knowledge::Held::Applied => crate::knowledge::without_block(&now, placed)?,
                crate::knowledge::Held::Other => {
                    println!(
                        "ohjaa apply --undo: {}",
                        crate::instructions::left_theirs(&file)
                    );
                    None
                }
                crate::knowledge::Held::None => None,
            })
        });
        let without = match planned {
            Ok(without) => without,
            Err(error) if recorded => {
                return Err(error).with_context(|| format!("not changing {}", file.display()))
            }
            Err(error) => {
                eprintln!(
                    "ohjaa apply --undo: warning: not changing {}: {error:#}; the rest is undone",
                    file.display()
                );
                None
            }
        };
        if let Some(without) = without {
            steps.push(step(
                ("remove", "removed"),
                format!("the knowledge block from {}", file.display()),
                Vec::new(),
                move || home::write_file(&file, &without, None),
            ));
        }
    }

    // The record goes last: an undo that fails before it can run again.
    // What it holds that this undo does not take back stays in it, never
    // dropped unread, and undo says it is not done.
    let kept: Record = record
        .iter()
        .filter(|(key, _)| !taken_back(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let record_file = record_path(&home);
    let note = (!kept.is_empty()).then(|| {
        format!(
            "not all undone: {} keeps what this ohjaa does not take back ({})",
            record_file.display(),
            kept.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    });
    if record_file.exists() && kept.is_empty() {
        steps.push(step(
            ("remove", "removed"),
            record_file.display().to_string(),
            Vec::new(),
            move || remove_if_present(&record_file),
        ));
    } else if kept.len() < record.len() {
        let target = home.clone();
        steps.push(step(
            ("write", "wrote"),
            format!(
                "{} with only what this ohjaa does not take back",
                record_file.display()
            ),
            Vec::new(),
            move || write_record(&target, &kept),
        ));
    }
    let code = if note.is_some() { crate::INCOMPLETE } else { 0 };

    if steps.is_empty() {
        println!("ohjaa apply --undo: nothing to undo");
        if let Some(note) = &note {
            println!("ohjaa apply --undo: {note}");
        }
        return Ok(code);
    }
    let mut done = Vec::new();
    for step in steps {
        if dry_run {
            println!("ohjaa apply --undo: {}", step.will);
            continue;
        }
        (step.run)()?;
        println!("ohjaa apply --undo: {}", step.did);
        done.push(step.did);
    }
    if dry_run {
        println!("ohjaa apply --undo: dry run; nothing was changed");
    } else {
        println!("ohjaa apply --undo: kept the home's history");
        home::commit_and_say(
            &home,
            &history_message("apply --undo", &done),
            "ohjaa apply --undo",
            lock.as_ref(),
        );
    }
    if let Some(note) = &note {
        println!("ohjaa apply --undo: {note}");
    }
    Ok(code)
}

/// Whether undo takes back what `key` records: the ignore block, the
/// terminal's line and the agents' knowledge blocks, each as apply wrote
/// them.
fn taken_back(key: &str) -> bool {
    key.starts_with("ignore_")
        || key == "zshrc"
        || key.starts_with("zshrc_")
        || key.starts_with("instructions_")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
        # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
        [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
        alias el='vi ~/projects/ohjaa-launch.zsh'\n";

    fn line() -> String {
        launch_line(Path::new("/home/dev/.cargo/bin/ohjaa"))
    }

    #[test]
    fn the_launcher_line_is_guarded_and_names_its_ohjaa() {
        assert_eq!(
            line(),
            "[[ -x '/home/dev/.cargo/bin/ohjaa' ]] && eval \"$('/home/dev/.cargo/bin/ohjaa' shell-init zsh)\""
        );
        assert_eq!(
            launch_target(&line()).as_deref(),
            Some(Path::new("/home/dev/.cargo/bin/ohjaa"))
        );
        assert_eq!(launch_target("eval \"$(ohjaa shell-init zsh)\""), None);
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
    fn a_launcher_line_for_another_ohjaa_is_rewritten() {
        let earlier = format!("{LAUNCH_COMMENT}\n{}\n", line());
        let moved = launch_line(Path::new("/opt/ohjaa"));
        let plan = zshrc_with_launcher(&earlier, &moved).unwrap();
        assert_eq!(plan.contents, format!("{LAUNCH_COMMENT}\n{moved}\n"));
        assert_eq!(plan.removed, [LAUNCH_COMMENT.to_owned(), line()]);
        assert_eq!(zshrc_with_launcher(&plan.contents, &moved), None);
    }

    /// Apply's comment above its line, in the words an earlier apply wrote:
    /// apply says it anew in place, and undo takes it out with the line.
    #[test]
    fn an_ohjaa_comment_above_the_line_is_applys_own() {
        let earlier = format!(
            "alias ll='ls -l'\n# Route claude/codex through ohjaa (written by `ohjaa apply`).\n{}\n",
            line()
        );
        let plan = zshrc_with_launcher(&earlier, &line()).unwrap();
        assert_eq!(
            plan.contents,
            format!("alias ll='ls -l'\n{LAUNCH_COMMENT}\n{}\n", line())
        );
        assert_eq!(plan.added, [LAUNCH_COMMENT.to_owned()]);
        assert_eq!(zshrc_with_launcher(&plan.contents, &line()), None);
        let undone = zshrc_without_launcher(&earlier, &[], false).unwrap();
        assert_eq!(undone.contents, "alias ll='ls -l'\n");
        // A comment of the person's own, above it, that names no ohjaa stays.
        let theirs = format!("# my shell\n{}\n", line());
        assert_eq!(
            zshrc_without_launcher(&theirs, &[], false)
                .unwrap()
                .contents,
            "# my shell\n"
        );
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
        let block = format!("{IGNORE_BEGIN}\n.sykli/\n.ohjaa/\n{IGNORE_END}\n");
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
