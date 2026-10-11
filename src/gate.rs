//! Which repositories run ohjaa's hooks: those whose remote names an entry,
//! and no other.
//!
//! Git's global config gets one stable line, `[include] path =
//! <home>/git/config`. That file, which ohjaa regenerates from `repos/` on
//! `apply` and `adopt`, holds one `includeIf "hasconfig:remote.*.url:…"`
//! section per remote pattern of each entry, each including
//! `<home>/git/gated.config`, which sets `core.hooksPath = <home>/hooks`. A
//! repository with no matching remote (a Homebrew tap, Cargo's git cache, a
//! personal project) never starts ohjaa. Needs git 2.36 (`hasconfig`).
//!
//! The patterns are git's path globs over the remote URL *as configured*
//! (before `insteadOf`). They are host-agnostic and case-insensitive: a
//! pattern that misses lets a push past the gate, while one that matches too
//! much costs one process that finds no entry and passes. `ohjaa doctor`
//! checks, clone by clone, that git really resolves `core.hooksPath` to the
//! home's hooks.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::home::{self, RepoId};
use crate::hook::{HOOK_NAMES, NOT_DISPATCHED};

/// `includeIf "hasconfig:remote.*.url:…"` arrived in git 2.36.
pub const MIN_GIT: (u32, u32) = (2, 36);

pub fn git_dir(home: &Path) -> PathBuf {
    home.join("git")
}

/// The file git's global config includes.
pub fn include_file(home: &Path) -> PathBuf {
    git_dir(home).join("config")
}

/// What a gated repository includes: `core.hooksPath`.
pub fn gated_file(home: &Path) -> PathBuf {
    git_dir(home).join("gated.config")
}

/// Git that reads no config but what it is given: git reads the global
/// config before anything else, even for `--version`, so one unreadable
/// included file would otherwise stop it from saying so.
fn isolated_git(args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir("/")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
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

/// One character of a glob that must match itself: letters either case
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

/// A remote `url.<base>.insteadOf = <prefix>` rule: a URL written as
/// `<prefix>rest` means `<base>rest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    pub base: String,
    pub prefix: String,
}

/// The `insteadOf` rules in git's global config.
pub fn global_rewrites() -> Result<Vec<Rewrite>> {
    Ok(parse_rewrites(&home::global_rewrites_text()?))
}

/// `git config -z --get-regexp` output: `key\nvalue\0` per entry.
fn parse_rewrites(text: &str) -> Vec<Rewrite> {
    text.split('\0')
        .filter_map(|item| {
            let (key, prefix) = item.split_once('\n')?;
            let base = key.strip_prefix("url.")?;
            let base = &base[..base.len().checked_sub(".insteadof".len())?];
            Some(Rewrite {
                base: base.to_owned(),
                prefix: prefix.to_owned(),
            })
        })
        .collect()
}

