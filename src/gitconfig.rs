//! Which repositories run ohjaa's hooks: those whose remote names an entry,
//! and no other, each with its entry's own hooks folder.
//!
//! Git's global config gets one stable line, `[include] path =
//! <home>/git/config`. That file, which ohjaa regenerates from `repos/` on
//! `apply` and `adopt`, holds `includeIf "hasconfig:remote.*.url:…"`
//! sections per remote pattern of each entry, each including the entry's own
//! `repos/<o>/<r>/gated.config`, which sets `core.hooksPath` to the entry's
//! own `hooks/`: pre-push, and only the hooks that entry's clones have of
//! their own. A repository with no matching remote (a Homebrew tap, Cargo's
//! git cache, a personal project) never starts ohjaa, and a commit starts it
//! only where the repository has a commit hook of its own. Needs git 2.36.
//!
//! The patterns are git's path globs over the remote URL *as configured*
//! (before `insteadOf`). They are host-agnostic and case-insensitive: a
//! pattern that misses lets a push past the gate, while one that matches too
//! much costs one process that finds no entry and passes. `ohjaa doctor`
//! checks, clone by clone, that git really resolves `core.hooksPath` to the
//! entry's hooks.
//!
//! Facts are read once and passed down: the global config in one `git config
//! --global --includes -z --list --show-origin`, each clone's in one `git
//! config -z --list` (remotes, `insteadOf`, `core.hooksPath`), and the common
//! directory from the repository's layout. `insteadOf` is applied here, the
//! way git applies it, instead of asking git once per remote.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::home::{self, RepoId};
use crate::hook::{common_dir_in, HOOK_NAMES, NOT_DISPATCHED};
use crate::knowledge::{config_home, expand_home};
use crate::process;

/// `includeIf "hasconfig:remote.*.url:…"` arrived in git 2.36.
pub const MIN_GIT: (u32, u32) = (2, 36);

pub fn git_dir(home: &Path) -> PathBuf {
    home.join("git")
}

/// The file git's global config includes.
pub fn include_file(home: &Path) -> PathBuf {
    git_dir(home).join("config")
}

/// An entry's own hooks folder.
pub fn entry_hooks(entry: &Path) -> PathBuf {
    entry.join("hooks")
}

/// What a repository naming the entry includes: `core.hooksPath`.
pub fn entry_gated(entry: &Path) -> PathBuf {
    entry.join("gated.config")
}

/// The environment a git run of ohjaa's own must not inherit from a hook
/// that started it: it would point git at another repository.
pub const REPO_ENV: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
];

/// The commit a checkout's HEAD names, and its branch (`None` detached), from
/// git's files: the checkout's own git dir (`.git`, or the one its `.git`
/// file names) holds HEAD, the common dir its branch. `None` when the files
/// do not say.
pub fn read_head(top: &Path, common: &Path) -> Option<(String, Option<String>)> {
    let dot_git = top.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        top.join(text.strip_prefix("gitdir:")?.trim())
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(reference) => {
            let reference = reference.trim();
            let commit = read_ref(common, reference)?;
            let branch = reference.strip_prefix("refs/heads/").map(str::to_owned);
            Some((commit, branch))
        }
        None => (head.len() >= 40 && head.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| (head.to_owned(), None)),
    }
}

/// Where `name` (`refs/remotes/origin/main`) points in the clone whose
/// common git dir is `common`, read from git's files without asking git: its
/// loose file, else its line in `packed-refs`. `None` when neither holds it
/// (or the clone keeps its refs in a reftable): what cannot be read is
/// never guessed.
pub fn read_ref(common: &Path, name: &str) -> Option<String> {
    let hex = |text: &str| {
        let text = text.trim();
        (text.len() >= 40 && text.chars().all(|c| c.is_ascii_hexdigit())).then(|| text.to_owned())
    };
    if let Ok(text) = std::fs::read_to_string(common.join(name)) {
        return hex(&text);
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (oid, reference) = line.split_once(' ')?;
        (reference == name).then(|| hex(oid)).flatten()
    })
}

/// Git that reads no config but what it is given: git reads the global
/// config before anything else, even for `--version`, so one unreadable
/// included file would otherwise stop it from saying so.
fn isolated_git(args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir("/")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for var in REPO_ENV {
        command.env_remove(var);
    }
    command
}

