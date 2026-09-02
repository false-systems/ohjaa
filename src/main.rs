//! ohjaa — the deciding layer above toimija.
//!
//! Toimija reports facts and refuses to decide: its boundaries forbid dispatch,
//! assignment, and network. That refusal is load-bearing, so the deciding lives
//! here instead. Ohjaa shells out to `toimija` and `sauma`, decides, and gets
//! out of the way. It never speaks the plugin protocol, never compiles a
//! packet, and stores nothing.

mod roots;
mod scan;
mod system;

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
        /// What to do; forwarded to `toimija run --task` so it travels in the
        /// composed launch prompt.
        #[arg(long)]
        task: Option<String>,
        /// Paths this session expects to touch (repeatable); forwarded to
        /// `toimija run --scope`.
        #[arg(long = "scope", value_name = "PATH")]
        scope: Vec<String>,
        /// Decide and explain, but do not launch.
        #[arg(long)]
        dry_run: bool,
        /// Do not prepend the derived system perimeter to the task.
        #[arg(long = "no-briefing")]
        no_briefing: bool,
        /// Everything after `--` is passed through to the agent command.
        #[arg(last = true)]
        passthrough: Vec<String>,
    },
    /// The system perimeter: what every tool in the fleet is, derived now.
    System {
        #[arg(long)]
        json: bool,
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
    let incomplete: Vec<&Scan> = scans
        .iter()
        .filter(|s| s.coverage != Coverage::Available)
        .collect();

    // stdout is block-buffered when piped; keep the report in order.
    let _ = std::io::stdout().flush();
    eprintln!(
        "\ncoverage: scanned {}/{} repo(s)",
        scans.len() - incomplete.len(),
        scans.len()
    );
    if incomplete.is_empty() {
        return 0;
    }
    eprintln!("  incomplete — absence here is not evidence of absence:");
    for scan in incomplete {
        eprintln!(
            "    {}: {}",
            scan.repo.file_name().unwrap_or_default().to_string_lossy(),
            coverage_detail(scan)
        );
    }
    INCOMPLETE
}

/// What to say about an incomplete scan: the failure if there was one, else
/// the first warning, else the bare verdict. A degraded scan with no detail
/// would otherwise render as a bare word that explains nothing.
fn coverage_detail(scan: &Scan) -> &str {
    scan.error
        .as_deref()
        .or_else(|| scan.warnings.first().map(String::as_str))
        .unwrap_or("degraded coverage")
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

fn cmd_system(explicit: &[PathBuf], json: bool) -> Result<i32> {
    let repos = repos(explicit)?;
    if repos.is_empty() {
        eprintln!("ohjaa: no toimija-prepared repos under the roots");
        return Ok(0);
    }
    let view = system::system_view(&repos);
    if json {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print!("{}", system::render_system(&view));
    }
    Ok(view.exit_code())
}

/// Everything `run` needs, bundled so the signature stays honest.
struct RunOptions<'a> {
    agent: &'a str,
    intent: &'a str,
    task: Option<&'a str>,
    scope: &'a [String],
    dry_run: bool,
    no_briefing: bool,
    passthrough: &'a [String],
    roots: &'a [PathBuf],
}

fn cmd_run(options: RunOptions<'_>) -> Result<i32> {
    let RunOptions {
        agent,
        intent,
        task,
        scope,
        dry_run,
        no_briefing,
        passthrough,
        roots,
    } = options;
    let root = git_root().context("not inside a Git repository")?;

    let scan = scan_repo(&root);
    if scan.coverage != Coverage::Available {
        eprintln!(
            "ohjaa: cannot completely read sessions in {}: {}",
            root.display(),
            coverage_detail(&scan)
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

    // A footgun guard, earned the hard way: ohjaa flags after `--` belong to
    // the agent, so `-- --dry-run` launches for real instead of explaining.
    // Passing ohjaa's own flags through is sometimes legitimate (an agent that
    // itself takes --task, say), so this warns loudly rather than refusing —
    // but nobody should lose a launch to a silent misplacement.
    for misplaced in [
        "--dry-run",
        "--no-briefing",
        "--intent",
        "--task",
        "--scope",
    ] {
        if passthrough.iter().any(|arg| arg == misplaced) {
            eprintln!(
                "ohjaa: `{misplaced}` after `--` was passed to the agent, not ohjaa — \
                 ohjaa flags go before `--`"
            );
        }
    }

    // The perimeter: the agent starts knowing what system it operates in —
    // the fleet's self-declared tools plus who is live — delivered inside the
    // task so it travels in toimija's composed launch prompt. Derived here,
    // re-derivable by the agent (`ohjaa system`, `ohjaa fleet`), never stored.
    let task = if no_briefing {
        task.map(String::from)
    } else {
        let fleet = repos(roots)?;
        let view = system::system_view(&fleet);
        let perimeter = system::briefing(&view, &root);
        Some(match task {
            Some(task) => format!("{task}\n\n{perimeter}"),
            None => perimeter,
        })
    };

    let mut command = Command::new("toimija");
    command.arg("run").arg("--intent").arg(intent);
    if let Some(task) = &task {
        command.arg("--task").arg(task);
    }
    for scope_path in scope {
        command.arg("--scope").arg(scope_path);
    }
    command.arg("--").arg(agent).args(passthrough);
    command.current_dir(&root);
    if dry_run {
        eprintln!("ohjaa: dry-run; would exec: {}", render_command(&command));
        return Ok(0);
    }
    // Replace this process with toimija instead of wrapping it: one fewer
    // parent above the agent tree, so killing "the session" cannot leave an
    // orphaned toimija/agent pair behind, and signals aimed at this PID reach
    // toimija itself. On success exec never returns.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = command.exec();
        Err(error).context("failed to exec toimija")
    }
    #[cfg(not(unix))]
    {
        let status = command.status().context("failed to run toimija")?;
        Ok(status.code().unwrap_or(1))
    }
}

/// One command line for humans: each argument shell-quoted only when it needs
/// it. It is a rendering, not a promise — the exec'd argv is the truth.
fn render_command(command: &Command) -> String {
    #[allow(deprecated)] // get_args/get_program are the stable cross-version view
    let mut words = vec![shell_word(command.get_program().to_string_lossy().as_ref())];
    words.extend(
        command
            .get_args()
            .map(|arg| shell_word(&arg.to_string_lossy())),
    );
    words.join(" ")
}

fn shell_word(word: &str) -> String {
    if word.is_empty()
        || word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\' | '$' | '`'))
    {
        format!("'{}'", word.replace('\'', "'\\\"'"))
    } else {
        word.to_string()
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::Fleet { json } => cmd_fleet(&cli.root, *json),
        Cmd::Why { query, json } => cmd_why(&cli.root, query, *json),
        Cmd::Run {
            agent,
            intent,
            task,
            scope,
            dry_run,
            no_briefing,
            passthrough,
        } => cmd_run(RunOptions {
            agent,
            intent,
            task: task.as_deref(),
            scope,
            dry_run: *dry_run,
            no_briefing: *no_briefing,
            passthrough,
            roots: &cli.root,
        }),
        Cmd::System { json } => cmd_system(&cli.root, *json),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("ohjaa: {error:#}");
            std::process::exit(1);
        }
    }
}
