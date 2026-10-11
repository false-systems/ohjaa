//! `tools.lock`: the revisions of the workflow's tools (sykli, toimija, ohjaa,
//! vessa, seula) that `ohjaa apply --tools` installs and `ohjaa doctor`
//! compares with what is installed.
//!
//! ```toml
//! schema = "ohjaa-tools-lock.v1"
//!
//! [tools.sykli]
//! git = "ssh://git@github.com/false-systems/sykli.git"
//! rev = "95bc592…"          # a full commit id, never a branch
//! toolchain = "1.98"        # the channel the tool's rust-toolchain.toml pins
//! ```
//!
//! One `[tools.<name>]` table per tool; `<name>` is the Cargo package, which
//! `cargo install` is asked for by name (a repository may hold several).
//! `toolchain` is there because `cargo install --git` builds with whatever
//! toolchain runs it and never reads the repository's `rust-toolchain.toml`.
//!
//! What is installed is read from Cargo's own record, `$CARGO_HOME/.crates.toml`
//! (else `~/.cargo/.crates.toml`): a git install names the commit it built,
//! a path install names only a directory, so it is "unpinned".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::flat_toml;
use crate::home;

pub const SCHEMA: &str = "ohjaa-tools-lock.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub git: String,
    pub rev: String,
    pub toolchain: String,
}

fn full_commit_id(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

fn tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn parse_lock(text: &str) -> std::result::Result<Vec<Pin>, String> {
    let tables = flat_toml::parse(text)?;
    flat_toml::only_keys(&tables, "", &["schema"])?;
    match tables[""].get("schema").map(String::as_str) {
        Some(SCHEMA) => {}
        Some(other) => return Err(format!("schema is {other:?}; this ohjaa reads {SCHEMA}")),
        None => return Err(format!("no schema = \"{SCHEMA}\"")),
    }
    let mut pins = Vec::new();
    for (table, keys) in &tables {
        if table.is_empty() || (table == "tools" && keys.is_empty()) {
            continue;
        }
        let Some(name) = table.strip_prefix("tools.").filter(|name| tool_name(name)) else {
            return Err(format!("unknown table [{table}]; tools are [tools.<name>]"));
        };
        flat_toml::only_keys(&tables, table, &["git", "rev", "toolchain"])?;
        let field = |key: &str| {
            keys.get(key)
                .filter(|value| !value.is_empty())
                .cloned()
                .ok_or_else(|| format!("[{table}] has no {key}"))
        };
        let pin = Pin {
            name: name.to_owned(),
            git: field("git")?,
            rev: field("rev")?,
            toolchain: field("toolchain")?,
        };
        if !full_commit_id(&pin.rev) {
            return Err(format!(
                "[{table}] rev {:?} is not a full commit id; a lock pins commits",
                pin.rev
            ));
        }
        pins.push(pin);
    }
    Ok(pins)
}

pub fn render_lock(pins: &[Pin]) -> String {
    let mut pins = pins.to_vec();
    pins.sort_by(|a, b| a.name.cmp(&b.name));
    let mut text = format!(
        "# The tool revisions `ohjaa apply --tools` installs, one table per tool.\n\
         # Written by `ohjaa tools pin`. `toolchain` is the channel the tool's\n\
         # rust-toolchain.toml pins: `cargo install --git` does not read that file.\n\
         schema = {}\n",
        flat_toml::quote(SCHEMA)
    );
    for pin in &pins {
        text.push_str(&format!(
            "\n[tools.{}]\ngit = {}\nrev = {}\ntoolchain = {}\n",
            pin.name,
            flat_toml::quote(&pin.git),
            flat_toml::quote(&pin.rev),
            flat_toml::quote(&pin.toolchain)
        ));
    }
    text
}

/// The pins in `path`; `Ok(None)` when there is no lock.
pub fn read_lock(path: &Path) -> Result<Option<Vec<Pin>>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_lock(&text)
            .map(Some)
            .map_err(|why| anyhow::anyhow!("{}: {why}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

pub fn write_lock(path: &Path, pins: &[Pin]) -> Result<()> {
    home::write_file(path, &render_lock(pins), None)
}

/// How Cargo says a tool was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// From git: the commit it was built from.
    Git { url: String, sha: String },
    /// From a directory: no commit is recorded.
    Path(String),
    /// From a registry or anything else.
    Other(String),
}

/// `"<name> <version> (<source>)" = [...]` lines of `.crates.toml`.
pub fn parse_crates_toml(text: &str) -> BTreeMap<String, Installed> {
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let Some(key) = line
            .trim_start()
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(key, _)| key)
        else {
            continue;
        };
        let Some((name, rest)) = key.split_once(' ') else {
            continue;
        };
        let source = rest
            .split_once('(')
            .and_then(|(_, source)| source.strip_suffix(')'))
            .unwrap_or("");
        let installed = if let Some(git) = source.strip_prefix("git+") {
            match git.rsplit_once('#') {
                Some((url, sha)) => Installed::Git {
                    url: url.split('?').next().unwrap_or(url).to_owned(),
                    sha: sha.to_owned(),
                },
                None => Installed::Other(source.to_owned()),
            }
        } else if let Some(path) = source.strip_prefix("path+") {
            Installed::Path(path.strip_prefix("file://").unwrap_or(path).to_owned())
        } else {
            Installed::Other(source.to_owned())
        };
        found.insert(name.to_owned(), installed);
    }
    found
}

