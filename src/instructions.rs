//! Each agent's own global instruction file, so that an agent launched
//! without ohjaa still reads the person's general knowledge (mine, machine),
//! from one source: the nodes in the home.
//!
//! | agent       | the file it reads, the first that exists                          |
//! |-------------|-------------------------------------------------------------------|
//! | Claude Code | `$CLAUDE_CONFIG_DIR` (else `~/.claude`)`/CLAUDE.md`               |
//! | Codex       | `$CODEX_HOME` (else `~/.codex`)`/AGENTS.override.md`, `AGENTS.md` |
//! | pi          | `$PI_CODING_AGENT_DIR` (else `~/.pi/agent`)`/AGENTS.override.md`, |
//! |             | `AGENTS.md`, `AGENTS.MD`, `CLAUDE.md`, `CLAUDE.MD`                |
//! | opencode    | `$XDG_CONFIG_HOME` (else `~/.config`)`/opencode/AGENTS.md`        |
//!
//! ohjaa writes only inside its marked block (the kind `brief --write`
//! writes, opened by [`knowledge::INSTRUCTIONS_BEGIN`]), never the text
//! around it, and never creates a file: an agent whose file does not exist is
//! said so. A file inside a git repository gets no block either, since one
//! person's knowledge never goes into a file a repository shares, and a
//! block apply did not write (`brief --write`'s, a person's) is theirs: left
//! as it is, and said. `ohjaa apply` adds the block, `apply --undo` takes it
//! out, and each decision rewrites it where it is (a block a person took out
//! stays out). Each decision does so under the home's lock
//! ([`home::lock`]), so the block holds what was last decided.
//!
//! An agent `ohjaa run` launches whose own file holds that block, current
//! (byte for byte the one apply would write now), is not briefed with mine
//! and machine a second time ([`source`]): the briefing says where they are.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use faija_knowledge::Scope;

use crate::home;
use crate::knowledge::{self, Broken, Compact, Dirs};

/// One agent's own global instruction file.
#[derive(Debug, Clone)]
pub struct Target {
    pub agent: &'static str,
    /// Its key in `applied.toml`.
    pub key: &'static str,
    /// The file the agent reads, when one exists.
    pub file: Option<PathBuf>,
    /// The file it usually reads, named when none exists.
    pub usual: PathBuf,
    /// Its folder exists: the agent is set up on this machine.
    pub installed: bool,
}

/// Every agent's own instruction file, each where the agent finds it itself.
pub fn targets() -> Vec<Target> {
    table().into_iter().filter_map(target).collect()
}

/// The own instruction file of the agent `command` launches (its file name
/// says which: `claude`, `codex`, `pi`, `opencode`), looked for where that
/// agent finds it; `None` for any other command. No other agent's folder is
/// looked in.
pub fn target_of(command: &str) -> Option<Target> {
    let key = Path::new(command).file_name()?.to_str()?;
    table()
        .into_iter()
        .find(|(_, row_key, _, _)| *row_key == key)
        .and_then(target)
}

type Row = (
    &'static str,
    &'static str,
    Option<PathBuf>,
    &'static [&'static str],
);

/// Each agent, its key, the folder its own file is in (from the environment
/// alone) and the names it reads there, the first that exists.
fn table() -> [Row; 4] {
    let stores = crate::usage::Stores::from_env();
    let opencode = knowledge::config_home()
        .ok()
        .map(|dir| dir.join("opencode"));
    [
        ("Claude Code", "claude", stores.claude, &["CLAUDE.md"]),
        (
            "Codex",
            "codex",
            stores.codex,
            &["AGENTS.override.md", "AGENTS.md"],
        ),
        (
            "pi",
            "pi",
            stores.pi,
            &[
                "AGENTS.override.md",
                "AGENTS.md",
                "AGENTS.MD",
                "CLAUDE.md",
                "CLAUDE.MD",
            ],
        ),
        ("opencode", "opencode", opencode, &["AGENTS.md"]),
    ]
}

/// A row's target: the first of its names that is a file.
fn target((agent, key, dir, names): Row) -> Option<Target> {
    let dir = dir?;
    let usual = dir.join(if names.len() > 1 { names[1] } else { names[0] });
    Some(Target {
        agent,
        key,
        file: names
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.is_file()),
        usual,
        installed: dir.is_dir(),
    })
}

/// What an agent's own instruction file holds of mine and machine, against
/// the block apply would write now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    /// Apply's block, byte for byte the one it would write now.
    Current,
    /// Apply's block, and not the one it would write now.
    Stale,
    /// No block.
    None,
    /// A block apply did not write (`brief --write`'s, a person's).
    Theirs,
}

