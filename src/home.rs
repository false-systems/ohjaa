//! The workflow home: `$XDG_CONFIG_HOME/ohjaa` (else `~/.config/ohjaa`).
//!
//! The agent workflow (the gate, the hooks, the launcher) lives here, outside
//! the product repositories, and is applied to them from outside. Nothing is
//! installed on, or merged into, a repository.
//!
//! ```text
//! ~/.config/ohjaa/
//!   repos/<owner>/<repo>/
//!     repo.toml    branch = "main"   the only per-repo workflow fact
//!     sykli.json   the gate (a sykli contract), inputs repo-relative
//!     sykli.lock
//!   git/config                       includeIf per entry remote -> gated.config
//!   git/gated.config                 core.hooksPath = <home>/hooks
//!   hooks/<name>                     exec <ohjaa> hook <name> "$@", for
//!                                    pre-push and each hook a gated clone has
//!   applied.toml                     what apply changed, for `apply --undo`
//!   tools.lock                       the tool revisions `apply --tools` installs
//! ```
//!
//! The knowledge scopes (`mine/`, `machine/`) and the `roots`, `ours` and
//! `drafter` files live in the same directory and are not touched here.
//!
//! A repository is found by its remote: `git@github.com:o/r.git`,
//! `https://github.com/o/r` and `ssh://git@github.com/o/r.git` are all
//! `github.com/o/r`, and its entry is `repos/o/r/`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::flat_toml;
use crate::knowledge::{config_home, expand_home};
use crate::process;

/// `$XDG_CONFIG_HOME/ohjaa`, else `~/.config/ohjaa`.
pub fn ohjaa_home() -> Result<PathBuf> {
    Ok(config_home()?.join("ohjaa"))
}

pub fn hooks_dir(home: &Path) -> PathBuf {
    home.join("hooks")
}

pub fn tools_lock(home: &Path) -> PathBuf {
    home.join("tools.lock")
}

/// A repository as its remote names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    /// `None` for a remote that is a local path.
    pub host: Option<String>,
    pub owner: String,
    pub name: String,
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Some(host) => write!(f, "{host}/{}/{}", self.owner, self.name),
            None => write!(f, "{}/{}", self.owner, self.name),
        }
    }
}

