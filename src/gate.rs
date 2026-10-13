//! `ohjaa gate`: run the gate a push is judged by for the repository here,
//! and leave the receipt that proves HEAD: the push finds it, and so do the
//! marks (`.sykli/receipt.json`, and the copy `sykli run` keeps under
//! `.sykli/receipts/`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::gitconfig::{self, push_url, repo_at, Listing};
use crate::home::{self, absolute, RepoId};
use crate::hook::{facts_for_sykli, quote_path};

/// The gate of the repository at `top`: the contract of the entry origin's
/// push URL names, else the repository's own `sykli.json`; with whose it is.
fn gate_of(top: &Path, listing: &Listing) -> Result<Option<(PathBuf, String)>> {
    let home = absolute(home::ohjaa_home()?);
    if let Some(id) = push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)
    {
        match home::find_entry(&home, &id)? {
            Some(entry) if entry.has_contract() => {
                return Ok(Some((
                    entry.contract(),
                    format!("the entry for {} (branch {})", id.slug(), entry.branch),
                )))
            }
            Some(entry) => bail!(
                "{} has an entry but no gate: {} is missing (ohjaa adopt --force {})",
                id.slug(),
                entry.contract().display(),
                top.display()
            ),
            None => {}
        }
    }
    let own = top.join("sykli.json");
    Ok(own
        .is_file()
        .then(|| (own, "the repository's own sykli.json (no entry)".to_owned())))
}

/// `sykli <command> --help` names both facts flags: then sykli is handed the
/// repository's facts instead of asking git for them again.
fn sykli_takes_facts(command: &str, top: &Path, common: &Path) -> Vec<std::ffi::OsString> {
    let help = Command::new("sykli")
        .args([command, "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    facts_for_sykli(&help, Some(top), common)
}

fn shown(words: &[std::ffi::OsString]) -> String {
    words
        .iter()
        .map(|word| {
            let word = word.to_string_lossy();
            if word.starts_with('-') {
                word.into_owned()
            } else {
                quote_path(Path::new(word.as_ref()))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ohjaa gate`: run the gate for the repository here, the way a push is
/// judged, and leave the receipt that proves HEAD. One git process (the
/// repository's config); the rest is sykli's.
pub fn gate_here(print_only: bool) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = repo_at(&cwd)?;
    let listing = gitconfig::clone_listing(&top)?;
    let Some((contract, whose)) = gate_of(&top, &listing)? else {
        eprintln!(
            "ohjaa gate: {} has no gate: origin names no entry (ohjaa adopt), and there is \
             no sykli.json in the repository",
            top.display()
        );
        return Ok(2);
    };
    let quoted = quote_path(&contract);
    let (run_facts, verify_facts) = if print_only {
        (Vec::new(), Vec::new())
    } else {
        (
            sykli_takes_facts("run", &top, &common),
            sykli_takes_facts("verify", &top, &common),
        )
    };
    let extra = |facts: &[std::ffi::OsString]| {
        if facts.is_empty() {
            String::new()
        } else {
            format!(" {}", shown(facts))
        }
    };
    let run = format!(
        "sykli run {quoted}{} --json > .sykli/receipt.json",
        extra(&run_facts)
    );
    let verify = format!(
        "sykli verify .sykli/receipt.json --contract {quoted}{} --commit HEAD",
        extra(&verify_facts)
    );
    println!("ohjaa gate: the gate is {} ({whose})", contract.display());
    if print_only {
        println!(
            "cd {} && mkdir -p .sykli && {run} && {verify}",
            quote_path(&top)
        );
        return Ok(0);
    }
    println!("ohjaa gate: in {}", top.display());
    let sykli_dir = top.join(".sykli");
    std::fs::create_dir_all(&sykli_dir)
        .with_context(|| format!("cannot create {}", sykli_dir.display()))?;
    println!("$ {run}");
    let receipt_path = sykli_dir.join("receipt.json");
    let receipt = std::fs::File::create(&receipt_path)
        .with_context(|| format!("cannot write {}", receipt_path.display()))?;
    let status = Command::new("sykli")
        .arg("run")
        .arg(&contract)
        .args(&run_facts)
        .arg("--json")
        .current_dir(&top)
        .stdout(receipt)
        .status()
        .context("cannot run sykli")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        println!("ohjaa gate: failed: sykli run exited {code}; the receipt says why");
        return Ok(code);
    }
    println!("$ {verify}");
    let status = Command::new("sykli")
        .args(["verify", ".sykli/receipt.json", "--contract"])
        .arg(&contract)
        .args(&verify_facts)
        .args(["--commit", "HEAD"])
        .current_dir(&top)
        .status()
        .context("cannot run sykli")?;
    let code = status.code().unwrap_or(1);
    if status.success() {
        println!("ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven");
    } else {
        println!("ohjaa gate: failed: sykli verify exited {code}");
    }
    Ok(code)
}