/// `$CARGO_HOME`, else `~/.cargo`.
fn cargo_home() -> Result<PathBuf> {
    match std::env::var_os("CARGO_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(PathBuf::from(
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .context("HOME is not set")?,
        )
        .join(".cargo")),
    }
}

/// What Cargo installed, and the file that says so.
pub fn installed() -> Result<(PathBuf, BTreeMap<String, Installed>)> {
    let path = cargo_home()?.join(".crates.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok((path, parse_crates_toml(&text))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((path, BTreeMap::new())),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// `Ok(())` when the installed tool is the pinned commit, else what it is.
pub fn standing(pin: &Pin, installed: Option<&Installed>) -> std::result::Result<(), String> {
    let short = |sha: &str| sha[..sha.len().min(12)].to_owned();
    match installed {
        None => Err("not installed".to_owned()),
        Some(Installed::Git { sha, .. }) if sha == &pin.rev => Ok(()),
        Some(Installed::Git { sha, .. }) => Err(format!("installed at {}", short(sha))),
        Some(Installed::Path(path)) => Err(format!("unpinned: installed from the path {path}")),
        Some(Installed::Other(source)) => Err(format!("unpinned: installed from {source}")),
    }
}

/// `cargo +<toolchain> install --git <url> --rev <rev> --locked --force <name>`.
///
/// `--force`, because a tool installed from a path or another commit owns
/// the binary and Cargo refuses to replace it otherwise; this only runs for a
/// tool that is not at its pin.
pub fn install_command(pin: &Pin) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg(format!("+{}", pin.toolchain))
        .args(["install", "--git", &pin.git, "--rev", &pin.rev])
        .args(["--locked", "--force", &pin.name]);
    command
}

/// A remote as `cargo install --git` takes it: the scp-like
/// `git@host:owner/repo.git` becomes `ssh://git@host/owner/repo.git`.
pub fn cargo_url(remote: &str) -> String {
    if remote.contains("://") {
        return remote.to_owned();
    }
    match remote.split_once(':') {
        Some((before, after)) if !before.contains('/') && !before.is_empty() => {
            format!("ssh://{before}/{}", after.trim_start_matches('/'))
        }
        _ => remote.to_owned(),
    }
}

/// The channel a `rust-toolchain.toml` (or a legacy one-line
/// `rust-toolchain`) pins.
pub fn toolchain_channel(text: &str) -> Option<String> {
    let channel = text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "channel").then(|| value.trim().trim_matches('"').to_owned())
    });
    channel
        .or_else(|| {
            let line = text.trim();
            (!line.is_empty() && !line.contains(['\n', '=', '['])).then(|| line.to_owned())
        })
        .filter(|channel| !channel.is_empty())
}