impl RepoId {
    /// Normalise a remote URL in any of git's forms to host, owner and name.
    ///
    /// - `scheme://[user@]host[:port]/owner/repo[.git][/]`
    /// - `[user@]host:owner/repo[.git]` (scp-like: no `/` before the first `:`)
    /// - `file:///path/owner/repo` and plain paths, which have no host
    ///
    /// The owner and name are the last two path components. `None` when there
    /// are not two, or one could not be a directory name.
    pub fn parse(url: &str) -> Option<RepoId> {
        let url = url.trim();
        let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
            if scheme.eq_ignore_ascii_case("file") {
                (None, rest)
            } else {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                let host = authority.rsplit('@').next().unwrap_or(authority);
                let host = match host.strip_prefix('[') {
                    Some(v6) => v6.split(']').next().unwrap_or(v6),
                    None => host.split(':').next().unwrap_or(host),
                };
                (Some(host), path)
            }
        } else {
            match url.split_once(':') {
                Some((before, after)) if !before.contains('/') && !before.is_empty() => {
                    (Some(before.rsplit('@').next().unwrap_or(before)), after)
                }
                _ => (None, url),
            }
        };
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.rsplit('/').filter(|part| !part.is_empty());
        let name = parts.next()?;
        let owner = parts.next()?;
        let usable = |part: &str| part != "." && part != ".." && !part.contains('\\');
        if !usable(owner) || !usable(name) || host.is_some_and(str::is_empty) {
            return None;
        }
        Some(RepoId {
            host: host.map(str::to_ascii_lowercase),
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// `owner/name`, which is what an entry is filed under.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// A repository's entry: where the workflow keeps its gate.
#[derive(Debug, Clone)]
pub struct Entry {
    pub dir: PathBuf,
    /// The branch the gate protects.
    pub branch: String,
}

impl Entry {
    pub fn contract(&self) -> PathBuf {
        self.dir.join("sykli.json")
    }

    pub fn has_contract(&self) -> bool {
        self.contract().is_file()
    }
}

/// `repos/<owner>/<name>`, lower case: owners and repositories are
/// case-insensitive on GitHub, and some file systems are not.
pub fn entry_dir(home: &Path, id: &RepoId) -> PathBuf {
    home.join("repos")
        .join(id.owner.to_ascii_lowercase())
        .join(id.name.to_ascii_lowercase())
}

/// What `repo.toml` holds: the branch, and nothing else.
pub fn repo_toml(id: &RepoId, branch: &str) -> String {
    format!(
        "# ohjaa's entry for {}, written by `ohjaa adopt`. The gate beside it\n\
         # (sykli.json) protects this branch; nothing else about the repo lives here.\n\
         branch = {}\n",
        id.slug(),
        flat_toml::quote(branch)
    )
}

fn parse_repo_toml(text: &str) -> std::result::Result<String, String> {
    let tables = flat_toml::parse(text)?;
    if let Some(table) = tables.keys().find(|name| !name.is_empty()) {
        return Err(format!(
            "unknown table [{table}]; repo.toml holds only branch"
        ));
    }
    flat_toml::only_keys(&tables, "", &["branch"])?;
    let branch = tables[""]
        .get("branch")
        .ok_or("no branch = \"…\"")?
        .to_owned();
    if branch.is_empty() || branch.starts_with('-') || branch.chars().any(char::is_whitespace) {
        return Err(format!("{branch:?} is not a branch name"));
    }
    Ok(branch
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch)
        .to_owned())
}

/// The entry at `dir`, `Ok(None)` when there is none. A directory with files
/// but no readable `repo.toml` is an error, never "no entry": the gate it
/// holds must not be skipped because its branch cannot be read.
pub fn read_entry(dir: &Path) -> Result<Option<Entry>> {
    let path = dir.join("repo.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let branch = parse_repo_toml(&text)
                .map_err(|why| anyhow::anyhow!("{}: {why}", path.display()))?;
            Ok(Some(Entry {
                dir: dir.to_path_buf(),
                branch,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let holds_files = std::fs::read_dir(dir)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false);
            if holds_files {
                bail!("{} has files but no repo.toml", dir.display());
            }
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The entry for `id`. Owners and repositories are case-insensitive on
/// GitHub, so `False-Systems/Ohjaa` finds `repos/false-systems/ohjaa/` on
/// any file system.
pub fn find_entry(home: &Path, id: &RepoId) -> Result<Option<Entry>> {
    let exact = entry_dir(home, id);
    if exact.is_dir() {
        return read_entry(&exact);
    }
    let wanted = id.slug();
    match entry_dirs(home)?
        .into_iter()
        .find(|(slug, _)| slug.eq_ignore_ascii_case(&wanted))
    {
        Some((_, dir)) => read_entry(&dir),
        None => Ok(None),
    }
}

/// Every entry directory under `repos/`, as `(owner/name, dir)`.
pub fn entry_dirs(home: &Path) -> Result<Vec<(String, PathBuf)>> {
    let repos = home.join("repos");
    let mut found = Vec::new();
    let owners = match std::fs::read_dir(&repos) {
        Ok(owners) => owners,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", repos.display()))
        }
    };
    for owner in owners.flatten().filter(|e| e.path().is_dir()) {
        let names = std::fs::read_dir(owner.path())
            .with_context(|| format!("cannot read {}", owner.path().display()))?;
        for name in names.flatten().filter(|e| e.path().is_dir()) {
            found.push((
                format!(
                    "{}/{}",
                    owner.file_name().to_string_lossy(),
                    name.file_name().to_string_lossy()
                ),
                name.path(),
            ));
        }
    }
    found.sort();
    Ok(found)
}

/// Run git and return its trimmed stdout, `Ok(None)` when it exits 1 (git
/// config's "not set"), an error otherwise.
pub fn git_value(args: &[&str], cwd: Option<&Path>) -> Result<Option<String>> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    value_of(&mut command, args)
}

pub fn value_of(command: &mut Command, args: &[&str]) -> Result<Option<String>> {
    let output = process::run(command, None, process::LOCAL).context("cannot run git")?;
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout_text().trim().to_owned())),
        Some(1) if output.stderr.is_empty() => Ok(None),
        _ => bail!("git {} failed: {}", args.join(" "), output.failure_detail()),
    }
}

/// `git config --global …`, run outside every repository: with `--includes`,
/// a repository's `includeIf` (ohjaa's own among them) would otherwise count
/// as global wherever the command happens to run.
fn global_git(args: &[&str]) -> Result<Option<String>> {
    let (mut command, full) = global_command(args);
    value_of(&mut command, &full)
}

fn global_command<'a>(args: &[&'a str]) -> (Command, Vec<&'a str>) {
    let mut full = vec!["config", "--global"];
    full.extend_from_slice(args);
    let mut command = Command::new("git");
    command.args(&full).current_dir("/");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(var);
    }
    (command, full)
}

