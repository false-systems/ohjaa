//! ohjaa — the deciding layer above toimija.
//!
//! Toimija reports facts and refuses to decide: its boundaries forbid dispatch,
//! assignment, and network. That refusal is load-bearing, so the deciding lives
//! here instead. Ohjaa shells out to `toimija` and `sauma`, decides, and gets
//! out of the way. It never speaks the plugin protocol, never compiles a
//! packet, and stores nothing.

mod cycle;
mod facts;
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
    /// Reduce a cycle facts snapshot to exactly one legal next action.
    Cycle {
        #[command(subcommand)]
        command: CycleCmd,
    },
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

/// Where the sealed upstream artifacts of a cycle live. Ohjaa derives the
/// rest — repository point, sessions, gate receipts, Teko state — on demand.
#[derive(clap::Args)]
struct ArtifactArgs {
    /// Sealed teko-work.v1 artifact (`teko packet --artifact`).
    #[arg(long, requires = "decision")]
    work: Option<PathBuf>,
    /// Sealed kisko.decision.v1 artifact (`kisko decide --json`).
    #[arg(long, requires = "qa_plan")]
    decision: Option<PathBuf>,
    /// Sealed kelpo-plan.v1 artifact (`kelpo seal-plan`).
    #[arg(long = "qa-plan", requires = "work")]
    qa_plan: Option<PathBuf>,
    /// Sealed kelpo-verdict.v1 artifact; absent or missing means not yet judged.
    #[arg(long)]
    verdict: Option<PathBuf>,
}

impl ArtifactArgs {
    fn paths(&self) -> Option<facts::ArtifactPaths> {
        Some(facts::ArtifactPaths {
            work: self.work.clone()?,
            decision: self.decision.clone()?,
            qa_plan: self.qa_plan.clone()?,
            verdict: self.verdict.clone(),
        })
    }
}

