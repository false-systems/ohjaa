//! What ohjaa reads of git's config and layout: the global excludes file
//! (where `apply` keeps its ignore block), and a checkout's common directory
//! and sibling worktrees from its files.
//!
//! Facts are read once and passed down: the global config in one `git config
//! --global --includes -z --list --show-origin`, and the common directory
//! from the repository's layout.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::home;
use crate::knowledge::{config_home, expand_home};
use crate::process;

/// The environment a git run of ohjaa's own must not inherit from whatever
/// started ohjaa (git sets these for its hooks and aliases): it would point
/// git at another repository.
pub const REPO_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

/// One setting of a `git config -z --list` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// `file:<path>` and the like, with `--show-origin`.
    pub origin: Option<String>,
    /// As git prints it: section and name lower case, subsection as written.
    pub key: String,
    pub value: String,
}

/// A `git config -z --list [--show-origin]` listing, in git's order (the
/// last of a key is the one that applies).
#[derive(Debug, Default, Clone)]
pub struct Listing(pub Vec<Setting>);

impl Listing {
    pub fn parse(text: &str, with_origin: bool) -> Listing {
        let mut settings = Vec::new();
        let mut origin = None;
        for token in text.split('\0') {
            if with_origin && origin.is_none() {
                origin = Some(token.to_owned());
                continue;
            }
            if token.is_empty() && origin.is_none() {
                continue;
            }
            let (key, value) = token.split_once('\n').unwrap_or((token, ""));
            if !key.is_empty() {
                settings.push(Setting {
                    origin: origin.take(),
                    key: key.to_owned(),
                    value: value.to_owned(),
                });
            }
            origin = None;
        }
        Listing(settings)
    }

    pub fn last(&self, key: &str) -> Option<&Setting> {
        self.0.iter().rev().find(|setting| setting.key == key)
    }
}

/// Run git, returning stdout as it is, or why it failed.
fn listing_of(mut command: Command, args: &[&str]) -> Result<std::result::Result<String, String>> {
    let output = process::run(&mut command, None, process::LOCAL).context("cannot run git")?;
    if output.status.success() {
        return Ok(Ok(output.stdout_text()));
    }
    let _ = args;
    Ok(Err(output.failure_detail()))
}

/// The files `git config --global` reads: `$GIT_CONFIG_GLOBAL`, else
/// `~/.gitconfig` and `$XDG_CONFIG_HOME/git/config`.
pub fn global_files() -> Result<Vec<PathBuf>> {
    if let Some(file) = std::env::var_os("GIT_CONFIG_GLOBAL").filter(|f| !f.is_empty()) {
        return Ok(vec![PathBuf::from(file)]);
    }
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")?;
    Ok(vec![
        home.join(".gitconfig"),
        config_home()?.join("git").join("config"),
    ])
}

/// Git's global config, read once: every setting, includes followed. Read
/// outside every repository, so no repository's `includeIf` counts as
/// global.
#[derive(Debug, Default)]
pub struct GlobalFacts {
    pub listing: Listing,
}

impl GlobalFacts {
    pub fn read() -> Result<GlobalFacts> {
        let args = [
            "config",
            "--global",
            "--includes",
            "-z",
            "--list",
            "--show-origin",
        ];
        let mut command = Command::new("git");
        command.args(args).current_dir("/");
        for var in REPO_ENV {
            command.env_remove(var);
        }
        let files = global_files()?;
        let listing = match listing_of(command, &args)? {
            Ok(text) => Listing::parse(&text, true),
            // No global config file at all is an empty one.
            Err(_) if files.iter().all(|file| !file.exists()) => Listing::default(),
            Err(why) => bail!("git config --global --includes --list failed: {why}"),
        };
        Ok(GlobalFacts { listing })
    }

    /// Git's global excludes file: `core.excludesFile` when set, else
    /// `$XDG_CONFIG_HOME/git/ignore`, else `~/.config/git/ignore`.
    pub fn excludes_file(&self) -> Result<PathBuf> {
        match self
            .listing
            .last("core.excludesfile")
            .filter(|setting| !setting.value.is_empty())
        {
            Some(setting) => Ok(expand_home(&setting.value)),
            None => Ok(config_home()?.join("git").join("ignore")),
        }
    }
}

pub fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// A clone's common directory: from its layout when that says, else asked
/// of git (one process).
pub fn common_dir(repo: &Path) -> Result<PathBuf> {
    if let Some(common) = common_dir_in(repo) {
        return Ok(common);
    }
    let mut command = Command::new("git");
    command
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(repo);
    for var in REPO_ENV {
        command.env_remove(var);
    }
    Ok(PathBuf::from(
        home::value_of(&mut command, &["rev-parse", "--git-common-dir"])?
            .context("git names no repository there")?,
    ))
}