/// Add or replace one pin. Without `from`, `git`, `rev` and `toolchain` are
/// taken as given (a missing one is kept from the existing pin). With
/// `from`, a local clone of the tool, each one not given is read from it
/// (its origin URL, the commit `rev` names there, else origin's default
/// branch, and that commit's rust-toolchain.toml), without the network.
pub fn pin(
    lock: &Path,
    name: &str,
    from: Option<&Path>,
    git: Option<&str>,
    rev: Option<&str>,
    toolchain: Option<&str>,
) -> Result<Pin> {
    if !tool_name(name) {
        bail!("{name:?} is not a Cargo package name");
    }
    let mut pins = read_lock(lock)?.unwrap_or_default();
    let existing = pins.iter().find(|pin| pin.name == name).cloned();
    let (mut git, mut rev, mut toolchain) = (
        git.map(str::to_owned),
        rev.map(str::to_owned),
        toolchain.map(str::to_owned),
    );
    if let Some(clone) = from {
        let in_clone = |args: &[&str]| home::git_value(args, Some(clone));
        let commit = format!(
            "{}^{{commit}}",
            rev.as_deref().unwrap_or("refs/remotes/origin/HEAD")
        );
        let sha = in_clone(&["rev-parse", "--verify", "--quiet", &commit])?
            .with_context(|| format!("{} has no commit {commit}", clone.display()))?;
        if git.is_none() {
            let remote = in_clone(&["remote", "get-url", "origin"])?
                .with_context(|| format!("{} has no origin remote", clone.display()))?;
            git = Some(cargo_url(&remote));
        }
        if toolchain.is_none() {
            toolchain = ["rust-toolchain.toml", "rust-toolchain"]
                .iter()
                .find_map(|file| {
                    in_clone(&["show", &format!("{sha}:{file}")])
                        .ok()
                        .flatten()
                        .and_then(|text| toolchain_channel(&text))
                });
        }
        rev = Some(sha);
    }
    let missing = |what: &str, flag: &str| {
        anyhow::anyhow!("no {what} for {name}: pass {flag}, or --from <a clone of it>")
    };
    let pin = Pin {
        name: name.to_owned(),
        git: git
            .or_else(|| existing.as_ref().map(|pin| pin.git.clone()))
            .ok_or_else(|| missing("git URL", "--git"))?,
        rev: rev
            .or_else(|| existing.as_ref().map(|pin| pin.rev.clone()))
            .ok_or_else(|| missing("revision", "--rev"))?,
        toolchain: toolchain
            .or_else(|| existing.as_ref().map(|pin| pin.toolchain.clone()))
            .ok_or_else(|| {
                missing(
                    "toolchain (the channel its rust-toolchain.toml pins)",
                    "--toolchain",
                )
            })?,
    };
    if !full_commit_id(&pin.rev) {
        bail!(
            "{:?} is not a full commit id; a lock pins commits (with --from, any revision the clone knows is resolved)",
            pin.rev
        );
    }
    pins.retain(|other| other.name != name);
    pins.push(pin.clone());
    write_lock(lock, &pins)?;
    Ok(pin)
}