/// A global git setting that names a path, `~` expanded, wherever git's
/// global config sets it: in the file itself or in a file it includes.
pub fn global_path_setting(key: &str) -> Result<Option<String>> {
    global_git(&["--includes", "--type=path", "--get", key])
}

/// Whether git's global config file itself (not a file it includes) sets
/// `key`: only that is ohjaa's to change.
pub fn global_sets_directly(key: &str) -> Result<bool> {
    Ok(global_git(&["--no-includes", "--get", key])?.is_some())
}

/// The file a global setting comes from, as `git config --show-origin` says.
pub fn global_origin(key: &str) -> Result<Option<String>> {
    Ok(global_git(&["--includes", "--show-origin", "--get", key])?
        .and_then(|line| line.split('\t').next().map(str::to_owned)))
}

pub fn set_global(key: &str, value: &str) -> Result<()> {
    global_git(&[key, value])?
        .map(|_| ())
        .with_context(|| format!("git config --global {key} did not take"))
}

/// Every value of a global setting that names a path, `~` expanded, in the
/// global file itself.
pub fn global_paths(key: &str) -> Result<Vec<String>> {
    Ok(global_git(&["--type=path", "--get-all", key])?
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default())
}

/// Add one more value to a global setting.
pub fn add_global(key: &str, value: &str) -> Result<()> {
    global_git(&["--add", key, value])?
        .map(|_| ())
        .with_context(|| format!("git config --global --add {key} did not take"))
}

/// Remove the global setting's values equal to `value` (all of them when
/// `None`). Git drops a section left empty.
pub fn unset_global(key: &str, value: Option<&str>) -> Result<()> {
    let mut args = vec!["--unset-all", key];
    if let Some(value) = value {
        args = vec!["--fixed-value", "--unset-all", key, value];
    }
    let (mut command, full) = global_command(&args);
    let output = process::run(&mut command, None, process::LOCAL).context("cannot run git")?;
    match output.status.code() {
        // Exit 5: nothing to unset, which is what was wanted.
        Some(0 | 5) => Ok(()),
        _ => bail!("git {} failed: {}", full.join(" "), output.failure_detail()),
    }
}

/// The `insteadOf` rules in git's global config, includes followed, as
/// `git config -z --get-regexp` prints them.
pub fn global_rewrites_text() -> Result<String> {
    Ok(
        global_git(&["--includes", "-z", "--get-regexp", r"^url\..*\.insteadof$"])?
            .unwrap_or_default(),
    )
}

/// Git's global excludes file: `core.excludesFile` when set (includes
/// followed), else `$XDG_CONFIG_HOME/git/ignore`, else `~/.config/git/ignore`.
pub fn global_ignore_file() -> Result<PathBuf> {
    if let Some(value) = global_path_setting("core.excludesFile")? {
        if !value.is_empty() {
            return Ok(expand_home(&value));
        }
    }
    Ok(config_home()?.join("git").join("ignore"))
}

/// zsh's startup file: `$ZDOTDIR/.zshrc`, else `~/.zshrc`.
pub fn zshrc() -> Result<PathBuf> {
    let dir = std::env::var_os("ZDOTDIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from)
        })
        .context("HOME is not set")?;
    Ok(dir.join(".zshrc"))
}

/// Two paths name the same place, whether or not either exists.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let clean = |path: &Path| {
        std::fs::canonicalize(path).unwrap_or_else(|_| {
            PathBuf::from(path.to_string_lossy().trim_end_matches('/').to_owned())
        })
    };
    clean(a) == clean(b)
}