/// The work tree's root and the common git directory of the repository at
/// or above `start`: the nearest directory holding `.git`, read from its
/// layout, not above a `GIT_CEILING_DIRECTORIES` entry. Git is asked (one
/// process) only when the layout does not say, or `GIT_DIR` /
/// `GIT_WORK_TREE` point elsewhere.
pub fn repo_at(start: &Path) -> Result<(PathBuf, PathBuf)> {
    let start = home::absolute(start.to_path_buf());
    let set = |var: &str| std::env::var_os(var).is_some_and(|value| !value.is_empty());
    if !set("GIT_DIR") && !set("GIT_WORK_TREE") {
        let ceilings: Vec<PathBuf> = std::env::var_os("GIT_CEILING_DIRECTORIES")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        for dir in start.ancestors() {
            if dir != start && ceilings.iter().any(|ceiling| home::same_path(ceiling, dir)) {
                break;
            }
            if dir.join(".git").exists() {
                if let Some(common) = common_dir_in(dir) {
                    return Ok((dir.to_path_buf(), common));
                }
                break;
            }
        }
    }
    let text = home::git_value(
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
        Some(&start),
    )
    .with_context(|| format!("{} is not inside a git repository", start.display()))?
    .with_context(|| format!("{} is not inside a git repository", start.display()))?;
    let mut lines = text.lines();
    match (lines.next(), lines.next()) {
        (Some(top), Some(common)) => Ok((PathBuf::from(top), PathBuf::from(common))),
        _ => bail!("git named no work tree for {}", start.display()),
    }
}

/// The common git directory of the checkout at `cwd`, as git's on-disk
/// layout names it (gitrepository-layout(5)): `cwd/.git` is the git
/// directory, or a `gitdir: <path>` file names it, and a git directory with
/// a `commondir` file shares that one's. `None` when the layout does not
/// say, and git must be asked.
pub fn common_dir_in(cwd: &Path) -> Option<PathBuf> {
    let dot_git = cwd.join(".git");
    let git_dir = if std::fs::metadata(&dot_git).ok()?.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        cwd.join(text.strip_prefix("gitdir:")?.trim())
    };
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Some(git_dir.join(common.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(git_dir),
        Err(_) => None,
    }
}

/// The Git repository `path` lies in, canonical. A path not created yet (a
/// file the agent will write) is looked up from its nearest existing
/// directory. `None` when that directory is not inside a repository.
pub fn repo_of(path: &Path) -> Option<PathBuf> {
    let dir = path.ancestors().find(|dir| dir.is_dir())?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"]);
    let output = process::run(&mut command, None, process::LOCAL).ok()?;
    if !output.status.success() {
        return None;
    }
    std::fs::canonicalize(output.stdout_text().trim()).ok()
}

/// Every checkout of the clone whose common git dir is `common`, canonical,
/// from its layout alone (no git): the main one when the common dir is its
/// `.git`, then each linked one `<common>/worktrees/*/gitdir` names, whether
/// or not its folder still exists.
pub fn layout_checkouts(common: &Path) -> Vec<PathBuf> {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut checkouts: Vec<PathBuf> = Vec::new();
    if common.file_name().is_some_and(|name| name == ".git") {
        if let Some(main) = common.parent() {
            checkouts.push(canonical(main));
        }
    }
    if let Ok(linked) = std::fs::read_dir(common.join("worktrees")) {
        let mut linked: Vec<PathBuf> = linked.flatten().map(|entry| entry.path()).collect();
        linked.sort();
        for admin in linked {
            let Ok(gitdir) = std::fs::read_to_string(admin.join("gitdir")) else {
                continue;
            };
            let gitdir = admin.join(gitdir.trim());
            if let Some(worktree) = gitdir.parent() {
                checkouts.push(canonical(worktree));
            }
        }
    }
    checkouts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_dir_is_read_from_the_layout_git_wrote() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        let linked = root.join("linked");
        std::fs::create_dir_all(main.join(".git/worktrees/linked")).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        // A linked worktree, written with relative paths (`--relative-paths`).
        std::fs::write(
            linked.join(".git"),
            "gitdir: ../main/.git/worktrees/linked\n",
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/linked/commondir"), "../..\n").unwrap();
        let canonical = |path: PathBuf| std::fs::canonicalize(path).unwrap();
        let common = canonical(main.join(".git"));
        let found = |cwd: &Path| common_dir_in(cwd).map(canonical);
        assert_eq!(found(&main), Some(common.clone()), "the main checkout");
        assert_eq!(found(&linked), Some(common), "a linked worktree");
        assert_eq!(found(&root), None, "no layout here: ask git");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn listings_are_read_with_and_without_origins() {
        let global = Listing::parse(
            "file:/h/.gitconfig\0core.hookspath\n~/h\0file:/h/inc\0core.excludesfile\n~/x\0\
             file:/h/.gitconfig\0url.git@github.com:.insteadof\ngh:\0file:/h/.gitconfig\0flag.bare\0",
            true,
        );
        assert_eq!(global.0.len(), 4);
        assert_eq!(global.0[1].origin.as_deref(), Some("file:/h/inc"));
        assert_eq!(global.last("core.excludesfile").unwrap().value, "~/x");
        assert_eq!(global.last("flag.bare").unwrap().value, "");
    }
}