pub fn unpin(lock: &Path, name: &str) -> Result<bool> {
    let Some(mut pins) = read_lock(lock)? else {
        return Ok(false);
    };
    let before = pins.len();
    pins.retain(|pin| pin.name != name);
    if pins.len() == before {
        return Ok(false);
    }
    write_lock(lock, &pins)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "95bc5921a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn sykli() -> Pin {
        Pin {
            name: "sykli".into(),
            git: "ssh://git@github.com/false-systems/sykli.git".into(),
            rev: SHA.into(),
            toolchain: "1.98".into(),
        }
    }

    #[test]
    fn a_lock_reads_back_what_was_written() {
        let toimija = Pin {
            name: "toimija".into(),
            ..sykli()
        };
        let text = render_lock(&[toimija.clone(), sykli()]);
        assert_eq!(parse_lock(&text).unwrap(), vec![sykli(), toimija]);
        assert!(text.contains("[tools.sykli]\n"), "{text}");
    }

    #[test]
    fn a_lock_pins_full_commits_and_names_what_it_lacks() {
        let base =
            format!("schema = \"{SCHEMA}\"\n[tools.sykli]\ngit = \"g\"\ntoolchain = \"1.98\"\n");
        for (text, why) in [
            (format!("{base}rev = \"main\"\n"), "not a full commit id"),
            (format!("{base}rev = \"95bc592\"\n"), "not a full commit id"),
            (base.clone(), "has no rev"),
            (
                format!("{base}rev = \"{SHA}\"\nbranch = \"x\"\n"),
                "unknown key branch",
            ),
            ("[tools.x]\n".to_owned(), "no schema"),
            ("schema = \"v0\"\n".to_owned(), "this ohjaa reads"),
            (
                format!("schema = \"{SCHEMA}\"\n[sykli]\n"),
                "unknown table [sykli]",
            ),
        ] {
            let error = parse_lock(&text).unwrap_err();
            assert!(error.contains(why), "{text:?}: {error}");
        }
    }

    #[test]
    fn cargo_records_say_which_installs_are_pinned() {
        let found = parse_crates_toml(&format!(
            "[v1]\n\
             \"sykli 0.6.0 (git+ssh://git@github.com/false-systems/sykli.git?rev={SHA}#{SHA})\" = [\"sykli\"]\n\
             \"cross 0.2.5 (git+https://github.com/cross-rs/cross#49cd054de9b832dfc11a4895c72b0aef533b5c6a)\" = [\n    \"cross\",\n]\n\
             \"toimija 0.1.1 (path+file:///Users/x/projects/toimija)\" = [\"toimija\"]\n\
             \"cargo-audit 0.22.0 (registry+https://github.com/rust-lang/crates.io-index)\" = [\"cargo-audit\"]\n"
        ));
        assert_eq!(
            found["sykli"],
            Installed::Git {
                url: "ssh://git@github.com/false-systems/sykli.git".into(),
                sha: SHA.into()
            }
        );
        assert!(
            matches!(&found["cross"], Installed::Git { sha, .. } if sha.starts_with("49cd054"))
        );
        assert_eq!(
            found["toimija"],
            Installed::Path("/Users/x/projects/toimija".into())
        );
        assert!(matches!(found["cargo-audit"], Installed::Other(_)));

        assert_eq!(standing(&sykli(), found.get("sykli")), Ok(()));
        let toimija = Pin {
            name: "toimija".into(),
            ..sykli()
        };
        let unpinned = standing(&toimija, found.get("toimija")).unwrap_err();
        assert!(unpinned.starts_with("unpinned"), "{unpinned}");
        assert_eq!(standing(&sykli(), None).unwrap_err(), "not installed");
        let other = Installed::Git {
            url: "u".into(),
            sha: "a".repeat(40),
        };
        assert!(standing(&sykli(), Some(&other))
            .unwrap_err()
            .contains("aaaaaaaaaaaa"));
    }

    #[test]
    fn the_install_runs_the_pinned_toolchain_on_the_pinned_commit() {
        let command = install_command(&sykli());
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "cargo");
        assert_eq!(
            args,
            [
                "+1.98",
                "install",
                "--git",
                "ssh://git@github.com/false-systems/sykli.git",
                "--rev",
                SHA,
                "--locked",
                "--force",
                "sykli"
            ]
        );
    }

    #[test]
    fn scp_like_remotes_become_urls_cargo_takes() {
        assert_eq!(
            cargo_url("git@github.com:false-systems/sykli.git"),
            "ssh://git@github.com/false-systems/sykli.git"
        );
        for url in [
            "https://github.com/o/r",
            "ssh://git@h/o/r.git",
            "/tmp/o/r.git",
        ] {
            assert_eq!(cargo_url(url), url);
        }
    }

    #[test]
    fn the_channel_is_read_from_either_toolchain_file() {
        let toml = "[toolchain]\nchannel = \"1.98\"\ncomponents = [\"rustfmt\", \"clippy\"]\n";
        assert_eq!(toolchain_channel(toml).as_deref(), Some("1.98"));
        assert_eq!(
            toolchain_channel("nightly-2026-01-01\n").as_deref(),
            Some("nightly-2026-01-01")
        );
        assert_eq!(toolchain_channel("[toolchain]\n"), None);
    }
}