#[derive(Subcommand)]
enum CycleCmd {
    /// Check that everything a cycle needs is present, and name what is not.
    Doctor,
    /// Derive and print the sealed ohjaa-cycle-facts.v1 snapshot for a cycle.
    Facts {
        /// A sealed cycle.v1 JSON document.
        #[arg(long)]
        cycle: PathBuf,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Print the one action legal for this cycle: from a supplied facts
    /// snapshot, or from facts derived now. `status` is the same question.
    #[command(visible_alias = "status")]
    Step {
        /// A sealed cycle.v1 JSON document.
        #[arg(long)]
        cycle: PathBuf,
        /// A ohjaa-cycle-facts.v1 JSON document; omit to derive facts now.
        #[arg(long, conflicts_with_all = ["work", "decision", "qa_plan", "verdict"])]
        facts: Option<PathBuf>,
        #[command(flatten)]
        artifacts: ArtifactArgs,
    },
    /// Advance the cycle: launch the one legal agent leg through
    /// `toimija run`, or — with derived facts — loop over `step`, running
    /// gates and closure through Toimija, until closed, held, or failed.
    Run {
        /// A sealed cycle.v1 JSON document.
        #[arg(long)]
        cycle: PathBuf,
        /// A ohjaa-cycle-facts.v1 JSON document; omit to derive facts and loop.
        #[arg(long, conflicts_with_all = ["work", "decision", "qa_plan", "verdict"])]
        facts: Option<PathBuf>,
        #[command(flatten)]
        artifacts: ArtifactArgs,
        /// Upper bound on reducer steps before giving up (derived mode).
        #[arg(long, default_value_t = 12)]
        max_steps: u32,
        /// Agent command, e.g. claude / codex.
        agent: String,
        /// Why this session exists.
        #[arg(long)]
        intent: String,
        /// Work for the selected leg.
        #[arg(long)]
        task: Option<String>,
        /// Paths this session expects to touch (repeatable).
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

fn verified<T: serde::de::DeserializeOwned>(path: &Path, name: &str) -> Result<T> {
    let status = Command::new("sauma")
        .arg("verify")
        .arg(path)
        .status()
        .with_context(|| format!("cannot run sauma verify for {}", path.display()))?;
    anyhow::ensure!(
        status.success(),
        "{} is not a verified Sauma artifact",
        path.display()
    );
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read {name} {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid {name} {}", path.display()))
}

fn load_cycle(cycle_path: &Path) -> Result<cycle::Cycle> {
    verified(cycle_path, "cycle")
}

fn supplied_facts(cycle: &cycle::Cycle, facts_path: &Path) -> Result<cycle::Next> {
    let facts: cycle::Facts = verified(facts_path, "cycle facts")?;
    cycle::next(cycle, &facts).map_err(anyhow::Error::msg)
}

fn derived_facts(cycle: &cycle::Cycle, artifacts: &ArtifactArgs) -> Result<facts::Derived> {
    let paths = artifacts
        .paths()
        .context("derived facts need --work, --decision, and --qa-plan (or pass --facts)")?;
    let root = git_root().context("not inside a Git repository")?;
    facts::derive(&root, cycle, &paths)
}

/// Exit code for a terminal reducer outcome. `closed` is success; `held` and
/// a non-launch action are the fleet's REFUSED; a declared failure is 1.
fn action_exit(action: &cycle::Next) -> i32 {
    match action {
        cycle::Next::Closed => 0,
        cycle::Next::Failed { .. } => 1,
        _ => REFUSED,
    }
}

/// Everything a cycle shells out to, checked before anything runs. A second
/// person should learn what is missing from this, not from a failure three
/// steps in. Required tools fail the check; optional ones are reported.
fn cmd_cycle_doctor() -> Result<i32> {
    let required = [
        ("toimija", "sessions, gates, repository points"),
        ("teko", "work, closure"),
        ("kisko", "decisions"),
        ("sauma", "envelopes, stamp, verify"),
        ("sykli", "the declared graph"),
        ("git", "the repository"),
        ("ssh-keygen", "deliberation signatures"),
        ("jq", "the cycle kit scripts"),
        ("python3", "kelpo.py"),
    ];
    let optional = [("gh", "open_pr effect detection")];
    let mut missing = Vec::new();
    for (tool, purpose) in required {
        match Command::new(tool).arg("--version").output() {
            Ok(output) if output.status.code().is_some() => {
                let version = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                println!("ok       {tool:<11} {version}  ({purpose})");
            }
            _ => {
                println!("MISSING  {tool:<11} needed for {purpose}");
                missing.push(tool);
            }
        }
    }
    for (tool, purpose) in optional {
        match Command::new(tool).arg("--version").output() {
            Ok(output) if output.status.success() => println!("ok       {tool:<11} ({purpose})"),
            _ => println!("absent   {tool:<11} optional: {purpose}"),
        }
    }
    match (
        std::env::var_os("TOIMIJA_SESSION"),
        std::env::var_os("TOIMIJA_REPO_ROOT"),
        git_root(),
    ) {
        (Some(session), Some(root), Some(here))
            if Path::new(&root)
                .canonicalize()
                .ok()
                .is_some_and(|root| here.canonicalize().ok() == Some(root)) =>
        {
            println!(
                "ok       session     {} for this repository (Teko will record Toimija's point)",
                session.to_string_lossy()
            );
        }
        (Some(_), _, _) => {
            println!("WARN     session     a Toimija session exists but not for this repository");
        }
        _ => {
            println!(
                "WARN     session     none: run under `toimija run` (or `ohjaa run`) so Teko records Toimija's point"
            );
        }
    }
    if missing.is_empty() {
        println!("ready");
        Ok(0)
    } else {
        println!("not ready: missing {}", missing.join(", "));
        Ok(REFUSED)
    }
}

fn cmd_cycle(command: &CycleCmd, roots: &[PathBuf]) -> Result<i32> {
    match command {
        CycleCmd::Doctor => cmd_cycle_doctor(),
        CycleCmd::Facts { cycle, artifacts } => {
            let cycle = load_cycle(cycle)?;
            let derived = derived_facts(&cycle, artifacts)?;
            println!("{}", serde_json::to_string(&derived.sealed)?);
            Ok(0)
        }
        CycleCmd::Step {
            cycle,
            facts,
            artifacts,
        } => {
            let cycle = load_cycle(cycle)?;
            let action = match facts {
                Some(facts) => supplied_facts(&cycle, facts)?,
                None => {
                    let derived = derived_facts(&cycle, artifacts)?;
                    eprintln!("ohjaa: facts {}", derived.sealed);
                    cycle::next(&cycle, &derived.facts).map_err(anyhow::Error::msg)?
                }
            };
            println!("{}", serde_json::to_string(&action)?);
            Ok(0)
        }
        CycleCmd::Run {
            cycle: cycle_path,
            facts,
            artifacts,
            max_steps,
            agent,
            intent,
            task,
            scope,
            dry_run,
            no_briefing,
            passthrough,
        } => {
            let cycle = load_cycle(cycle_path)?;
            // The whole leg contract, in one paragraph: which leg, what done
            // means, and the one rule about Teko. The obligations themselves
            // travel in toimija's packet (Active Teko Work); nothing here is
            // repeated from it.
            let verdict_path = artifacts
                .verdict
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<the verdict path the loop was given>".into());
            let leg_task = |leg: &str| {
                let contract = match leg {
                    "qa" => format!(
                        "You are the QA leg of cycle {}. Done means a sealed kelpo-verdict.v1 at {} \
                         that verifies and is anchored to the current repository point; tests you add \
                         must be able to fail (red at the baseline, green at the final tree). Never run \
                         `teko` yourself: Toimija holds its store, and the loop delivers your verdict.",
                        cycle.sauma.digest, verdict_path
                    ),
                    _ => format!(
                        "You are the implementation leg of cycle {}. Done means the declared write \
                         scope changed and is committed; Teko judges it, not your exit code. Never run \
                         `teko` yourself: Toimija holds its store.",
                        cycle.sauma.digest
                    ),
                };
                match task {
                    Some(task) => format!("Cycle leg: {leg}\n\n{contract}\n\n{task}"),
                    None => format!("Cycle leg: {leg}\n\n{contract}"),
                }
            };
            if let Some(facts) = facts {
                // Single shot from a supplied snapshot: launch the one leg and
                // hand the process to toimija, exactly like `ohjaa run`.
                return match supplied_facts(&cycle, facts)? {
                    cycle::Next::Launch { leg } => cmd_run(RunOptions {
                        agent,
                        intent,
                        task: Some(&leg_task(leg)),
                        scope,
                        dry_run: *dry_run,
                        no_briefing: *no_briefing,
                        passthrough,
                        roots,
                        cycle_action: Some((&cycle.sauma.digest, leg, 0)),
                        wait: false,
                    }),
                    action => {
                        println!("{}", serde_json::to_string(&action)?);
                        eprintln!("ohjaa: cycle action is not an agent launch");
                        Ok(REFUSED)
                    }
                };
            }
            // Derived mode: a loop over `step`. Every iteration re-derives the
            // facts, so a leg that another process completed meanwhile is
            // consumed, not duplicated. No state survives between iterations.
            let root = git_root().context("not inside a Git repository")?;
            for step in 0..*max_steps {
                let derived = derived_facts(&cycle, artifacts)?;
                let action = cycle::next(&cycle, &derived.facts).map_err(anyhow::Error::msg)?;
                eprintln!(
                    "ohjaa: step {step} at {} (tree {}) -> {}",
                    derived.repository_point.id,
                    derived.repository_point.tree_oid,
                    serde_json::to_string(&action)?
                );
                let status = match &action {
                    cycle::Next::Launch { leg } => {
                        let attempt = if *leg == "qa" {
                            derived.qa_attempt
                        } else {
                            derived.implementation_attempt
                        };
                        cmd_run(RunOptions {
                            agent,
                            intent,
                            task: Some(&leg_task(leg)),
                            scope,
                            dry_run: *dry_run,
                            no_briefing: *no_briefing,
                            passthrough,
                            roots,
                            cycle_action: Some((&cycle.sauma.digest, leg, attempt)),
                            wait: true,
                        })?
                    }
                    cycle::Next::RunGates => toimija_step(&root, "verify-work", *dry_run)?,
                    cycle::Next::CloseWork => {
                        deliver_verdict(&root, artifacts.verdict.as_deref(), *dry_run)?;
                        toimija_step(&root, "close-work", *dry_run)?
                    }
                    terminal => {
                        println!("{}", serde_json::to_string(terminal)?);
                        return Ok(action_exit(terminal));
                    }
                };
                if *dry_run {
                    println!("{}", serde_json::to_string(&action)?);
                    return Ok(0);
                }
                if status != 0 {
                    eprintln!("ohjaa: step {step} exited {status}; re-deriving facts");
                }
            }
            eprintln!("ohjaa: step budget of {max_steps} exhausted");
            println!(
                "{}",
                serde_json::to_string(&cycle::Next::Held {
                    reason: "step_budget"
                })?
            );
            Ok(REFUSED)
        }
    }
}

/// Hand the sealed Kelpo verdict to Teko at the current repository point.
/// Teko's store is single-process and Toimija holds it open for the whole
/// worker session, so a QA leg cannot deliver its own verdict; the loop does
/// it between legs, and Teko alone decides what the verdict discharges.
fn deliver_verdict(root: &Path, verdict: Option<&Path>, dry_run: bool) -> Result<()> {
    let Some(verdict) = verdict.filter(|path| path.exists()) else {
        return Ok(());
    };
    let point = Command::new("toimija")
        .args(["-q", "repository-point", "--json"])
        .current_dir(root)
        .output()
        .context("failed to run toimija repository-point")?;
    anyhow::ensure!(
        point.status.success(),
        "toimija repository-point failed: {}",
        String::from_utf8_lossy(&point.stderr).trim()
    );
    let point_path = std::env::temp_dir().join(format!("ohjaa-point-{}.json", std::process::id()));
    std::fs::write(&point_path, &point.stdout)?;
    let mut command = Command::new("teko");
    command
        .arg("observe-verdict")
        .arg("--verdict")
        .arg(verdict)
        .arg("--repository-point")
        .arg(&point_path)
        .current_dir(root);
    if dry_run {
        eprintln!("ohjaa: dry-run; would run: {}", render_command(&command));
    } else {
        let status = command
            .status()
            .context("failed to run teko observe-verdict")?;
        if !status.success() {
            eprintln!("ohjaa: teko did not accept the verdict; closure will say why");
        }
    }
    let _ = std::fs::remove_file(&point_path);
    Ok(())
}

/// Run one Toimija verification or closure command in the repository and
/// return its exit status. Toimija stays the authority: ohjaa never runs a
/// gate or closes work itself.
fn toimija_step(root: &Path, subcommand: &str, dry_run: bool) -> Result<i32> {
    let mut command = Command::new("toimija");
    command.arg(subcommand).current_dir(root);
    if dry_run {
        eprintln!("ohjaa: dry-run; would run: {}", render_command(&command));
        return Ok(0);
    }
    let status = command
        .status()
        .with_context(|| format!("failed to run toimija {subcommand}"))?;
    Ok(status.code().unwrap_or(1))
}

struct RunOptions<'a> {
    agent: &'a str,
    intent: &'a str,
    task: Option<&'a str>,
    scope: &'a [String],
    dry_run: bool,
    no_briefing: bool,
    passthrough: &'a [String],
    roots: &'a [PathBuf],
    cycle_action: Option<(&'a str, &'a str, u32)>,
    /// Wait for toimija and return its status instead of exec'ing into it.
    wait: bool,
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
        cycle_action,
        wait,
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
    if let Some((cycle, leg, attempt)) = cycle_action {
        command.args(["--cycle", cycle, "--cycle-leg", leg]);
        command.arg("--cycle-attempt").arg(attempt.to_string());
    }
    command.arg("--").arg(agent).args(passthrough);
    command.current_dir(&root);
    if dry_run {
        eprintln!("ohjaa: dry-run; would exec: {}", render_command(&command));
        return Ok(0);
    }
    if wait {
        // A cycle leg is one step of a loop that must observe its outcome, so
        // the process stays; the agent tree is still toimija's, not ohjaa's.
        let status = command.status().context("failed to run toimija")?;
        return Ok(status.code().unwrap_or(1));
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
        Cmd::Cycle { command } => cmd_cycle(command, &cli.root),
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
            cycle_action: None,
            wait: false,
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