/// `git --version` as (major, minor).
pub fn git_version() -> Result<(u32, u32)> {
    let text =
        home::value_of(&mut isolated_git(&["--version"]), &["--version"])?.unwrap_or_default();
    parse_git_version(&text).with_context(|| format!("cannot read git's version from {text:?}"))
}

fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let version = text.split_whitespace().nth(2)?;
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Refuse a git too old for `hasconfig`.
pub fn require_git() -> Result<(u32, u32)> {
    let version = git_version()?;
    if version < MIN_GIT {
        bail!(
            "git {}.{} cannot scope hooks by remote (includeIf hasconfig needs {}.{}); \
             update git",
            version.0,
            version.1,
            MIN_GIT.0,
            MIN_GIT.1
        );
    }
    Ok(version)
}

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

    /// `url.<base>.<kind>` rules (`insteadof` or `pushinsteadof`).
    fn rules(&self, kind: &str) -> Vec<Rewrite> {
        let suffix = format!(".{kind}");
        self.0
            .iter()
            .filter_map(|setting| {
                let base = setting.key.strip_prefix("url.")?.strip_suffix(&suffix)?;
                Some(Rewrite {
                    base: base.to_owned(),
                    prefix: setting.value.clone(),
                })
            })
            .collect()
    }

    pub fn rewrites(&self) -> Vec<Rewrite> {
        self.rules("insteadof")
    }

    /// Every URL the remotes name, before and after git's rewriting: each
    /// `url` as written, with `insteadOf`, and with `pushInsteadOf` (which
    /// applies when the remote has no `pushurl`); each `pushurl` as written
    /// and with `insteadOf`.
    pub fn remote_urls(&self) -> Vec<String> {
        let instead = self.rewrites();
        let push_instead = self.rules("pushinsteadof");
        let remote = |setting: &Setting, field: &str| {
            let rest = setting.key.strip_prefix("remote.")?;
            rest.strip_suffix(field).map(str::to_owned)
        };
        let with_pushurl: BTreeSet<String> = self
            .0
            .iter()
            .filter_map(|setting| remote(setting, ".pushurl"))
            .collect();
        let mut urls: Vec<String> = Vec::new();
        let mut add = |url: String| {
            if !urls.contains(&url) {
                urls.push(url);
            }
        };
        for setting in &self.0 {
            if let Some(name) = remote(setting, ".url") {
                add(setting.value.clone());
                add(rewrite(&setting.value, &instead));
                if !with_pushurl.contains(&name) {
                    add(rewrite(&setting.value, &push_instead));
                }
            } else if remote(setting, ".pushurl").is_some() {
                add(setting.value.clone());
                add(rewrite(&setting.value, &instead));
            }
        }
        urls
    }

    /// `core.hooksPath` as it applies, `~` expanded.
    pub fn hooks_path(&self) -> Option<String> {
        self.last("core.hookspath")
            .filter(|setting| !setting.value.is_empty())
            .map(|setting| expand_home(&setting.value).to_string_lossy().into_owned())
    }
}

/// A remote `url.<base>.insteadOf = <prefix>` rule: a URL written as
/// `<prefix>rest` means `<base>rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub base: String,
    pub prefix: String,
}

