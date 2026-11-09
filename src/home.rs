//! The workflow home: `$XDG_CONFIG_HOME/ohjaa` (else `~/.config/ohjaa`).
//!
//! ```text
//! ~/.config/ohjaa/
//!   mine/ machine/   the person's and this machine's knowledge
//!   ours             where the team's knowledge is checked out
//!   drafter          the command intake drafts with, when set
//!   roots            where ohjaa looks for repositories
//!   applied.toml     what apply changed, for `apply --undo`
//!   .git/            the home's history: a local repository, never pushed,
//!                    one commit per decision on mine or machine and per
//!                    apply ([`commit`])
//! ```
//!
//! A repository is named by its remote: `git@github.com:o/r.git`,
//! `https://github.com/o/r` and `ssh://git@github.com/o/r.git` are all
//! `github.com/o/r` ([`RepoId`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::knowledge::config_home;
use crate::process;

/// `$XDG_CONFIG_HOME/ohjaa`, else `~/.config/ohjaa`.
pub fn ohjaa_home() -> Result<PathBuf> {
    Ok(config_home()?.join("ohjaa"))
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

/// How long a change to the home waits for another ohjaa's to finish.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The home's repository folders, from the file system: where its own
/// files go (`git dir`) and where git reads its excludes (`common dir`).
/// `.git` itself, or, when `.git` is a file (a worktree, a submodule), the
/// folder it names and, for a worktree, the common folder that one names.
fn repository_dirs(home: &Path) -> (PathBuf, PathBuf) {
    let dot_git = home.join(".git");
    let named = |base: &Path, file: &Path, prefix: &str| -> Option<PathBuf> {
        let text = std::fs::read_to_string(file).ok()?;
        let named = text
            .trim()
            .strip_prefix(prefix)
            .unwrap_or(text.trim())
            .trim();
        (!named.is_empty()).then(|| base.join(named))
    };
    let Some(git_dir) = dot_git
        .is_file()
        .then(|| named(home, &dot_git, "gitdir:"))
        .flatten()
    else {
        return (dot_git.clone(), dot_git);
    };
    let common = named(&git_dir, &git_dir.join("commondir"), "").unwrap_or_else(|| git_dir.clone());
    (git_dir, common)
}

/// The home's lock, in its repository's own folder (made, when missing,
/// before the repository is), never in the files the history holds: one
/// ohjaa at a time changes the home and commits it, so each decision, apply
/// and adopt is one commit holding its own change, and the agents' block is
/// rendered from what was just decided. A change that has to wait says so;
/// one that cannot take it in time says that, and goes on without it:
/// nothing waits on another ohjaa for good.
pub fn lock(home: &Path) -> Option<process::FileLock> {
    let path = repository_dirs(home).0.join("ohjaa.lock");
    let taken = process::lock_file(&path, std::time::Duration::ZERO).or_else(|_| {
        eprintln!(
            "ohjaa: waiting for another ohjaa's change to the home ({}) to finish",
            path.display()
        );
        process::lock_file(&path, LOCK_WAIT)
    });
    match taken {
        Ok(lock) => Some(lock),
        Err(why) => {
            // Its holder is another ohjaa, or the git that one runs here.
            let why = if why.contains("held by another") {
                format!(
                    "{} is still held after {}s, by another ohjaa's change to the home (that \
                     ohjaa, or the git it runs there)",
                    path.display(),
                    LOCK_WAIT.as_secs()
                )
            } else {
                why
            };
            eprintln!(
                "ohjaa: warning: {why}; going on without the home's lock, so this change may \
                 share a commit with another"
            );
            None
        }
    }
}

const EXCLUDE_BEGIN: &str = "# >>> ohjaa: the home's history holds only what ohjaa keeps here";
const EXCLUDE_END: &str = "# <<< ohjaa";
/// Everything is left out, then what ohjaa keeps in the home is let in
/// (the knowledge scopes, the roots, apply's record, and the `ours` and
/// `drafter` settings), and what an
/// editor, the Finder or a half-done write leaves among those is left out
/// again.
const EXCLUDE: &[&str] = &[
    "/*",
    "!/mine/",
    "!/machine/",
    "!/roots",
    "!/applied.toml",
    "!/ours",
    "!/drafter",
    "*.tmp",
    ".*.ohjaa-*",
    ".*.sw?",
    "*~",
    ".#*",
    "\\#*#",
    ".DS_Store",
];

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// `now` with the allowlist's marked block taken out (and the newline after
/// it), and where it was; `None` when there is none.
fn without_allowlist(now: &str) -> Option<(String, usize)> {
    match (now.find(EXCLUDE_BEGIN), now.find(EXCLUDE_END)) {
        (Some(start), Some(end)) if end > start => {
            let after = &now[end + EXCLUDE_END.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            Some((format!("{}{after}", &now[..start]), start))
        }
        _ => None,
    }
}

/// The allowlist, as a marked block beside anything else in the file git
/// reads it from, kept untracked so the home needs no file of its own: the
/// home's repository's own `info/exclude`, which outranks the person's own
/// excludes file, read below it. A home that is a linked worktree keeps it in
/// its own worktree folder instead (`ohjaa.exclude`), and that file is
/// returned, to give `git add` as its excludes file: git reads the common
/// folder's `info/exclude` for every worktree of the repository, and the
/// allowlist begins by leaving out everything. That file holds the person's
/// own excludes (`person`, read for each commit) and then the allowlist: as
/// the last pattern that matches decides, the allowlist decides wherever it
/// matches, and the person's patterns everywhere else, as they would below
/// `info/exclude`. A block an earlier ohjaa wrote in the shared file is
/// taken out.
fn keep_out_strays(
    home: &Path,
    person: impl FnOnce() -> Result<String>,
) -> Result<Option<PathBuf>> {
    let (git_dir, common) = repository_dirs(home);
    let shared = common.join("info").join("exclude");
    let block = format!("{EXCLUDE_BEGIN}\n{}\n{EXCLUDE_END}\n", EXCLUDE.join("\n"));
    if git_dir != common {
        let now = read_or_empty(&shared)?;
        if let Some((without, _)) = without_allowlist(&now) {
            write_file(&shared, &without, None)?;
        }
        let path = git_dir.join("ohjaa.exclude");
        let mut person = person()?;
        if !person.is_empty() && !person.ends_with('\n') {
            person.push('\n');
        }
        let new = format!(
            "# Written by ohjaa for each commit in its home: the person's own excludes, then\n\
             # the allowlist, which decides wherever it matches.\n{person}{block}"
        );
        if read_or_empty(&path)? != new {
            write_file(&path, &new, None)?;
        }
        return Ok(Some(path));
    }
    let now = read_or_empty(&shared)?;
    let new = match without_allowlist(&now) {
        Some((without, at)) => format!("{}{block}{}", &without[..at], &without[at..]),
        None if now.is_empty() || now.ends_with('\n') => format!("{now}{block}"),
        None => format!("{now}\n{block}"),
    };
    if new != now {
        write_file(&shared, &new, None)?;
    }
    Ok(None)
}

/// The person's own excludes file, as git finds it for the home: its
/// `core.excludesFile` (from any of git's configs), else git's default,
/// `$XDG_CONFIG_HOME/git/ignore`, else `~/.config/git/ignore`; its text, or
/// nothing when there is none. One git process.
fn person_excludes(home: &Path) -> Result<String> {
    let args = ["config", "--path", "--get", "core.excludesFile"];
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(home.join(".git"))
        .args(args)
        .current_dir(home);
    for var in REPOSITORY_VARS {
        command.env_remove(var);
    }
    let file = match value_of(&mut command, &args)? {
        Some(file) => Some(PathBuf::from(file)),
        None => std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .map(|dir| dir.join("git").join("ignore")),
    };
    match file {
        Some(file) => read_or_empty(&file),
        None => Ok(String::new()),
    }
}

/// The variables that would point git at another repository than the home.
const REPOSITORY_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// What the home's git is run with, every time, so that nothing it can start
/// beside itself outlives it: the home's lock is held by that git
/// ([`home_git`]), and a process left running would hold up every later
/// change to the home. Nothing here changes what is committed.
const HOME_GIT: &[&str] = &[
    // No signing: gpg starts its agent, which lives on, and a prompt must
    // not hold up a decision. Nothing verifies a local history.
    "commit.gpgsign=false",
    // No fsmonitor: git's own daemon (`core.fsmonitor=true`) detaches and
    // lives on, and a command there is run with git's stdin, the lock.
    "core.fsmonitor=false",
    // No hook from a hooks folder: the home's own, one a template copied,
    // or one git's config names. (The home is made with no template.)
    "core.hooksPath=/dev/null",
    // No automatic gc or maintenance, which run detached by default
    // (gc.autoDetach, maintenance.autoDetach). Turned off, not kept in the
    // foreground: a history this small never needs either, and a run in the
    // foreground would hold the lock for as long as it takes.
    "gc.auto=0",
    "maintenance.auto=false",
    // No hook from git's config (`hook.<name>.command`) for any event an
    // init, an add or a commit fires, where git has this switch (after
    // 2.54). An older git runs such a hook with /dev/null as its stdin, so
    // nothing it leaves holds the lock.
    "hook.pre-commit.enabled=false",
    "hook.prepare-commit-msg.enabled=false",
    "hook.commit-msg.enabled=false",
    "hook.post-commit.enabled=false",
    "hook.post-index-change.enabled=false",
    "hook.reference-transaction.enabled=false",
];

/// `git <args>` in the home, with [`HOME_GIT`]; with `repository`, on the
/// home's own repository whatever git would find from there (an enclosing
/// dotfiles repository, say). It holds the home's lock too, when ohjaa
/// holds it, as its stdin ([`process::FileLock::as_stdin`]): a git that
/// outlives a killed ohjaa still commits under it, and what git starts with
/// a stdin of its own does not hold it.
fn home_git(
    home: &Path,
    args: &[&str],
    repository: bool,
    lock: Option<&process::FileLock>,
) -> Result<()> {
    let mut command = Command::new("git");
    if repository {
        command
            .arg("--git-dir")
            .arg(home.join(".git"))
            .arg("--work-tree")
            .arg(home);
    }
    for setting in HOME_GIT {
        command.args(["-c", setting]);
    }
    command.args(args).current_dir(home);
    for var in REPOSITORY_VARS {
        command.env_remove(var);
    }
    let output = match lock {
        Some(lock) => {
            let stdin = lock.as_stdin().context("cannot give git the home's lock")?;
            process::run_with_stdin(&mut command, stdin, process::LOCAL)
        }
        None => process::run(&mut command, None, process::LOCAL),
    }
    .context("cannot run git")?;
    if !output.status.success() {
        // Named by its subcommand, after any `-c <setting>`.
        let mut named = args;
        while let ["-c", _, rest @ ..] = named {
            named = rest;
        }
        bail!(
            "git {} failed: {}",
            named.first().copied().unwrap_or_default(),
            output.failure_detail()
        );
    }
    Ok(())
}

/// The home's history: what ohjaa keeps in the home (the allowlist in its
/// `info/exclude`, never a stray file), committed as one commit with
/// `message`, under the home's [`lock`], which the caller holds and passes
/// (`None` when it went on without it), so each git here holds it too. The home is
/// a local git repository (no remote; ohjaa never pushes), made by its first
/// commit unless the home already is one. A
/// commit is the person's, with the identity any commit of theirs takes from
/// git's config, and never signed: nothing verifies a local history, and a
/// signing prompt must not hold up a decision. Two git processes (`add -A`,
/// `commit`), and `init` once before the first; a history this small never
/// needs git's maintenance. A failure is the caller's to say; what was
/// changed stands either way.
pub fn commit(home: &Path, message: &str, lock: Option<&process::FileLock>) -> Result<()> {
    let dot_git = home.join(".git");
    // A `.git` folder without HEAD holds only the home's lock: no repository yet.
    if !dot_git.is_file() && !dot_git.join("HEAD").exists() {
        std::fs::create_dir_all(home)
            .with_context(|| format!("cannot create {}", home.display()))?;
        // No template (`init.templateDir`): no hook is copied in.
        home_git(home, &["init", "-q", "--template="], false, lock)?;
    }
    let excludes = keep_out_strays(home, || person_excludes(home))?
        .map(|file| format!("core.excludesFile={}", file.display()));
    let mut add: Vec<&str> = Vec::new();
    if let Some(excludes) = &excludes {
        add.extend(["-c", excludes]);
    }
    add.extend(["add", "-A"]);
    home_git(home, &add, true, lock)?;
    home_git(
        home,
        &["commit", "-q", "--allow-empty", "-m", message],
        true,
        lock,
    )
}

/// [`commit`], said by `who`: what was committed on stdout, or on stderr why
/// nothing was. Never an error: what was changed stands either way.
pub fn commit_and_say(home: &Path, message: &str, who: &str, lock: Option<&process::FileLock>) {
    let subject = message.lines().next().unwrap_or_default();
    match commit(home, message, lock) {
        Ok(()) => println!("{who}: committed in {}: {subject}", home.display()),
        Err(error) => eprintln!(
            "{who}: warning: \"{subject}\" stands, but is not in the history of {}: {error:#}; \
             the next commit there takes it in",
            home.display()
        ),
    }
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

/// `path` made absolute against the current directory, never resolved.
pub fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
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

/// The first `name` on PATH, as the shell finds it: made absolute, never
/// resolved, so `~/.cargo/bin/ohjaa` stays that and a reinstall keeps the
/// launcher line working.
pub fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| crate::gitconfig::executable(path))
        .map(|path| std::path::absolute(&path).unwrap_or(path))
}

/// `path` quoted for a POSIX shell.
pub fn quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
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

    /// A home whose `.git` is a file (a linked worktree of another
    /// repository): the lock goes in the folder it names, and so does the
    /// allowlist, since the common folder's `info/exclude` is read for every
    /// worktree of that repository (#65's re-review); a block an earlier
    /// ohjaa left there is taken out, and the person's own lines stay.
    /// Neither is ever in the files the history holds.
    #[test]
    fn a_home_whose_git_is_a_file_keeps_its_lock_and_allowlist_out_of_its_files() {
        let dir = std::env::temp_dir().join(format!("ohjaa-gitfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (home, store) = (dir.join("home"), dir.join("store"));
        std::fs::create_dir_all(store.join("worktrees/home")).unwrap();
        std::fs::create_dir_all(store.join("info")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".git"), "gitdir: ../store/worktrees/home\n").unwrap();
        std::fs::write(store.join("worktrees/home/commondir"), "../..\n").unwrap();
        let earlier = format!("{EXCLUDE_BEGIN}\n{}\n{EXCLUDE_END}\n", EXCLUDE.join("\n"));
        std::fs::write(
            store.join("info/exclude"),
            format!("# the person's\n*.log\n{earlier}build/\n"),
        )
        .unwrap();
        let (git_dir, common) = repository_dirs(&home);
        assert_eq!(git_dir, home.join("../store/worktrees/home"));
        assert_eq!(common, home.join("../store/worktrees/home").join("../.."));
        let lock = lock(&home).expect("the lock");
        assert!(store.join("worktrees/home/ohjaa.lock").is_file());
        drop(lock);
        for _ in 0..2 {
            let given = keep_out_strays(&home, || Ok("*.orig".to_owned())).unwrap();
            assert_eq!(given, Some(git_dir.join("ohjaa.exclude")));
        }
        let shared = std::fs::read_to_string(store.join("info/exclude")).unwrap();
        assert_eq!(shared, "# the person's\n*.log\nbuild/\n");
        // The person's own excludes, then the allowlist, which decides
        // wherever it matches.
        let exclude = std::fs::read_to_string(store.join("worktrees/home/ohjaa.exclude")).unwrap();
        assert_eq!(exclude.matches(EXCLUDE_BEGIN).count(), 1, "{exclude}");
        let (person, allowlist) = exclude.split_once(EXCLUDE_BEGIN).unwrap();
        assert!(person.ends_with("\n*.orig\n"), "{exclude}");
        assert!(allowlist.trim_end().ends_with(EXCLUDE_END), "{exclude}");
        let names: Vec<_> = std::fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [".git"], "nothing of ohjaa's among the home's files");
        // A plain home: both are its `.git`.
        let plain = dir.join("plain");
        assert_eq!(
            repository_dirs(&plain),
            (plain.join(".git"), plain.join(".git"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
