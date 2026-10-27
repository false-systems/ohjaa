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
//! ohjaa writes only inside its marked block (the one `brief --write`
//! writes, opened by [`knowledge::INSTRUCTIONS_BEGIN`]), never the text
//! around it, and never creates a file: an agent whose file does not exist is
//! said so. A file inside a git repository gets no block either, since one
//! person's knowledge never goes into a file a repository shares. `ohjaa
//! apply` adds the block, `apply --undo` takes it out, and each decision
//! rewrites it where it is (a block a person took out stays out).

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
    let stores = crate::usage::Stores::from_env();
    let opencode = knowledge::config_home()
        .ok()
        .map(|dir| dir.join("opencode"));
    let table: [(&str, &str, Option<PathBuf>, &[&str]); 4] = [
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
    ];
    table
        .into_iter()
        .filter_map(|(agent, key, dir, names)| {
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
        })
        .collect()
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
            Ok(true) => said.push(format!(
                "rewrote the knowledge block in {} ({}'s own instructions; {})",
                file.display(),
                target.agent,
                compact.summary()
            )),
            Ok(false) => {}
            Err(error) => eprintln!(
                "ohjaa: warning: the knowledge block in {} is not rewritten: {error:#}",
                file.display()
            ),
        }
    }
    said
}

/// The block rewritten in `file` if it holds one and it differs; whether
/// it was. A file inside a repository is never written.
fn rewrite(file: &Path, compact: &Compact) -> Result<bool> {
    let now =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    if !knowledge::has_block(&now)? {
        return Ok(false);
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
        return Ok(false);
    }
    home::write_file(file, &new, None)?;
    Ok(true)
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