/// What `text`, an agent's own file, holds, against `compact`: decided by
/// the bytes alone, never by a time. A block that cannot be told from the
/// rest (two, a marker without its partner) is an error.
pub fn block_in(text: &str, compact: &Compact) -> Result<Block> {
    Ok(match knowledge::held(text)? {
        knowledge::Held::None => Block::None,
        knowledge::Held::Other => Block::Theirs,
        knowledge::Held::Applied => {
            let (new, _) =
                knowledge::with_block(text, knowledge::INSTRUCTIONS_BEGIN, &compact.text)?;
            if new == text {
                Block::Current
            } else {
                Block::Stale
            }
        }
    })
}

/// Where the agent `ohjaa run` launches gets mine and machine from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Its own instruction file holds them, current: the briefing leaves
    /// them out.
    OwnFile { agent: &'static str, file: PathBuf },
    /// The briefing carries them, because of `why`.
    Briefing { agent: &'static str, why: String },
}

/// For the agent `command` launches with `args` (what follows the command),
/// whether it reads its own instruction file, and that file holds `compact`
/// (mine and machine, as apply would write them now) current. `None` for a
/// command that is no agent ohjaa writes the own file of. One file is read,
/// and no git runs.
pub fn source(command: &str, args: &[String], compact: &Compact) -> Option<Source> {
    let target = target_of(command)?;
    let agent = target.agent;
    let carried = |why: String| Some(Source::Briefing { agent, why });
    let looked = target.file.as_deref().unwrap_or(&target.usual);
    if let Some(why) = not_read(target.key, args, looked, |name| std::env::var_os(name)) {
        return carried(why);
    }
    let Some(file) = target.file else {
        return carried(format!("{} does not exist", target.usual.display()));
    };
    let shown = file.display();
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) => return carried(format!("{shown} cannot be read ({error})")),
    };
    match block_in(&text, compact) {
        Ok(Block::Current) => Some(Source::OwnFile { agent, file }),
        Ok(Block::Stale) => carried(format!(
            "the ohjaa block in {shown} is not the one `ohjaa apply` would write now"
        )),
        Ok(Block::None) => carried(format!(
            "{shown} holds no ohjaa block (`ohjaa apply` writes it)"
        )),
        Ok(Block::Theirs) => carried(format!(
            "the ohjaa block in {shown} is not apply's: `ohjaa brief --write` or the person \
             wrote it"
        )),
        Err(error) => carried(format!(
            "{shown} cannot be read as one ohjaa block ({error})"
        )),
    }
}

/// Why the agent `key`, launched with `args` in the environment `var`
/// reads, will not read its own instruction file `file`, when its own
/// options or variables say so (Claude Code 2.1.311, from its `--help` and
/// what it does):
/// - `--bare` and `--safe-mode`, before its own `--`, and their variables
///   `CLAUDE_CODE_SIMPLE` and `CLAUDE_CODE_SAFE_MODE` when truthy as it
///   reads them (`1`, `true`, `yes`, `on`, in any case), skip every CLAUDE.md;
/// - `--setting-sources` without `user` skips the user's own CLAUDE.md;
/// - `CLAUDE_CONFIG_DIR` set but empty is taken as is, so the file is not
///   the one under `~/.claude` that an empty variable means to ohjaa.
///
/// Codex 0.164.0 has no option or setting in `codex --help` or `codex exec
/// --help` that skips `$CODEX_HOME/AGENTS.md`, nor do pi and opencode here.
fn not_read(
    key: &str,
    args: &[String],
    file: &Path,
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<String> {
    if key != "claude" {
        return None;
    }
    let options: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
        .collect();
    for (at, option) in options.iter().enumerate() {
        if matches!(*option, "--bare" | "--safe-mode") {
            return Some(format!(
                "`{option}` is passed to Claude Code, which then reads no CLAUDE.md"
            ));
        }
        let sources = match option.strip_prefix("--setting-sources") {
            Some("") => options.get(at + 1).map(|value| (*value, " ")),
            Some(value) => value.strip_prefix('=').map(|value| (value, "=")),
            None => None,
        };
        if let Some((value, joined)) = sources {
            if !value.split(',').any(|source| source.trim() == "user") {
                return Some(format!(
                    "`--setting-sources{joined}{value}` leaves out `user`, so Claude Code does \
                     not read {}",
                    file.display()
                ));
            }
        }
    }
    for name in ["CLAUDE_CODE_SIMPLE", "CLAUDE_CODE_SAFE_MODE"] {
        let Some(value) = var(name) else { continue };
        let value = value.to_string_lossy().into_owned();
        if matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ) {
            return Some(format!(
                "{name}={value} is in the environment, so Claude Code reads no CLAUDE.md"
            ));
        }
    }
    if var("CLAUDE_CONFIG_DIR").is_some_and(|value| value.is_empty()) {
        return Some(format!(
            "CLAUDE_CONFIG_DIR is set but empty, so Claude Code does not read {}",
            file.display()
        ));
    }
    None
}

