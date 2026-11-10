//! Whether an agent reads the repository's own
//! AGENTS.md, where its lessons are ([`crate::lessons`]): the briefing leaves
//! the lessons to that file only for an agent shown to read it, the rule
//! [`crate::instructions::source`] keeps for mine and machine. Anything
//! ohjaa cannot show carries them. Files only, no git and no agent run:
//!
//! - **Claude Code 2.1.311** loads AGENTS.md through its built-in plugin
//!   `cc-plugin-agents-md`, whose option `instructionFiles` (in
//!   `pluginConfigs` of the user's or the organisation's settings; project
//!   settings are not read, by the plugin's own words) is
//!   `claude-md-or-agents-md` by default: AGENTS.md only where the project
//!   has no CLAUDE.md, `.claude/CLAUDE.md` or `CLAUDE.local.md` at the
//!   session's root or above it. ohjaa does not rely on that fallback (one
//!   CLAUDE.md above every checkout defeats it): left to AGENTS.md only when
//!   a CLAUDE.md, `.claude/CLAUDE.md` or `CLAUDE.local.md` of the root, or
//!   of a folder between it and the launch folder, imports the root's
//!   AGENTS.md (`@AGENTS.md` in its text: not in frontmatter, fenced or
//!   indented code, code spans, HTML comments or HTML blocks, which its
//!   extractor skips), and Claude Code starts at the root or the project
//!   approved external imports (its loader follows an import outside the
//!   folder it starts in only then: `hasClaudeMdExternalIncludesApproved` of
//!   the project, keyed by its git root, in `~/.claude.json`); or
//!   `instructionFiles` is `claude-md-and-agents-md` (the legacy
//!   `projectInstructions = "both"` too). Carried with `--bare`,
//!   `--safe-mode`, `CLAUDE_CODE_SIMPLE` or `CLAUDE_CODE_SAFE_MODE` truthy,
//!   `CLAUDE_CODE_DISABLE_CLAUDE_MDS` with any value, `--setting-sources`
//!   without `project`, `--settings` (not read here), `claudeMdExcludes` set
//!   in any settings (its patterns not matched here), the plugin turned off
//!   in `enabledPlugins`, or `instructionFiles` `managed-only`
//!   (`CLAUDE_CODE_MANAGED_SETTINGS_PATH` moves the organisation's settings).
//! - **Codex 0.164.0** reads AGENTS.md from the git root down to the launch
//!   folder, an `AGENTS.override.md` in its place where there is one, at
//!   most `project_doc_max_bytes` (32768 by default) in all, the root's
//!   first: carried with an override at the root, or when a lesson's line
//!   ends past that many bytes of the root's AGENTS.md.
//! - **pi 1.2.0** reads the working folder's and its parents' context files
//!   (its `docs/configuration.md`), an `AGENTS.override.md` in AGENTS.md's
//!   place: carried with one at the root, or `-nc`/`--no-context-files`.
//! - **opencode 2.1.4** walks up from the launch folder for AGENTS.md:
//!   carried with `OPENCODE_DISABLE_PROJECT_CONFIG` or
//!   `OPENCODE_CONFIG_PROJECT_DISABLE` set, or `--server` (a server ohjaa
//!   cannot see reads them).
//! - Any other command, and `ohjaa brief` without `--agent`: carried.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Whether the agent reads the repository's AGENTS.md.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    /// It does, by `how`: the briefing leaves the lessons there to it.
    Yes { agent: &'static str, how: String },
    /// It does not, or ohjaa cannot tell, because of `why`: the briefing
    /// carries them.
    No { why: String },
}

/// Codex's `project_doc_max_bytes` when nothing sets it (Codex 0.164.0).
pub const CODEX_PROJECT_DOC_MAX_BYTES: usize = 32_768;