/// The `hasconfig:remote.*.url` globs that name `owner/name` in any form git
/// takes: `**/<o>/<r>`, with `.git` or `/`, for URLs and paths; `*:<o>/<r>`
/// for scp-like ones (`git@host:o/r`); and `<prefix><rest>` for each
/// `insteadOf` rule whose base completes to the repository.
pub fn patterns(owner: &str, name: &str, rewrites: &[Rewrite]) -> Vec<String> {
    let path = format!("{}/{}", any_case(owner), any_case(name));
    let mut found = vec![
        format!("**/{path}"),
        format!("**/{path}.git"),
        format!("**/{path}/"),
        format!("*:{path}"),
        format!("*:{path}.git"),
        format!("*:{path}/"),
    ];
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

/// `<home>/git/config`: for each entry, its patterns, each including
/// `gated.config`.
pub fn include_text(home: &Path, entries: &[(String, String)], rewrites: &[Rewrite]) -> String {
    let gated = gated_file(home);
    let mut text = String::from(
        "# Written by `ohjaa apply` and `ohjaa adopt` from repos/; edits are replaced.\n\
         # A repository runs ohjaa's hooks only when one of its remotes matches here.\n",
    );
    for (owner, name) in entries {
        text.push_str(&format!("\n# {owner}/{name}\n"));
        for pattern in patterns(owner, name, rewrites) {
            text.push_str(&format!(
                "[includeIf {}]\n\tpath = {}\n",
                config_quote(&format!("hasconfig:remote.*.url:{pattern}")),
                config_quote(&gated.to_string_lossy())
            ));
        }
    }
    text
}

/// `<home>/git/gated.config`.
pub fn gated_text(home: &Path) -> String {
    format!(
        "# Written by `ohjaa apply`: what a repository whose remote names an entry includes.\n\
         [core]\n\thooksPath = {}\n",
        config_quote(&home::hooks_dir(home).to_string_lossy())
    )
}

/// Every entry, as (owner, name): each directory under `repos/` that holds
/// anything, readable or not. A broken entry still gates its repository, so
/// the dispatcher runs and refuses the push instead of git never asking.
pub fn entry_names(home: &Path) -> Result<Vec<(String, String)>> {
    let mut names = Vec::new();
    for (slug, dir) in home::entry_dirs(home)? {
        if !matches!(home::read_entry(&dir), Ok(None)) {
            if let Some((owner, name)) = slug.split_once('/') {
                names.push((owner.to_owned(), name.to_owned()));
            }
        }
    }
    Ok(names)
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

/// The two generated files as they should be now.
pub fn generated(home: &Path) -> Result<[(PathBuf, String); 2]> {
    Ok([
        (gated_file(home), gated_text(home)),
        (
            include_file(home),
            include_text(home, &entry_names(home)?, &global_rewrites()?),
        ),
    ])
}

/// The URLs of a clone's remotes, after `insteadOf`, each once.
pub fn remote_urls(repo: &Path) -> Result<Vec<String>> {
    let text = home::git_value(&["remote", "-v"], Some(repo))?.unwrap_or_default();
    let mut urls: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(url) = line
            .split('\t')
            .nth(1)
            .map(|rest| rest.rsplit_once(" (").map_or(rest, |(url, _)| url))
        else {
            continue;
        };
        if !urls.iter().any(|known| known == url) {
            urls.push(url.to_owned());
        }
    }
    Ok(urls)
}

/// The entries a clone's remotes name, as `owner/name`. A broken entry
/// counts: git/config gates its repository all the same.
pub fn entries_of(home: &Path, repo: &Path) -> Result<Vec<String>> {
    let mut found: Vec<String> = Vec::new();
    for url in remote_urls(repo)? {
        let Some(id) = RepoId::parse(&url) else {
            continue;
        };
        if matches!(home::find_entry(home, &id), Ok(None)) {
            continue;
        }
        let slug = id.slug();
        if !found.iter().any(|known| known.eq_ignore_ascii_case(&slug)) {
            found.push(slug);
        }
    }
    Ok(found)
}

pub fn common_dir(repo: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        home::git_value(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            Some(repo),
        )?
        .context("git names no repository there")?,
    ))
}

/// The clone's own hooks git would run without a `core.hooksPath`: the
/// executable files in `<common dir>/hooks` named as git hooks (`.sample`
/// files are not).
pub fn own_hooks(repo: &Path) -> Result<Vec<&'static str>> {
    let dir = common_dir(repo)?.join("hooks");
    Ok(HOOK_NAMES
        .into_iter()
        .filter(|name| executable(&dir.join(name)))
        .collect())
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

/// `core.hooksPath` as git resolves it in the clone, includes and all.
pub fn effective_hooks_path(repo: &Path) -> Result<Option<String>> {
    home::git_value(
        &["config", "--type=path", "--get", "core.hooksPath"],
        Some(repo),
    )
}

/// A clone under the roots whose remote names an entry.
pub struct Gated {
    pub path: PathBuf,
    pub slugs: Vec<String>,
    pub own: Vec<&'static str>,
    pub hooks_path: Option<String>,
}

/// Every gated clone under `roots`, with what doctor and apply need of it.
pub fn gated_clones(home: &Path, roots: &[PathBuf]) -> Result<Vec<Gated>> {
    let mut gated = Vec::new();
    for repo in crate::roots::git_repos(roots)? {
        if let Some(clone) = gated_clone(home, &repo)? {
            gated.push(clone);
        }
    }
    Ok(gated)
}

pub fn gated_clone(home: &Path, repo: &Path) -> Result<Option<Gated>> {
    let slugs = entries_of(home, repo)?;
    if slugs.is_empty() {
        return Ok(None);
    }
    Ok(Some(Gated {
        path: repo.to_path_buf(),
        slugs,
        own: own_hooks(repo)?,
        hooks_path: effective_hooks_path(repo)?,
    }))
}