/// The repository a file is inside, from the file system (a `.git` beside
/// the file or above it, the file's links followed): no git process.
pub fn inside_repository(file: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    real.parent()?
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The home's own knowledge (mine, machine) as an agent's file holds it, and
/// the files that could not be read as nodes.
pub fn home_knowledge() -> Result<(Compact, Vec<Broken>)> {
    let dirs = Dirs {
        ours_checkout: None,
        repo_root: None,
        ..Dirs::resolve(None)?
    };
    let loaded = knowledge::load(&dirs)?;
    let compact = knowledge::render_compact(
        &loaded,
        &[Scope::Mine, Scope::Machine],
        knowledge::DEFAULT_BUDGET,
    );
    Ok((compact, loaded.broken))
}

/// After a decision: the block rewritten in each agent's file that holds it,
/// and each file rewritten, said. Never an error: what cannot be read or
/// written is said on stderr, and the decision stands.
pub fn sync() -> Vec<String> {
    let compact = match home_knowledge() {
        Ok((compact, _)) => compact,
        Err(error) => {
            eprintln!("ohjaa: warning: the agents' instruction files are not rewritten: {error:#}");
            return Vec::new();
        }
    };
    let mut said = Vec::new();
    for target in targets() {
        let Some(file) = target.file else {
            continue;
        };
        match rewrite(&file, &compact) {
            Ok(Rewrite::Rewritten) => said.push(format!(
                "rewrote the knowledge block in {} ({}'s own instructions; {})",
                file.display(),
                target.agent,
                compact.summary()
            )),
            Ok(Rewrite::Theirs) => said.push(left_theirs(&file)),
            Ok(Rewrite::Unchanged) => {}
            Err(error) => eprintln!(
                "ohjaa: warning: the knowledge block in {} is not rewritten: {error:#}",
                file.display()
            ),
        }
    }
    said
}

/// What is said of a block apply did not write, wherever it is left.
pub fn left_theirs(file: &Path) -> String {
    format!(
        "left the knowledge block in {}: apply did not write it (`ohjaa brief --write` or the \
         person did), so it is theirs",
        file.display()
    )
}

enum Rewrite {
    Rewritten,
    Unchanged,
    /// A block apply did not write: the person's, left as it is.
    Theirs,
}

/// The block rewritten in `file` if it holds the one apply wrote and it
/// differs. A file inside a repository is never written.
fn rewrite(file: &Path, compact: &Compact) -> Result<Rewrite> {
    let now =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    match knowledge::held(&now)? {
        knowledge::Held::None => return Ok(Rewrite::Unchanged),
        knowledge::Held::Other => return Ok(Rewrite::Theirs),
        knowledge::Held::Applied => {}
    }
    if let Some(root) = inside_repository(file) {
        anyhow::bail!(
            "it is inside the repository {}, and one person's knowledge never goes into a file \
             a repository shares",
            root.display()
        );
    }
    let (new, _) = knowledge::with_block(&now, knowledge::INSTRUCTIONS_BEGIN, &compact.text)?;
    if new == now {
        return Ok(Rewrite::Unchanged);
    }
    home::write_file(file, &new, None)?;
    Ok(Rewrite::Rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_file_is_inside_the_repository_a_git_folder_above_it_names() {
        let dir = std::env::temp_dir().join(format!("ohjaa-instr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dotfiles/.git")).unwrap();
        std::fs::create_dir_all(dir.join("dotfiles/claude")).unwrap();
        std::fs::create_dir_all(dir.join("home/.claude")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let shared = dir.join("dotfiles/claude/CLAUDE.md");
        std::fs::write(&shared, "# Mine\n").unwrap();
        assert_eq!(inside_repository(&shared), Some(dir.join("dotfiles")));
        let own = dir.join("home/.claude/CLAUDE.md");
        std::fs::write(&own, "# Mine\n").unwrap();
        assert_eq!(inside_repository(&own), None);
        // A link into a repository is the file it names.
        let linked = dir.join("home/.claude/AGENTS.md");
        std::os::unix::fs::symlink(&shared, &linked).unwrap();
        assert_eq!(inside_repository(&linked), Some(dir.join("dotfiles")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
