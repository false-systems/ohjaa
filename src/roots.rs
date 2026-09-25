//! Where to look.
//!
//! The root list is derived by default and configured only if you insist.
//! Storing a list of roots is one refactor away from storing an index of their
//! contents, and an index is the second database this stack refuses to build.
//! So: paths only, never contents, and no file at all in the common case.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Config file holding one path per line; `#` starts a comment. Lives in
/// `$XDG_CONFIG_HOME/ohjaa/roots`, else `~/.config/ohjaa/roots`; an empty
/// `XDG_CONFIG_HOME` counts as unset.
pub fn config_path() -> Option<PathBuf> {
    crate::knowledge::config_home()
        .ok()
        .map(|config| config.join("ohjaa").join("roots"))
}

/// The sibling directories of this repo.
///
/// Zero configuration and honest: working in `~/projects/toimija` makes the
/// fleet `~/projects/*`.
fn derived_roots(git_root: Option<&Path>) -> Vec<PathBuf> {
    match git_root.and_then(Path::parent) {
        Some(parent) => vec![parent.to_path_buf()],
        None => vec![PathBuf::from(".")],
    }
}

fn parse_config(contents: &str) -> Vec<PathBuf> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.split('#').next().unwrap_or("").trim();
            (!line.is_empty()).then(|| PathBuf::from(expand_home(line)))
        })
        .collect()
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest).to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// Explicit `--root` wins, then the config file, then derivation.
pub fn load(explicit: &[PathBuf], git_root: Option<&Path>) -> Vec<PathBuf> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    if let Some(path) = config_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let roots = parse_config(&contents);
            if !roots.is_empty() {
                return roots;
            }
        }
    }
    derived_roots(git_root)
}

/// Every toimija-prepared repo under the roots. A root may itself be one.
pub fn toimija_repos(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    repos_under(roots, |path| path.join(".toimija").is_dir())
}

/// Every Git repository under the roots. A root may itself be one.
pub fn git_repos(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    repos_under(roots, |path| path.join(".git").exists())
}

/// A root that is not a directory is an error, never an empty fleet: a typo in
/// `--root` must not read as "nobody is working anywhere".
fn repos_under(roots: &[PathBuf], is_repo: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let mut found: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !root.is_dir() {
            anyhow::bail!("root {} is not a directory", root.display());
        }
        let mut candidates = vec![root.clone()];
        if let Ok(entries) = std::fs::read_dir(root) {
            let mut children: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            children.sort();
            candidates.extend(children);
        }
        for path in candidates {
            if is_repo(&path) && !found.contains(&path) {
                found.push(path);
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_comments_and_blank_lines_are_ignored() {
        let roots = parse_config("# a comment\n\n/one\n/two  # trailing\n");
        assert_eq!(roots, vec![PathBuf::from("/one"), PathBuf::from("/two")]);
    }

    #[test]
    fn derived_roots_are_the_repo_siblings() {
        let roots = derived_roots(Some(Path::new("/Users/x/projects/toimija")));
        assert_eq!(roots, vec![PathBuf::from("/Users/x/projects")]);
    }

    #[test]
    fn derivation_outside_a_repo_falls_back_to_cwd() {
        assert_eq!(derived_roots(None), vec![PathBuf::from(".")]);
    }

    #[test]
    fn explicit_roots_win() {
        let explicit = vec![PathBuf::from("/tmp/x")];
        let roots = load(&explicit, Some(Path::new("/Users/x/projects/toimija")));
        assert_eq!(roots, explicit);
    }

    #[test]
    fn only_toimija_prepared_dirs_are_found() {
        let tmp = std::env::temp_dir().join(format!("ohjaa-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("prepared").join(".toimija")).unwrap();
        std::fs::create_dir_all(tmp.join("plain")).unwrap();

        let found = toimija_repos(std::slice::from_ref(&tmp)).unwrap();
        assert_eq!(found, vec![tmp.join("prepared")]);

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_root_may_itself_be_a_repo_and_is_not_double_counted() {
        let tmp = std::env::temp_dir().join(format!("ohjaa-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".toimija")).unwrap();

        let found = toimija_repos(&[tmp.clone(), tmp.clone()]).unwrap();
        assert_eq!(found, vec![tmp.clone()]);

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