/// The dispatchers the home needs: pre-push, and every hook a gated clone
/// has of its own (the hooks folder is shared, so the union).
pub fn needed_hooks(gated: &[Gated]) -> BTreeSet<&'static str> {
    let mut needed: BTreeSet<&'static str> = BTreeSet::from(["pre-push"]);
    for clone in gated {
        needed.extend(
            clone
                .own
                .iter()
                .copied()
                .filter(|name| !NOT_DISPATCHED.contains(name)),
        );
    }
    needed
}

/// What is wrong with a gated clone: (a) git does not resolve its
/// `core.hooksPath` to the home's hooks, so the gate does not run; (b) a hook
/// of its own has no dispatcher, so git silently skips it.
pub fn clone_drift(home: &Path, clone: &Gated) -> Vec<String> {
    let hooks = home::hooks_dir(home);
    let mut drift = Vec::new();
    match &clone.hooks_path {
        Some(path) if home::same_path(Path::new(path), &hooks) => {}
        Some(path) => drift.push(format!(
            "its own core.hooksPath ({path}) overrides ohjaa's: workflow gate not applied \
             here: the repo's own hooks run"
        )),
        None => drift.push(format!(
            "git does not include {} for it: no pattern there matches its remotes, or \
             `ohjaa apply` has not run; the gate is not applied (ohjaa apply)",
            include_file(home).display()
        )),
    }
    let routed = clone
        .hooks_path
        .as_deref()
        .is_some_and(|path| home::same_path(Path::new(path), &hooks));
    let (never, own): (Vec<&str>, Vec<&str>) = clone
        .own
        .iter()
        .copied()
        .partition(|name| NOT_DISPATCHED.contains(name));
    if routed && !never.is_empty() {
        drift.push(format!(
            "its own {} cannot run under ohjaa's hooks folder (a dispatcher of that name \
             would change what git does in every gated repository), so git skips it",
            never.join(", ")
        ));
    }
    let missing: Vec<&str> = own
        .into_iter()
        .filter(|name| !hooks.join(name).is_file())
        .collect();
    if routed && !missing.is_empty() {
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

/// For `ohjaa run`: the same checks for the repository a session starts in,
/// silent everywhere else. At most three git calls, and only in a gated
/// repository.
pub fn launch_warnings(repo: &Path) -> Vec<String> {
    let Ok(home) = home::ohjaa_home() else {
        return Vec::new();
    };
    match gated_clone(&home, repo) {
        Ok(Some(clone)) => clone_drift(&home, &clone)
            .into_iter()
            .map(|drift| format!("{} ({}): {drift}", repo.display(), clone.slugs.join(", ")))
            .collect(),
        Ok(None) => Vec::new(),
        Err(error) => vec![format!(
            "cannot tell whether the workflow gate applies in {}: {error:#}",
            repo.display()
        )],
    }
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
    fn patterns_cover_every_form_in_either_case() {
        let found = patterns("acme", "w-1", &[]);
        assert_eq!(
            found,
            [
                "**/[aA][cC][mM][eE]/[wW]-1",
                "**/[aA][cC][mM][eE]/[wW]-1.git",
                "**/[aA][cC][mM][eE]/[wW]-1/",
                "*:[aA][cC][mM][eE]/[wW]-1",
                "*:[aA][cC][mM][eE]/[wW]-1.git",
                "*:[aA][cC][mM][eE]/[wW]-1/",
            ]
        );
        assert_eq!(any_case("a*b"), "[aA]\\*[bB]");
    }

    #[test]
    fn insteadof_rules_add_the_short_forms_that_reach_the_repository() {
        let rewrites = parse_rewrites(
            "url.git@github.com:.insteadof\ngh:\0\
             url.git@github.com:acme/.insteadof\nac:\0\
             url.https://elsewhere.example/other/.insteadof\nx:\0",
        );
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
    fn the_include_file_quotes_what_git_reads_back() {
        let text = include_text(Path::new("/h/ohjaa"), &[("acme".into(), "w".into())], &[]);
        assert!(text.contains(
            "[includeIf \"hasconfig:remote.*.url:**/[aA][cC][mM][eE]/[wW]\"]\n\tpath = \"/h/ohjaa/git/gated.config\"\n"
        ));
        assert_eq!(config_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert!(gated_text(Path::new("/h/ohjaa")).contains("hooksPath = \"/h/ohjaa/hooks\""));
    }
}
