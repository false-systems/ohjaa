//! ohjaa — the deciding layer above toimija.
//!
//! Toimija reports facts and refuses to decide: its boundaries forbid dispatch,
//! assignment, and network. That refusal is load-bearing, so the deciding lives
//! here instead. Ohjaa shells out to `toimija` and `sauma`, decides, and gets
//! out of the way. It never speaks the plugin protocol, never compiles a
//! packet, and stores nothing.

mod roots;
mod scan;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use scan::{ago, scan_fleet, scan_repo, Coverage, Scan};

/// The launch was refused.
const REFUSED: i32 = 3;
/// At least one repo could not be scanned, so absence proves nothing.
const INCOMPLETE: i32 = 4;

#[derive(Parser)]
#[command(
    name = "ohjaa",
    version,
    about = "The deciding layer above toimija.",
    long_about = "Toimija reports facts and refuses to decide. Ohjaa decides.\n\n\
                  Everything is derived on demand: no inbox, no index, no cache."
)]
struct Cli {
    /// Where to look. Repeatable; defaults to the config file, else this repo's siblings.
    #[arg(long, global = true, value_name = "PATH")]
    root: Vec<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Who is live, across every repo.
    Fleet {
        #[arg(long)]
        json: bool,
    },
    /// Provenance walk, with every root filled in automatically.
    Why {
        /// Digest, consistency token, or artifact id.
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect live siblings, then hand off to `toimija run`.
    Run {
        /// Agent command, e.g. claude / codex.
        agent: String,
        /// Why this session exists.
        #[arg(long)]
        intent: String,
        /// Decide and explain, but do not launch.
        #[arg(long)]
        dry_run: bool,
    },
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().context("failed to run git")?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_root() -> Option<PathBuf> {
    git(&["rev-parse", "--show-toplevel"], None)
        .ok()
        .map(PathBuf::from)
}

fn repos(explicit: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let roots = roots::load(explicit, git_root().as_deref());
    roots::toimija_repos(&roots)
}

/// Report coverage on stderr and return the exit code it implies.
///
/// A scan that failed is not a scan that found nothing. Saying so is the point.
fn report_coverage(scans: &[Scan]) -> i32 {
    let failed: Vec<&Scan> = scans
        .iter()
        .filter(|s| s.coverage == Coverage::Failed)
        .collect();

    // stdout is block-buffered when piped; keep the report in order.
    let _ = std::io::stdout().flush();
    eprintln!(
        "\ncoverage: scanned {}/{} repo(s)",
        scans.len() - failed.len(),
        scans.len()
    );
    if failed.is_empty() {
        return 0;
    }
    eprintln!("  incomplete — absence here is not evidence of absence:");
    for scan in failed {
        eprintln!(
            "    {}: {}",
            scan.repo.file_name().unwrap_or_default().to_string_lossy(),
            scan.error.as_deref().unwrap_or("unknown failure")
        );
    }
    INCOMPLETE
}

fn cmd_fleet(explicit: &[PathBuf], json: bool) -> Result<i32> {
    let repos = repos(explicit)?;
    let mut scans = scan_fleet(&repos);
    scans.sort_by_key(|scan| scan.repo.file_name().map(|n| n.to_os_string()));

    if json {
        println!("{}", serde_json::to_string_pretty(&scans)?);
        return Ok(report_coverage(&scans));
    }

    let mut any_live = false;
    for scan in &scans {
        let live: Vec<_> = scan.occupying().collect();
        if live.is_empty() && scan.error.is_none() {
            continue;
        }
        any_live |= !live.is_empty();
        println!(
            "{}",
            scan.repo.file_name().unwrap_or_default().to_string_lossy()
        );
        if let Some(error) = &scan.error {
            println!("    ! scan failed: {error}");
        }
        for session in live {
            println!("    {}  {}", session.agent, session.id);
            println!(
                "      branch: {}   seen: {}{}",
                session.branch.as_deref().unwrap_or("detached"),
                ago(session.idle_seconds),
                // Just the label. Asserting "may still be running" next to
                // `seen: 486h ago` would be its own overclaim; the age is the
                // evidence and the reader can judge it. Proposal 0010's rule —
                // a signal belongs only if it cannot go stale without
                // confessing it — and the timestamp is the confession.
                if session.is_quiet() { "   stale" } else { "" }
            );
            let intent = if session.intent.is_empty() {
                "(undeclared)"
            } else {
                &session.intent
            };
            println!("      intent: {intent}");
            if !session.scope.is_empty() {
                println!("      scope:  {}", session.scope.join(", "));
            }
        }
        for warning in &scan.warnings {
            println!("    warning: {warning}");
        }
    }
    if !any_live {
        println!("no live sessions");
    }
    Ok(report_coverage(&scans))
}

fn cmd_why(explicit: &[PathBuf], query: &str, json: bool) -> Result<i32> {
    let repos = repos(explicit)?;
    let mut command = Command::new("sauma");
    command.arg("why").arg(query);
    for repo in &repos {
        command.arg("--root").arg(repo);
    }
    if json {
        command.arg("--json");
    }
    eprintln!("ohjaa: walking {} repo(s)", repos.len());
    let status = command.status().context("failed to run sauma")?;
    Ok(status.code().unwrap_or(1))
}

fn cmd_run(agent: &str, intent: &str, dry_run: bool) -> Result<i32> {
    let root = git_root().context("not inside a Git repository")?;

    let scan = scan_repo(&root);
    if scan.coverage == Coverage::Failed {
        eprintln!(
            "ohjaa: cannot read sessions in {}: {}",
            root.display(),
            scan.error.as_deref().unwrap_or("unknown failure")
        );
        eprintln!("ohjaa: refusing to launch blind");
        return Ok(REFUSED);
    }

    // Counts every session that may still be working here, not just the ones
    // toimija currently calls active: reporting an occupied checkout as empty
    // is the one thing this line must never do.
    let live = scan.occupying().count();
    let quiet = scan.occupying().filter(|s| s.is_quiet()).count();
    // "at launch", because this is a reading taken a moment ago, not a
    // standing fact: a session starting in the same instant will not be in it.
    // Nothing is gated on the number, so a slightly stale count costs nothing —
    // but a line that sounds like a guarantee it cannot give is the habit this
    // whole tool exists to avoid.
    eprintln!("ohjaa: {live} sibling session(s) in this checkout at launch");
    if quiet > 0 {
        eprintln!(
            "ohjaa: {quiet} of them stale — toimija cannot tell a finished agent \
             from a quiet one; `toimija sessions` shows their last heartbeat"
        );
    }
    for warning in &scan.warnings {
        eprintln!("ohjaa: warning: {warning}");
    }
    if dry_run {
        return Ok(0);
    }

    let mut command = Command::new("toimija");
    command.arg("run").arg("--intent").arg(intent);
    command.arg("--").arg(agent).current_dir(&root);
    let status = command.status().context("failed to run toimija")?;
    Ok(status.code().unwrap_or(1))
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::Fleet { json } => cmd_fleet(&cli.root, *json),
        Cmd::Why { query, json } => cmd_why(&cli.root, query, *json),
        Cmd::Run {
            agent,
            intent,
            dry_run,
        } => cmd_run(agent, intent, *dry_run),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("ohjaa: {error:#}");
            std::process::exit(1);
        }
    }
}