/// For `agent` (its command and what follows it, or `None` for `ohjaa brief`
/// without `--agent`), launched in `launch` inside
/// the repository at `root`, whether it reads `root`'s AGENTS.md as far as
/// `end` (the byte where the last lesson's line ends in it).
pub fn reads(agent: Option<(&str, &[String])>, root: &Path, launch: &Path, end: usize) -> Reads {
    reads_with(agent, root, launch, end, &|name| std::env::var_os(name))
}

fn reads_with(
    agent: Option<(&str, &[String])>,
    root: &Path,
    launch: &Path,
    end: usize,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Reads {
    let no = |why: String| Reads::No { why };
    let Some((command, args)) = agent else {
        return no(
            "no agent is named, so ohjaa cannot tell whether it reads AGENTS.md \
                   (`ohjaa brief --agent <command>` says for one)"
                .to_owned(),
        );
    };
    let key = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    let options: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
        .collect();
    let (agent, result) = match key {
        "claude" => ("Claude Code", claude(&options, root, launch, var)),
        "codex" => ("Codex", codex(&options, root, end, var)),
        "pi" => ("pi", pi(&options, root)),
        "opencode" => ("opencode", opencode(&options, var)),
        _ => {
            return no(format!(
                "ohjaa does not know whether {command} reads AGENTS.md"
            ))
        }
    };
    match result {
        Ok(how) => Reads::Yes { agent, how },
        Err(why) => no(why),
    }
}

/// An option's value among `options`, written `--name value` or
/// `--name=value`, and how it was written.
fn valued(options: &[&str], at: usize, name: &str) -> Option<(String, String)> {
    match options[at].strip_prefix(name)? {
        "" => options
            .get(at + 1)
            .map(|value| (format!("{name} {value}"), (*value).to_owned())),
        joined => joined
            .strip_prefix('=')
            .map(|value| (format!("{name}={value}"), value.to_owned())),
    }
}

fn truthy(value: &OsString) -> bool {
    matches!(
        value.to_string_lossy().trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn claude(
    options: &[&str],
    root: &Path,
    launch: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<String, String> {
    let mut user_settings = true;
    for (at, option) in options.iter().enumerate() {
        if matches!(*option, "--bare" | "--safe-mode") {
            return Err(format!(
                "`{option}` is passed to Claude Code, which then loads no project instructions"
            ));
        }
        if let Some((written, value)) = valued(options, at, "--setting-sources") {
            let sources: Vec<&str> = value.split(',').map(str::trim).collect();
            if !sources.contains(&"project") {
                return Err(format!(
                    "`{written}` leaves out `project`, so Claude Code loads no CLAUDE.md of the \
                     repository's"
                ));
            }
            user_settings = sources.contains(&"user");
        }
        if let Some((written, _)) = valued(options, at, "--settings") {
            return Err(format!(
                "`{written}` is passed to Claude Code, and ohjaa does not read what those \
                 settings say of AGENTS.md"
            ));
        }
    }
    for name in ["CLAUDE_CODE_SIMPLE", "CLAUDE_CODE_SAFE_MODE"] {
        if let Some(value) = var(name).filter(truthy) {
            return Err(format!(
                "{name}={} is in the environment, so Claude Code loads no project instructions",
                value.to_string_lossy()
            ));
        }
    }
    // Any value turns every CLAUDE.md off (Claude Code sets it itself under
    // `--safe-mode`).
    if let Some(value) = var("CLAUDE_CODE_DISABLE_CLAUDE_MDS").filter(|value| !value.is_empty()) {
        return Err(format!(
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS={} is in the environment, so Claude Code loads no \
             CLAUDE.md",
            value.to_string_lossy()
        ));
    }
    if let Some(file) = md_excludes(root, user_settings, var)? {
        return Err(format!(
            "{} sets claudeMdExcludes, and ohjaa does not match its patterns against the \
             CLAUDE.md files",
            file.display()
        ));
    }
    match plugin_mode(user_settings, var)? {
        Mode::ManagedOnly(file) => Err(format!(
            "Claude Code's agents-md plugin is set to managed-only in {}, so no project \
             instructions load",
            file.display()
        )),
        Mode::Beside(file) => Ok(format!(
            "its agents-md plugin loads AGENTS.md beside CLAUDE.md, as {} sets",
            file.display()
        )),
        Mode::Otherwise => imports_agents_md(root, launch, var)
            .map(|file| format!("{} imports it", file.display())),
    }
}

/// What Claude Code's agents-md plugin is set to, as far as it matters here.
enum Mode {
    ManagedOnly(PathBuf),
    Beside(PathBuf),
    Otherwise,
}

const PLUGIN: &str = "cc-plugin-agents-md";

/// Where the organisation's settings are when nothing moves them.
const MANAGED_SETTINGS: &str = if cfg!(target_os = "macos") {
    "/Library/Application Support/ClaudeCode/managed-settings.json"
} else {
    "/etc/claude-code/managed-settings.json"
};

/// The organisation's settings: `CLAUDE_CODE_MANAGED_SETTINGS_PATH` moves them.
fn managed_settings(var: &dyn Fn(&str) -> Option<OsString>) -> PathBuf {
    var("CLAUDE_CODE_MANAGED_SETTINGS_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(MANAGED_SETTINGS))
}

/// Claude Code's own folder: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn claude_config(var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            var("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".claude"))
        })
}

/// The settings file that sets `claudeMdExcludes` (patterns of CLAUDE.md
/// files Claude Code leaves out), among the user's (unless left out), the
/// project's, its local ones and the organisation's. ohjaa does not match
/// those patterns, so any such setting carries the lessons.
fn md_excludes(
    root: &Path,
    user: bool,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    if user {
        files.extend(claude_config(var).map(|dir| dir.join("settings.json")));
    }
    files.push(root.join(".claude").join("settings.json"));
    files.push(root.join(".claude").join("settings.local.json"));
    files.push(managed_settings(var));
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let settings: Value = serde_json::from_str(&text).map_err(|error| {
            format!(
                "{} cannot be read as JSON ({error}), so what it says of CLAUDE.md cannot be told",
                file.display()
            )
        })?;
        let set = match &settings["claudeMdExcludes"] {
            Value::Null => false,
            Value::Array(patterns) => !patterns.is_empty(),
            _ => true,
        };
        if set {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

/// Whether Claude Code may follow an import outside the folder it starts in,
/// for the project at `root`: `projects[<root>].hasClaudeMdExternalIncludesApproved`
/// in its global config (`$CLAUDE_CONFIG_DIR/.claude.json`, else
/// `~/.claude.json`), the project keyed by its git root (Claude Code
/// 2.1.311's `lPe`). The file, and whether it says so, or why not.
fn external_imports_approved(
    root: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> (PathBuf, bool) {
    let file = var("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join(".claude.json"))
        .or_else(|| {
            var("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".claude.json"))
        })
        .unwrap_or_else(|| PathBuf::from(".claude.json"));
    let approved = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|config| {
            let real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            [root.to_path_buf(), real].iter().any(|key| {
                config["projects"][key.to_string_lossy().as_ref()]
                    ["hasClaudeMdExternalIncludesApproved"]
                    .as_bool()
                    == Some(true)
            })
        });
    (file, approved)
}

/// The plugin's `instructionFiles` from the user's settings (unless
/// `--setting-sources` leaves `user` out) and the organisation's (which
/// come after, and win), and whether the plugin is turned off. A settings
/// file that cannot be read is an error: what it says cannot be told.
fn plugin_mode(user: bool, var: &dyn Fn(&str) -> Option<OsString>) -> Result<Mode, String> {
    let config = claude_config(var);
    let mut files: Vec<PathBuf> = Vec::new();
    if user {
        files.extend(config.map(|dir| dir.join("settings.json")));
    }
    files.push(managed_settings(var));
    let mut mode = Mode::Otherwise;
    let mut off = false;
    for file in files {
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "{} cannot be read ({error}), so what it says of AGENTS.md cannot be told",
                    file.display()
                ))
            }
        };
        let settings: Value = serde_json::from_str(&text).map_err(|error| {
            format!(
                "{} cannot be read as JSON ({error}), so what it says of AGENTS.md cannot be told",
                file.display()
            )
        })?;
        let ours = |key: &str| key.split('@').next() == Some(PLUGIN);
        if let Some(enabled) = settings["enabledPlugins"].as_object() {
            for (key, value) in enabled {
                if ours(key) {
                    off = value.as_bool() == Some(false);
                }
            }
        }
        if let Some(configs) = settings["pluginConfigs"].as_object() {
            for (key, config) in configs {
                if !ours(key) {
                    continue;
                }
                let options = &config["options"];
                let legacy = options["projectInstructions"]
                    .as_str()
                    .map(|value| match value {
                        "none" => "managed-only",
                        "claude" => "claude-md",
                        "agents-fallback" => "claude-md-or-agents-md",
                        "both" => "claude-md-and-agents-md",
                        _ => "",
                    });
                let chosen = options["instructionFiles"]
                    .as_str()
                    .filter(|value| *value != "claude-md-or-agents-md")
                    .or(legacy)
                    .unwrap_or("");
                mode = match chosen {
                    "managed-only" => Mode::ManagedOnly(file.clone()),
                    "claude-md-and-agents-md" => Mode::Beside(file.clone()),
                    _ => Mode::Otherwise,
                };
            }
        }
    }
    Ok(match mode {
        Mode::Beside(_) if off => Mode::Otherwise,
        mode => mode,
    })
}

/// The CLAUDE.md that imports `root`'s AGENTS.md: `CLAUDE.md`,
/// `.claude/CLAUDE.md` or `CLAUDE.local.md` of the root, or of a folder
/// between it and `launch` (each loaded where Claude Code starts there),
/// with an `@` import outside fenced blocks and code spans that names it,
/// relative to the file, `~/`, or absolute. Else why not.
fn imports_agents_md(
    root: &Path,
    launch: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, String> {
    // The root's AGENTS.md, named by its folder and its name: a CLAUDE.md
    // may import it before the file exists (a dry run, before the merge).
    let root_real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let is_agents_md = |path: &Path| {
        path.file_name().is_some_and(|name| name == "AGENTS.md")
            && path
                .parent()
                .and_then(|folder| std::fs::canonicalize(folder).ok())
                .is_some_and(|folder| folder == root_real)
    };
    let home = var("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let launch = std::fs::canonicalize(launch).unwrap_or_else(|_| launch.to_path_buf());
    let mut folders: Vec<PathBuf> = launch
        .ancestors()
        .take_while(|folder| folder.starts_with(&root_real))
        .map(Path::to_path_buf)
        .collect();
    if folders.is_empty() {
        folders.push(root_real.clone());
    }
    let mut seen: Vec<String> = Vec::new();
    for folder in folders {
        for name in ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"] {
            let file = folder.join(name);
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            seen.push(file.display().to_string());
            let base = file.parent().unwrap_or(&folder);
            let names_it = imports(&text).into_iter().any(|path| {
                let resolved = match (path.strip_prefix("~/"), &home) {
                    (Some(rest), Some(home)) => home.join(rest),
                    _ => base.join(&path),
                };
                is_agents_md(&resolved)
            });
            if names_it {
                // Claude Code 2.1.311 follows an import whose file is outside
                // the folder it starts in only when the project approved
                // external imports (its loader: `if(!j0(We)&&!V)continue`).
                // The root's AGENTS.md is inside that folder only when it
                // starts at the root.
                if launch != root_real {
                    let (config, approved) = external_imports_approved(root, var);
                    if !approved {
                        return Err(format!(
                            "Claude Code starts in {}, and follows {}'s import of {}, outside \
                             that folder, only when the project's \
                             hasClaudeMdExternalIncludesApproved is true in {}, which it is not",
                            launch.display(),
                            file.display(),
                            root.join("AGENTS.md").display(),
                            config.display()
                        ));
                    }
                }
                return Ok(file);
            }
        }
    }
    Err(if seen.is_empty() {
        format!(
            "{} has no CLAUDE.md, and Claude Code then reads AGENTS.md only where no CLAUDE.md is \
             found above it either, which ohjaa does not rely on (an `@AGENTS.md` line in a \
             CLAUDE.md makes it read)",
            root.display()
        )
    } else {
        format!(
            "{} does not import AGENTS.md (an `@AGENTS.md` line), and Claude Code reads it then \
             only where no CLAUDE.md is found",
            seen.join(", ")
        )
    })
}

/// The paths a CLAUDE.md imports, as Claude Code 2.1.311's extractor finds
/// them (it walks Markdown's tokens: code and code spans skipped, HTML
/// comments stripped, other HTML blocks skipped, frontmatter dropped): each
/// word that begins with `@` in its text, and nowhere else. Indented code
/// is skipped, though a paragraph's indented line is text to Markdown: what
/// is not counted only carries the lessons.
fn imports(text: &str) -> Vec<String> {
    let text = without_comments(text);
    let mut lines: Vec<&str> = text.lines().collect();
    // Frontmatter: a first line `---`, to the next `---` (an unclosed one
    // to the end).
    if lines.first().is_some_and(|line| line.trim() == "---") {
        match lines.iter().skip(1).position(|line| line.trim() == "---") {
            Some(end) => {
                lines.drain(..end + 2);
            }
            None => lines.clear(),
        }
    }
    let (mut fence, mut html) = (false, false);
    let mut found = Vec::new();
    for line in lines {
        let bare = line.trim_start();
        if html {
            // An HTML block runs to a blank line.
            html = !bare.is_empty();
            continue;
        }
        if bare.starts_with("```") || bare.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence || line.starts_with('\t') || line.starts_with("    ") {
            continue;
        }
        if bare.starts_with('<')
            && bare[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/')
        {
            html = true;
            continue;
        }
        // What is between backticks is code, not an import.
        let outside: String = line
            .split('`')
            .enumerate()
            .filter(|(at, _)| at % 2 == 0)
            .map(|(_, part)| part)
            .collect::<Vec<_>>()
            .join(" ");
        for word in outside.split_whitespace() {
            if let Some(path) = word.strip_prefix('@').filter(|path| !path.is_empty()) {
                found.push(path.to_owned());
            }
        }
    }
    found
}

/// `text` without its HTML comments, an unclosed one to the end.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        out.push_str(&rest[..at]);
        rest = match rest[at + 4..].find("-->") {
            Some(end) => &rest[at + 4 + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn codex(
    options: &[&str],
    root: &Path,
    end: usize,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<String, String> {
    let override_md = root.join("AGENTS.override.md");
    if override_md.is_file() {
        return Err(format!(
            "Codex reads {} in its AGENTS.md's place",
            override_md.display()
        ));
    }
    let (cap, from) = codex_cap(options, var)?;
    if cap == 0 {
        return Err(format!(
            "project_doc_max_bytes is 0 ({from}), so Codex reads no AGENTS.md"
        ));
    }
    if end > cap {
        return Err(format!(
            "the last lesson's line ends at byte {end} of AGENTS.md, past the {cap} bytes of \
             project instructions Codex reads (project_doc_max_bytes, {from})"
        ));
    }
    Ok(format!(
        "its lessons end within the {cap} bytes of project instructions Codex reads"
    ))
}

/// Codex's `project_doc_max_bytes` and where it comes from: a `-c`/`--config`
/// given to it, else the top level of `$CODEX_HOME` (else `~/.codex`)
/// `/config.toml`, else its default. A value that cannot be read is an error.
fn codex_cap(
    options: &[&str],
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<(usize, String), String> {
    const KEY: &str = "project_doc_max_bytes";
    let number = |text: &str| text.trim().replace('_', "").parse::<usize>().ok();
    for at in 0..options.len() {
        for flag in ["-c", "--config"] {
            let Some((written, value)) = valued(options, at, flag) else {
                continue;
            };
            if let Some((key, value)) = value.split_once('=') {
                if key.trim() == KEY {
                    return number(value)
                        .map(|cap| (cap, format!("`{written}`")))
                        .ok_or_else(|| format!("`{written}` is not a number of bytes"));
                }
            }
        }
    }
    let home = var("CODEX_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            var("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".codex"))
        });
    if let Some(file) = home.map(|dir| dir.join("config.toml")) {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                if line.starts_with('[') {
                    break;
                }
                if let Some((key, value)) = line.split_once('=') {
                    if key.trim() == KEY {
                        return number(value)
                            .map(|cap| (cap, format!("in {}", file.display())))
                            .ok_or_else(|| {
                                format!(
                                    "{KEY} in {} is not a number of bytes ohjaa reads",
                                    file.display()
                                )
                            });
                    }
                }
            }
        }
    }
    Ok((CODEX_PROJECT_DOC_MAX_BYTES, "its default".to_owned()))
}

fn pi(options: &[&str], root: &Path) -> Result<String, String> {
    if let Some(option) = options
        .iter()
        .find(|option| matches!(**option, "--no-context-files" | "-nc"))
    {
        return Err(format!(
            "`{option}` is passed to pi, which then reads no AGENTS.md or CLAUDE.md"
        ));
    }
    let override_md = root.join("AGENTS.override.md");
    if override_md.is_file() {
        return Err(format!(
            "pi reads {} in its AGENTS.md's place",
            override_md.display()
        ));
    }
    Ok("it reads the working folder's and its parents' context files".to_owned())
}

fn opencode(options: &[&str], var: &dyn Fn(&str) -> Option<OsString>) -> Result<String, String> {
    if let Some((written, _)) = (0..options.len()).find_map(|at| valued(options, at, "--server")) {
        return Err(format!(
            "`{written}` is passed to opencode, so a server ohjaa cannot see reads its \
             instructions"
        ));
    }
    for name in [
        "OPENCODE_DISABLE_PROJECT_CONFIG",
        "OPENCODE_CONFIG_PROJECT_DISABLE",
    ] {
        if let Some(value) = var(name).filter(|value| !value.is_empty()) {
            return Err(format!(
                "{name}={} is in the environment, so opencode reads no project instructions",
                value.to_string_lossy()
            ));
        }
    }
    Ok("it walks up from the launch folder for AGENTS.md".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_are_words_beginning_with_at_outside_fences_and_code() {
        assert_eq!(
            imports("@AGENTS.md\nsee @docs/x.md and `@not.md`\n```\n@fenced.md\n```\nme@mail.com"),
            vec!["AGENTS.md", "docs/x.md"]
        );
    }

    #[test]
    fn options_after_the_agents_own_double_dash_are_not_its_options() {
        let root = std::env::temp_dir();
        let none = |_: &str| -> Option<OsString> { None };
        let args = vec!["--".to_owned(), "-nc".to_owned()];
        assert!(matches!(
            reads_with(Some(("pi", &args)), &root, &root, 0, &none),
            Reads::Yes { .. }
        ));
        let args = vec!["-nc".to_owned()];
        assert!(matches!(
            reads_with(Some(("/usr/bin/pi", &args)), &root, &root, 0, &none),
            Reads::No { .. }
        ));
    }

    #[test]
    fn codexs_cap_comes_from_its_option_then_its_config_then_its_default() {
        let none = |_: &str| -> Option<OsString> { None };
        assert_eq!(
            codex_cap(&["-c", "project_doc_max_bytes=10"], &none)
                .unwrap()
                .0,
            10
        );
        assert_eq!(
            codex_cap(&["--config=project_doc_max_bytes=2_048"], &none)
                .unwrap()
                .0,
            2048
        );
        assert!(codex_cap(&["-c", "project_doc_max_bytes=lots"], &none).is_err());
        assert_eq!(
            codex_cap(&[], &none).unwrap().0,
            CODEX_PROJECT_DOC_MAX_BYTES
        );
    }
}