/// Replace `path`'s contents without a moment where it is half written. A
/// symbolic link (a dotfiles checkout) is written through, never replaced,
/// and the file keeps its permissions.
pub fn write_file(path: &Path, contents: &str, mode: Option<u32>) -> Result<()> {
    write_checked(path, contents, mode, |_| Ok(()))
}

/// [`write_file`], with `check` run on the written temporary file before it
/// replaces `path`; a failed check leaves `path` as it was.
pub fn write_checked(
    path: &Path,
    contents: &str,
    mode: Option<u32>,
    check: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .with_context(|| format!("{} has no directory", target.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.ohjaa-{}", std::process::id()));
    std::fs::write(&temp, contents).with_context(|| format!("cannot write {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = mode.or_else(|| {
            std::fs::metadata(&target)
                .ok()
                .map(|meta| meta.permissions().mode() & 0o7777)
        });
        if let Some(mode) = mode {
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))
                .with_context(|| format!("cannot set the mode of {}", temp.display()))?;
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    if let Err(error) = check(&temp) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    std::fs::rename(&temp, &target).with_context(|| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot replace {}", target.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_remote_form_names_the_same_repository() {
        let github = Some("github.com".to_owned());
        for url in [
            "git@github.com:false-systems/ohjaa.git",
            "git@github.com:false-systems/ohjaa",
            "github.com:false-systems/ohjaa.git",
            "ssh://git@github.com/false-systems/ohjaa.git",
            "ssh://git@github.com:22/false-systems/ohjaa",
            "https://github.com/false-systems/ohjaa.git",
            "https://github.com/false-systems/ohjaa/",
            "https://x-access-token:secret@GitHub.com/false-systems/ohjaa",
            "git://github.com/false-systems/ohjaa.git",
        ] {
            let id = RepoId::parse(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(id.host, github, "{url}");
            assert_eq!(id.slug(), "false-systems/ohjaa", "{url}");
            assert_eq!(id.to_string(), "github.com/false-systems/ohjaa", "{url}");
        }
    }

    #[test]
    fn a_local_remote_has_no_host() {
        for url in [
            "/tmp/remotes/acme/widget.git",
            "file:///tmp/remotes/acme/widget.git",
            "../remotes/acme/widget",
        ] {
            let id = RepoId::parse(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(id.host, None, "{url}");
            assert_eq!(id.slug(), "acme/widget", "{url}");
        }
    }

    #[test]
    fn a_remote_without_owner_and_name_is_not_a_repository() {
        for url in [
            "",
            "widget",
            "git@github.com:widget.git",
            "https://github.com/",
            "/a/..",
        ] {
            assert_eq!(RepoId::parse(url), None, "{url:?}");
        }
    }

    #[test]
    fn repo_toml_holds_the_branch_and_nothing_else() {
        let id = RepoId::parse("git@github.com:acme/widget.git").unwrap();
        assert_eq!(parse_repo_toml(&repo_toml(&id, "main")).unwrap(), "main");
        assert_eq!(
            parse_repo_toml("branch = \"refs/heads/trunk\"").unwrap(),
            "trunk"
        );
        for (text, why) in [
            ("", "no branch"),
            ("branch = \"\"", "not a branch name"),
            (
                "branch = \"main\"\ncontract = \"x\"",
                "unknown key contract",
            ),
            ("[gate]\nbranch = \"main\"", "unknown table"),
            ("branch = main", "only \"text\" values"),
        ] {
            let error = parse_repo_toml(text).unwrap_err();
            assert!(error.contains(why), "{text:?}: {error}");
        }
    }

    #[test]
    fn an_entry_directory_without_repo_toml_is_an_error_not_an_absence() {
        let dir = std::env::temp_dir().join(format!("ohjaa-entry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            read_entry(&dir).unwrap().is_none(),
            "no directory: no entry"
        );
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            read_entry(&dir).unwrap().is_none(),
            "empty directory: no entry"
        );
        std::fs::write(dir.join("sykli.json"), "{}").unwrap();
        let error = read_entry(&dir).unwrap_err().to_string();
        assert!(error.contains("no repo.toml"), "{error}");
        std::fs::write(dir.join("repo.toml"), "branch = \"main\"\n").unwrap();
        let entry = read_entry(&dir).unwrap().unwrap();
        assert_eq!(entry.branch, "main");
        assert!(entry.has_contract());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