/// `url` as git rewrites it: the longest matching prefix wins.
pub fn rewrite(url: &str, rules: &[Rewrite]) -> String {
    rules
        .iter()
        .filter(|rule| !rule.prefix.is_empty() && url.starts_with(&rule.prefix))
        .max_by_key(|rule| rule.prefix.len())
        .map_or_else(
            || url.to_owned(),
            |rule| format!("{}{}", rule.base, &url[rule.prefix.len()..]),
        )
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

/// The file `git config --global` writes: `$GIT_CONFIG_GLOBAL`, else
/// `~/.gitconfig`, unless only `$XDG_CONFIG_HOME/git/config` exists.
pub fn global_write_target() -> Result<PathBuf> {
    let files = global_files()?;
    if files.len() == 2 && !files[0].exists() && files[1].exists() {
        return Ok(files[1].clone());
    }
    Ok(files[0].clone())
}

/// Git's global config, read once: every setting, includes followed, with
/// the file each comes from. Read outside every repository, so no
/// repository's `includeIf` (ohjaa's own among them) counts as global.
#[derive(Debug, Default)]
pub struct GlobalFacts {
    pub listing: Listing,
    files: Vec<PathBuf>,
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
        Ok(GlobalFacts { listing, files })
    }

    /// Whether a setting comes from the global file itself, not from a file
    /// it includes: only that is ohjaa's to change.
    fn direct(&self, setting: &Setting) -> bool {
        setting
            .origin
            .as_deref()
            .and_then(|origin| origin.strip_prefix("file:"))
            .is_some_and(|file| {
                self.files
                    .iter()
                    .any(|f| home::same_path(f, Path::new(file)))
            })
    }

    /// `core.hooksPath`: its value (`~` expanded), whether the global file
    /// itself sets it, and where it comes from.
    pub fn hooks_path(&self) -> Option<(String, bool, String)> {
        let setting = self
            .listing
            .last("core.hookspath")
            .filter(|setting| !setting.value.is_empty())?;
        Some((
            expand_home(&setting.value).to_string_lossy().into_owned(),
            self.direct(setting),
            setting
                .origin
                .clone()
                .unwrap_or_default()
                .trim_start_matches("file:")
                .to_owned(),
        ))
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

    /// Whether the global file itself includes `path`.
    pub fn includes(&self, path: &Path) -> bool {
        self.listing.0.iter().any(|setting| {
            setting.key == "include.path"
                && self.direct(setting)
                && home::same_path(&expand_home(&setting.value), path)
        })
    }

    pub fn rewrites(&self) -> Vec<Rewrite> {
        self.listing.rewrites()
    }
}

/// One glob character that must match itself: letters either case
/// (wildmatch is case-sensitive, owners and repositories are not), glob
/// syntax escaped.
fn glob_char(c: char, out: &mut String) {
    if c.is_ascii_alphabetic() {
        out.push('[');
        out.push(c.to_ascii_lowercase());
        out.push(c.to_ascii_uppercase());
        out.push(']');
    } else {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
}

fn any_case(text: &str) -> String {
    let mut out = String::new();
    text.chars().for_each(|c| glob_char(c, &mut out));
    out
}

fn literal(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The `hasconfig:remote.*.url` globs that name `owner/name` in any form git
/// takes: `**/<o>/<r>`, with `.git` and/or `/`, for URLs and paths;
/// `*:<o>/<r>` the same for scp-like ones (`git@host:o/r`); and
/// `<prefix><rest>` for each `insteadOf` rule whose base completes to the
/// repository.
pub fn patterns(owner: &str, name: &str, rewrites: &[Rewrite]) -> Vec<String> {
    let path = format!("{}/{}", any_case(owner), any_case(name));
    let mut found = Vec::new();
    for lead in ["**/", "*:"] {
        for tail in ["", ".git", "/", ".git/"] {
            found.push(format!("{lead}{path}{tail}"));
        }
    }
    for rewrite in rewrites {
        // What follows the prefix: the name or owner/name, with or without
        // `.git` (kept literal, as in the forms above).
        for (rest, glob) in [
            (name.to_owned(), any_case(name)),
            (format!("{name}.git"), format!("{}.git", any_case(name))),
            (format!("{owner}/{name}"), path.clone()),
            (format!("{owner}/{name}.git"), format!("{path}.git")),
        ] {
            // Ohjaa names a repository by the last two parts of its URL, so
            // a rule gates every short form that completes to owner/name.
            let names_it = RepoId::parse(&format!("{}{rest}", rewrite.base)).is_some_and(|id| {
                id.owner.eq_ignore_ascii_case(owner) && id.name.eq_ignore_ascii_case(name)
            });
            let pattern = format!("{}{glob}", literal(&rewrite.prefix));
            if names_it && !found.contains(&pattern) {
                found.push(pattern);
            }
        }
    }
    found
}

/// A value or subsection name as git's config reads it back.
fn config_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// An entry by name and directory.
#[derive(Debug, Clone)]
pub struct Named {
    pub owner: String,
    pub name: String,
    pub dir: PathBuf,
}

/// Every entry: each directory under `repos/` that holds a `repo.toml`, or
/// anything at all, readable or not. A broken entry still gates its
/// repository, so the dispatcher runs and refuses the push instead of git
/// never asking.
pub fn entry_names(home: &Path) -> Result<Vec<Named>> {
    let mut names = Vec::new();
    for (slug, dir) in home::entry_dirs(home)? {
        let holds_files = std::fs::read_dir(&dir).is_ok_and(|mut e| e.next().is_some());
        if holds_files {
            if let Some((owner, name)) = slug.split_once('/') {
                names.push(Named {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                    dir,
                });
            }
        }
    }
    Ok(names)
}

/// `<home>/git/config`: for each entry, its patterns, each including that
/// entry's `gated.config`.
pub fn include_text(entries: &[Named], rewrites: &[Rewrite]) -> String {
    let mut text = String::from(
        "# Written by `ohjaa apply` and `ohjaa adopt` from repos/; edits are replaced.\n\
         # A repository runs ohjaa's hooks only when one of its remotes matches here,\n\
         # and then its entry's own hooks folder.\n",
    );
    for entry in entries {
        text.push_str(&format!("\n# {}/{}\n", entry.owner, entry.name));
        let gated = entry_gated(&entry.dir);
        for pattern in patterns(&entry.owner, &entry.name, rewrites) {
            text.push_str(&format!(
                "[includeIf {}]\n\tpath = {}\n",
                config_quote(&format!("hasconfig:remote.*.url:{pattern}")),
                config_quote(&gated.to_string_lossy())
            ));
        }
    }
    text
}

/// `<entry>/gated.config`.
pub fn gated_text(entry: &Path) -> String {
    format!(
        "# Written by `ohjaa apply`: what a repository whose remote names this entry includes.\n\
         [core]\n\thooksPath = {}\n",
        config_quote(&entry_hooks(entry).to_string_lossy())
    )
}

/// The generated files as they should be now: `git/config` and each
/// entry's `gated.config`.
pub fn generated(home: &Path, rewrites: &[Rewrite]) -> Result<Vec<(PathBuf, String)>> {
    let entries = entry_names(home)?;
    let mut files = vec![(include_file(home), include_text(&entries, rewrites))];
    files.extend(
        entries
            .iter()
            .map(|entry| (entry_gated(&entry.dir), gated_text(&entry.dir))),
    );
    Ok(files)
}

/// That git reads `path` as config, and that it sets no `remote.*` key (git
/// refuses those in a file included by `hasconfig`, and every repository the
/// file applies to would then fail). A half-written included file breaks
/// every git command on the machine, so nothing unchecked is put in place.
pub fn check_git_config(path: &Path) -> Result<()> {
    let path_text = path.to_string_lossy();
    let args = ["config", "--file", &path_text, "--list"];
    let listing = home::value_of(&mut isolated_git(&args), &args)
        .with_context(|| format!("{} is not valid git config", path.display()))?
        .unwrap_or_default();
    if let Some(line) = listing.lines().find(|line| line.starts_with("remote.")) {
        bail!(
            "{} sets {line}, which an included file must not",
            path.display()
        );
    }
    Ok(())
}

/// Write a generated git config file, checked before it replaces the old.
pub fn write_generated(path: &Path, text: &str) -> Result<()> {
    home::write_checked(path, text, None, check_git_config)
}

/// The hooks git would run in a clone without a `core.hooksPath`: the
/// executable files in `<common dir>/hooks` named as git hooks (`.sample`
/// and `.disabled` files are not).
pub fn own_hooks_in(common: &Path) -> Vec<&'static str> {
    let dir = common.join("hooks");
    HOOK_NAMES
        .into_iter()
        .filter(|name| executable(&dir.join(name)))
        .collect()
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
    if let Some(common) = common_dir_in(repo, None, None) {
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

/// A clone's config, all scopes and includes, in one process.
pub fn clone_listing(repo: &Path) -> Result<Listing> {
    let args = ["config", "-z", "--list"];
    let mut command = Command::new("git");
    command.args(args).current_dir(repo);
    for var in REPO_ENV {
        command.env_remove(var);
    }
    match listing_of(command, &args)? {
        Ok(text) => Ok(Listing::parse(&text, false)),
        Err(why) => bail!("git config --list in {} failed: {why}", repo.display()),
    }
}

/// A clone under the roots whose remote names an entry.
#[derive(Debug)]
pub struct Gated {
    pub path: PathBuf,
    pub slugs: Vec<String>,
    /// The entries its remotes name, by directory.
    pub entries: Vec<PathBuf>,
    pub own: Vec<&'static str>,
    pub hooks_path: Option<String>,
}

/// The entries the remotes in `listing` name, as (slug, directory). A
/// broken entry counts: git/config gates its repository all the same.
fn entries_named(home: &Path, listing: &Listing) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for url in listing.remote_urls() {
        let Some(id) = RepoId::parse(&url) else {
            continue;
        };
        let dir = match home::find_entry(home, &id) {
            Ok(None) => continue,
            Ok(Some(entry)) => entry.dir,
            Err(_) => home::entry_dir(home, &id),
        };
        if !found.iter().any(|(_, known)| known == &dir) {
            found.push((id.slug(), dir));
        }
    }
    found
}

/// The clone at `repo`, if its remotes name an entry: one git process (its
/// config), and its layout.
pub fn gated_clone(home: &Path, repo: &Path) -> Result<Option<Gated>> {
    let listing = clone_listing(repo)?;
    gated_from(home, repo, &common_dir(repo)?, &listing)
}

/// The clone at `repo`, if the remotes in its config `listing` name an entry.
fn gated_from(home: &Path, repo: &Path, common: &Path, listing: &Listing) -> Result<Option<Gated>> {
    let entries = entries_named(home, listing);
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(Gated {
        path: repo.to_path_buf(),
        slugs: entries.iter().map(|(slug, _)| slug.clone()).collect(),
        entries: entries.into_iter().map(|(_, dir)| dir).collect(),
        own: own_hooks_in(common),
        hooks_path: listing.hooks_path(),
    }))
}

/// Every gated clone among `repos`. With no entry at all, none is read.
pub fn gated_clones(home: &Path, repos: &[PathBuf]) -> Result<Vec<Gated>> {
    if entry_names(home)?.is_empty() {
        return Ok(Vec::new());
    }
    let mut gated = Vec::new();
    for repo in repos {
        if let Some(clone) = gated_clone(home, repo)? {
            gated.push(clone);
        }
    }
    Ok(gated)
}

/// The dispatchers each entry needs: pre-push, and every hook a clone that
/// names it has of its own (never one of [`NOT_DISPATCHED`]).
pub fn needed_hooks(
    home: &Path,
    gated: &[Gated],
) -> Result<BTreeMap<PathBuf, BTreeSet<&'static str>>> {
    let mut needed: BTreeMap<PathBuf, BTreeSet<&'static str>> = entry_names(home)?
        .into_iter()
        .map(|entry| (entry.dir, BTreeSet::from(["pre-push"])))
        .collect();
    for clone in gated {
        for entry in &clone.entries {
            needed.entry(entry.clone()).or_default().extend(
                clone
                    .own
                    .iter()
                    .copied()
                    .filter(|name| !NOT_DISPATCHED.contains(name)),
            );
        }
    }
    Ok(needed)
}

/// What is wrong with an observed clone: (a) git does not resolve its
/// `core.hooksPath` to the hooks of an entry it names, so its pushes are not
/// observed; (b) a hook of its own has no dispatcher there, so git skips it.
pub fn clone_drift(home: &Path, clone: &Gated) -> Vec<String> {
    let mut drift = Vec::new();
    let routed = clone.hooks_path.as_deref().and_then(|path| {
        clone
            .entries
            .iter()
            .map(|entry| entry_hooks(entry))
            .find(|hooks| home::same_path(Path::new(path), hooks))
    });
    match (&clone.hooks_path, &routed) {
        (_, Some(_)) => {}
        (Some(path), None) => drift.push(format!(
            "its own core.hooksPath ({path}) overrides ohjaa's: its pushes are not \
             observed here: the repo's own hooks run"
        )),
        (None, None) => drift.push(format!(
            "git does not include {} for it: no pattern there matches its remotes, or \
             `ohjaa apply` has not run; its pushes are not observed (ohjaa apply)",
            include_file(home).display()
        )),
    }
    let Some(hooks) = routed else {
        return drift;
    };
    let (never, own): (Vec<&str>, Vec<&str>) = clone
        .own
        .iter()
        .copied()
        .partition(|name| NOT_DISPATCHED.contains(name));
    if !never.is_empty() {
        drift.push(format!(
            "its own {} cannot run under ohjaa's hooks folder (a dispatcher of that name \
             would change what git does in every repository of the entry), so git skips it",
            never.join(", ")
        ));
    }
    let missing: Vec<&str> = own
        .into_iter()
        .filter(|name| !hooks.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        drift.push(format!(
            "its own {} hook{} {} no dispatcher in {}, so git skips {} (ohjaa apply)",
            missing.join(", "),
            if missing.len() == 1 { "" } else { "s" },
            if missing.len() == 1 { "has" } else { "have" },
            hooks.display(),
            if missing.len() == 1 { "it" } else { "them" }
        ));
    }
    drift
}

/// For `ohjaa run` and `ohjaa brief`: the same checks for the repository a
/// session starts in, and the gate origin's entry gives it. Nothing at all
/// without an entry; else one git process (its config) and the layout.
pub fn launch_view(home: &Path, repo: &Path) -> (Vec<String>, Option<crate::lifecycle::Gate>) {
    if entry_names(home).map_or(true, |entries| entries.is_empty()) {
        return (Vec::new(), None);
    }
    let cannot = |error: anyhow::Error| {
        vec![format!(
            "cannot tell whether ohjaa observes {}: {error:#}",
            repo.display()
        )]
    };
    let read = common_dir(repo).and_then(|common| Ok((clone_listing(repo)?, common)));
    let (listing, common) = match read {
        Ok(read) => read,
        Err(error) => return (cannot(error), None),
    };
    let warnings = match gated_from(home, repo, &common, &listing) {
        Ok(Some(clone)) => clone_drift(home, &clone)
            .into_iter()
            .map(|drift| format!("{} ({}): {drift}", repo.display(), clone.slugs.join(", ")))
            .collect(),
        Ok(None) => Vec::new(),
        Err(error) => cannot(error),
    };
    (
        warnings,
        crate::mark::gate_of(home, repo, &common, &listing),
    )
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
                if let Some(common) = common_dir_in(dir, None, None) {
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

/// The URL `git push <remote>` pushes to, as git works it out: the
/// `pushurl` with `insteadOf`, else the `url` with `pushInsteadOf`, else with
/// `insteadOf`.
pub fn push_url(listing: &Listing, remote: &str) -> Option<String> {
    let rewrites = listing.rewrites();
    if let Some(push) = listing.last(&format!("remote.{remote}.pushurl")) {
        return Some(rewrite(&push.value, &rewrites));
    }
    let url = &listing.last(&format!("remote.{remote}.url"))?.value;
    let push_rules: Vec<Rewrite> = listing
        .0
        .iter()
        .filter_map(|setting| {
            let base = setting
                .key
                .strip_prefix("url.")?
                .strip_suffix(".pushinsteadof")?;
            Some(Rewrite {
                base: base.to_owned(),
                prefix: setting.value.clone(),
            })
        })
        .collect();
    let pushed = rewrite(url, &push_rules);
    Some(if &pushed != url {
        pushed
    } else {
        rewrite(url, &rewrites)
    })
}

/// The URL `git fetch <remote>` uses: its `url` with `insteadOf`.
pub fn fetch_url(listing: &Listing, remote: &str) -> Option<String> {
    let url = &listing.last(&format!("remote.{remote}.url"))?.value;
    Some(rewrite(url, &listing.rewrites()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_versions_are_read() {
        assert_eq!(
            parse_git_version("git version 2.54.0 (Apple Git-157)"),
            Some((2, 54))
        );
        assert_eq!(
            parse_git_version("git version 2.36.0.windows.1"),
            Some((2, 36))
        );
        assert_eq!(parse_git_version("nonsense"), None);
        assert!((2, 35) < MIN_GIT && (2, 36) >= MIN_GIT && (3, 0) > MIN_GIT);
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
        assert_eq!(
            global.rewrites(),
            [Rewrite {
                base: "git@github.com:".into(),
                prefix: "gh:".into()
            }]
        );
        let clone = Listing::parse("core.hookspath\n.githooks\0core.bare\nfalse\0", false);
        assert_eq!(clone.hooks_path().as_deref(), Some(".githooks"));
    }

    #[test]
    fn remote_urls_are_rewritten_the_way_git_does() {
        let listing = Listing::parse(
            "url.git@github.com:.insteadof\ngh:\0\
             url.git@github.com:acme/.insteadof\ngh:acme/\0\
             url.ssh://push.example/.pushinsteadof\nhttps://h/\0\
             remote.origin.url\ngh:acme/w\0\
             remote.up.url\nhttps://h/o/r\0\
             remote.fork.url\nhttps://h/me/r\0\
             remote.fork.pushurl\ngh:o/p\0",
            false,
        );
        let urls = listing.remote_urls();
        for url in [
            "gh:acme/w",
            // The longest prefix wins.
            "git@github.com:acme/w",
            "https://h/o/r",
            "ssh://push.example/o/r",
            "gh:o/p",
            "git@github.com:o/p",
        ] {
            assert!(urls.iter().any(|u| u == url), "{url} in {urls:?}");
        }
        // A remote with a pushurl is not pushed to through pushInsteadOf.
        assert!(
            !urls.iter().any(|u| u == "ssh://push.example/me/r"),
            "{urls:?}"
        );
    }

    #[test]
    fn patterns_cover_every_form_in_either_case() {
        let found = patterns("acme", "w-1", &[]);
        assert_eq!(
            found,
            [
                "**/[aA][cC][mM][eE]/[wW]-1",
                "**/[aA][cC][mM][eE]/[wW]-1.git",
                "**/[aA][cC][mM][eE]/[wW]-1/",
                "**/[aA][cC][mM][eE]/[wW]-1.git/",
                "*:[aA][cC][mM][eE]/[wW]-1",
                "*:[aA][cC][mM][eE]/[wW]-1.git",
                "*:[aA][cC][mM][eE]/[wW]-1/",
                "*:[aA][cC][mM][eE]/[wW]-1.git/",
            ]
        );
        assert_eq!(any_case("a*b"), "[aA]\\*[bB]");
    }

    #[test]
    fn insteadof_rules_add_the_short_forms_that_reach_the_repository() {
        let rewrites = Listing::parse(
            "url.git@github.com:.insteadof\ngh:\0\
             url.git@github.com:acme/.insteadof\nac:\0\
             url.https://elsewhere.example/other/.insteadof\nx:\0",
            false,
        )
        .rewrites();
        assert_eq!(rewrites.len(), 3);
        assert_eq!(rewrites[1].base, "git@github.com:acme/");
        let found = patterns("acme", "w", &rewrites);
        for short in [
            "gh:[aA][cC][mM][eE]/[wW]",
            "gh:[aA][cC][mM][eE]/[wW].git",
            "ac:[wW]",
            "ac:[wW].git",
            // x:acme/w is https://elsewhere.example/other/acme/w: acme/w.
            "x:[aA][cC][mM][eE]/[wW]",
        ] {
            assert!(found.iter().any(|p| p == short), "{short} in {found:?}");
        }
        // x:w is https://elsewhere.example/other/w: other/w, not acme/w.
        assert!(!found.iter().any(|p| p == "x:[wW]"), "{found:?}");
    }

    #[test]
    fn the_include_file_points_each_entry_at_its_own_gated_config() {
        let entries = [Named {
            owner: "acme".into(),
            name: "w".into(),
            dir: PathBuf::from("/h/ohjaa/repos/acme/w"),
        }];
        let text = include_text(&entries, &[]);
        assert!(text.contains(
            "[includeIf \"hasconfig:remote.*.url:**/[aA][cC][mM][eE]/[wW]\"]\n\tpath = \"/h/ohjaa/repos/acme/w/gated.config\"\n"
        ));
        assert_eq!(config_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert!(gated_text(Path::new("/h/ohjaa/repos/acme/w"))
            .contains("hooksPath = \"/h/ohjaa/repos/acme/w/hooks\""));
    }

    #[test]
    fn push_and_fetch_urls_are_worked_out_the_way_git_does() {
        let listing = Listing::parse(
            "url.git@github.com:.insteadof\ngh:\0\
             url.ssh://push.example/.pushinsteadof\nhttps://h/\0\
             remote.origin.url\ngh:acme/w\0\
             remote.up.url\nhttps://h/o/r\0\
             remote.fork.url\nhttps://h/me/r\0\
             remote.fork.pushurl\ngh:o/p\0",
            false,
        );
        assert_eq!(
            fetch_url(&listing, "origin").as_deref(),
            Some("git@github.com:acme/w")
        );
        assert_eq!(
            push_url(&listing, "origin").as_deref(),
            Some("git@github.com:acme/w")
        );
        assert_eq!(
            push_url(&listing, "up").as_deref(),
            Some("ssh://push.example/o/r")
        );
        assert_eq!(fetch_url(&listing, "up").as_deref(), Some("https://h/o/r"));
        assert_eq!(
            push_url(&listing, "fork").as_deref(),
            Some("git@github.com:o/p")
        );
        assert_eq!(push_url(&listing, "none"), None);
    }
}
