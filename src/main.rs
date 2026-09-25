//! ohjaa: steering knowledge for agents.
//!
//! Toimija reports facts and refuses to decide. Ohjaa holds the other half:
//! the graph of what the people an agent works for know and have decided
//! (faija-ontology.v2, knowledge-node.v1), and it hands that graph to every
//! agent *before* it acts, together with the live perimeter of the fleet.
//! Agents may propose Knowledge; only a person makes it current.

mod intake;
mod knowledge;
mod process;
mod roots;
mod scan;
mod system;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use faija_knowledge::{self as fk, Kind, Scope, Status};

use knowledge::{Dirs, Outcome, Proposal};
use scan::{ago, scan_fleet, scan_repo, Coverage, Scan};

/// The launch or the change was refused.
const REFUSED: i32 = 3;
/// At least one repo could not be scanned, so absence proves nothing.
const INCOMPLETE: i32 = 4;

#[derive(Parser)]
#[command(
    name = "ohjaa",
    version,
    about = "Steering knowledge for agents, handed to them before they act.",
    long_about = "Toimija reports facts and refuses to decide. Ohjaa holds what the people \
                  an agent works for know and have decided, and briefs every agent with it \
                  at launch. Agents propose; only a person makes knowledge current."
)]
struct Cli {
    /// Where to look for repositories. Repeatable; defaults to the config file,
    /// else this repo's siblings.
    #[arg(long, global = true, value_name = "PATH")]
    root: Vec<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inspect live siblings, brief the agent, then hand off to `toimija run`.
    Run {
        /// Agent command, e.g. claude / codex.
        agent: String,
        /// Why this session exists.
        #[arg(long)]
        intent: String,
        /// What to do; forwarded to `toimija run --task`.
        #[arg(long)]
        task: Option<String>,
        /// Paths this session expects to touch (repeatable).
        #[arg(long = "scope", value_name = "PATH")]
        scope: Vec<String>,
        /// Decide and explain, but do not launch.
        #[arg(long)]
        dry_run: bool,
        /// Do not brief the agent with knowledge and the perimeter.
        #[arg(long = "no-briefing")]
        no_briefing: bool,
        /// Everything after `--` is passed through to the agent command.
        #[arg(last = true)]
        passthrough: Vec<String>,
    },
    /// Print the briefing an agent gets here, or write its knowledge into
    /// instruction files for agents launched some other way.
    Brief {
        /// Only the knowledge, without the live perimeter.
        #[arg(long)]
        knowledge_only: bool,
        /// Write the knowledge as a generated block into this file (repeatable),
        /// keeping everything else in it.
        #[arg(long, value_name = "FILE")]
        write: Vec<PathBuf>,
    },
    /// Who is live, across every repo.
    Fleet {
        #[arg(long)]
        json: bool,
    },
    /// The system perimeter: what every tool in the fleet is, derived now.
    System {
        #[arg(long)]
        json: bool,
    },
    /// List the knowledge that applies here.
    List {
        /// Include superseded and retired knowledge.
        #[arg(long)]
        all: bool,
    },
    /// Propose knowledge. It waits until a person accepts it.
    Propose {
        /// ours, repo, mine or machine.
        #[arg(long)]
        scope: String,
        /// kebab-case id, unique across every scope.
        #[arg(long)]
        id: String,
        /// decision, fact, taste or lesson.
        #[arg(long)]
        kind: String,
        /// Why it exists: the correction, decision or outcome behind it (repeatable).
        #[arg(long, required = true)]
        origin: Vec<String>,
        /// The node this narrows or builds on.
        #[arg(long)]
        refines: Option<String>,
        /// Nodes this replaces (repeatable; same scope only).
        #[arg(long)]
        supersedes: Vec<String>,
        /// A fact that must stay true: `command:<name>` or `path:<path>` (repeatable).
        #[arg(long = "holds-while")]
        holds_while: Vec<String>,
        /// For repo scope: the repository, when not the current one.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// What an agent should know. `-` reads it from stdin.
        #[arg(required = true, num_args = 1..)]
        body: Vec<String>,
    },
    /// Go through proposals and decide each one (a person, at a terminal).
    Review {
        /// Only list what is waiting; decide nothing.
        #[arg(long)]
        list: bool,
    },
    /// Make a proposal current. A person only.
    Accept { id: String },
    /// Drop a proposal. A person only.
    Reject { id: String },
    /// Retire knowledge that no longer applies. It is kept, with its why.
    Retire {
        id: String,
        #[arg(long)]
        why: String,
    },
    /// Re-confirm that knowledge still holds, as of today. A person only.
    Confirm { id: String },
    /// Validate every node and flag what may no longer hold.
    Check {
        /// Briefing size, in characters, past which it is too long to be read.
        #[arg(long, default_value_t = knowledge::DEFAULT_BUDGET)]
        budget: usize,
        /// Exit non-zero on flags too, not only on errors.
        #[arg(long)]
        strict: bool,
    },
    /// Draft proposals from what people said: agent transcripts, pull request
    /// comments and reverts.
    Intake {
        /// Start here (YYYY-MM-DD or RFC 3339). Default: where the last intake
        /// stopped, else fourteen days ago.
        #[arg(long)]
        since: Option<String>,
        /// transcripts, reviews or reverts (repeatable). Default: all three.
        #[arg(long = "source", value_name = "SOURCE")]
        sources: Vec<String>,
        /// The command that drafts: it reads the prompt on stdin and prints a
        /// JSON array. Default: ~/.config/ohjaa/drafter, else `claude -p`.
        #[arg(long)]
        drafter: Option<String>,
        /// Candidates per drafter call.
        #[arg(long, default_value_t = 40)]
        batch: usize,
        /// At most this many drafter calls; the rest wait for the next run.
        #[arg(long, default_value_t = 10)]
        max_batches: usize,
        /// Show what would be sent, and send nothing.
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
    let output = process::run(&mut command, None, process::LOCAL).context("failed to run git")?;
    if !output.status.success() {
        bail!("git {} failed: {}", args.join(" "), output.failure_detail());
    }
    Ok(output.stdout_text().trim().to_string())
}

/// Whether `path` lies inside a Git work tree, i.e. in a file other people
/// may read once it is committed.
fn inside_work_tree(path: &Path) -> bool {
    let dir = match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => parent.to_path_buf(),
        None => PathBuf::from("."),
    };
    let mut existing = dir.as_path();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return false,
        }
    }
    git(&["rev-parse", "--is-inside-work-tree"], Some(existing)).is_ok_and(|out| out == "true")
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
/// the first warning, else the bare verdict.
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
                // The age is the evidence; the label only says it is inferred.
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
        println!("no possibly running sessions reported by available scans");
    }
    Ok(report_coverage(&scans))
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

// ---- knowledge --------------------------------------------------------------

fn dirs() -> Result<Dirs> {
    Dirs::resolve(git_root().as_deref())
}

/// The knowledge section of the briefing, with its problems reported on stderr.
/// Broken files and flags are said out loud; they never make the briefing
/// silently smaller.
fn knowledge_briefing(dirs: &Dirs) -> Result<String> {
    knowledge_briefing_for(dirs, &Scope::ALL)
}

fn knowledge_briefing_for(dirs: &Dirs, scopes: &[Scope]) -> Result<String> {
    let loaded = knowledge::load(dirs)?;
    for broken in &loaded.broken {
        eprintln!(
            "ohjaa: unreadable knowledge {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    for error in fk::validate_set(&loaded.nodes) {
        eprintln!("ohjaa: knowledge does not hold together: {error}");
    }
    let flags = knowledge::flags(&loaded, dirs.repo_root.as_deref(), knowledge::today());
    let text = knowledge::render_scopes(&loaded, &flags, scopes);
    if text.len() > knowledge::DEFAULT_BUDGET {
        eprintln!(
            "ohjaa: the knowledge briefing is {} characters, over the {} an agent will \
             reliably read; merge or retire some (`ohjaa check`)",
            text.len(),
            knowledge::DEFAULT_BUDGET
        );
    }
    Ok(text)
}

fn cmd_brief(roots: &[PathBuf], knowledge_only: bool, write: &[PathBuf]) -> Result<i32> {
    let dirs = dirs()?;
    if !write.is_empty() {
        for file in write {
            // A file inside a repository is read by everyone the repository
            // is shared with: only shared Knowledge goes there. One person's
            // `mine` and `machine` go only into files outside every repo.
            let (scopes, which): (&[Scope], &str) = if inside_work_tree(file) {
                (&[Scope::Ours, Scope::Repo], "shared (ours, repo)")
            } else {
                (&Scope::ALL, "all")
            };
            let knowledge = knowledge_briefing_for(&dirs, scopes)?;
            let text = if knowledge.is_empty() {
                "No knowledge recorded yet.".to_owned()
            } else {
                knowledge
            };
            let existing = match std::fs::read_to_string(file) {
                Ok(existing) => existing,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot read {}", file.display()))
                }
            };
            let updated = knowledge::write_block(&existing, &text)
                .with_context(|| format!("not writing {}", file.display()))?;
            if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(file, updated)
                .with_context(|| format!("cannot write {}", file.display()))?;
            eprintln!("ohjaa: wrote {which} knowledge into {}", file.display());
        }
        return Ok(0);
    }
    let knowledge = knowledge_briefing(&dirs)?;
    if knowledge.is_empty() {
        println!("No knowledge recorded yet (`ohjaa propose`, then `ohjaa review`).");
    } else {
        print!("{knowledge}");
    }
    if knowledge_only {
        return Ok(0);
    }
    let here = git_root().unwrap_or_else(|| PathBuf::from("."));
    let view = system::system_view(&repos(roots)?);
    println!("\n{}", system::briefing(&view, &here));
    // An incomplete perimeter is said with the exit code, as `system` does.
    Ok(view.exit_code())
}

fn scope_arg(name: &str) -> Result<Scope> {
    Scope::from_name(name)
        .with_context(|| format!("`{name}` is not a scope: ours, repo, mine or machine"))
}

fn kind_arg(name: &str) -> Result<Kind> {
    Kind::from_name(name)
        .with_context(|| format!("`{name}` is not a kind: decision, fact, taste or lesson"))
}

fn cmd_list(all: bool) -> Result<i32> {
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let mut nodes: Vec<&fk::Located> = loaded
        .nodes
        .iter()
        .filter(|l| all || l.node.status == Status::Current)
        .collect();
    nodes.sort_by(|a, b| {
        (a.scope, a.node.kind, &a.node.id).cmp(&(b.scope, b.node.kind, &b.node.id))
    });
    for located in &nodes {
        let node = &located.node;
        println!(
            "{:<8} {:<10} {:<9} {:<32} {}  {}",
            located.scope.name(),
            node.status.name(),
            node.kind.name(),
            node.id,
            node.as_of,
            node.body.lines().next().unwrap_or("")
        );
    }
    if nodes.is_empty() {
        println!("no knowledge here yet");
    }
    for broken in &loaded.broken {
        eprintln!(
            "ohjaa: unreadable {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    Ok(if loaded.broken.is_empty() { 0 } else { 1 })
}

#[allow(clippy::too_many_arguments)]
fn cmd_propose(
    scope: &str,
    id: &str,
    kind: &str,
    origin: &[String],
    refines: Option<&str>,
    supersedes: &[String],
    holds_while: &[String],
    repo: Option<&Path>,
    body: &[String],
) -> Result<i32> {
    let body = if body == ["-"] {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        text
    } else {
        body.join(" ")
    };
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let proposal = Proposal {
        scope: scope_arg(scope)?,
        repo: repo.map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())),
        id: id.to_owned(),
        kind: kind_arg(kind)?,
        body,
        origin: origin.to_vec(),
        supersedes: supersedes.to_vec(),
        refines: refines.map(str::to_owned),
        holds_while: holds_while.to_vec(),
    };
    let path = knowledge::propose(&dirs, &loaded, proposal, knowledge::today())?;
    println!("proposed {id}: waiting for a person to accept it");
    eprintln!("ohjaa: {}", path.display());
    Ok(0)
}

fn report(outcome: &Outcome) {
    for path in &outcome.written {
        println!("  wrote   {}", path.display());
    }
    for path in &outcome.removed {
        println!("  removed {}", path.display());
    }
    for checkout in &outcome.review_in {
        println!(
            "  shared knowledge changed in {}: commit it and open a pull request, so the \
             team reviews it before every agent reads it",
            checkout.display()
        );
    }
}

fn cmd_decide(action: &str, id: &str, why: Option<&str>) -> Result<i32> {
    knowledge::require_person(action)?;
    let dirs = dirs()?;
    let today = knowledge::today();
    let (outcome, done) = match action {
        "accept" => (knowledge::accept(&dirs, id, today)?, "accepted"),
        "reject" => (knowledge::reject(&dirs, id)?, "rejected"),
        "retire" => (
            knowledge::retire(&dirs, id, why.unwrap_or(""), today)?,
            "retired",
        ),
        "confirm" => (knowledge::confirm(&dirs, id, today)?, "confirmed"),
        other => bail!("unknown decision `{other}`"),
    };
    println!("{done} {id}");
    report(&outcome);
    Ok(0)
}

fn show_pending(pending: &knowledge::Pending) {
    let node = &pending.node;
    let place = match &pending.repo {
        Some(repo) => format!("{} · {}", pending.scope.name(), repo.display()),
        None => pending.scope.name().to_owned(),
    };
    println!("\n{}  [{} · {}]", node.id, node.kind.name(), place);
    for line in node.body.lines() {
        println!("    {line}");
    }
    for why in &node.origin {
        println!("    why: {why}");
    }
    if let Some(base) = &node.refines {
        println!("    refines: {base}");
    }
    if !node.supersedes.is_empty() {
        println!("    supersedes: {}", node.supersedes.join(", "));
    }
}

fn cmd_review(list: bool) -> Result<i32> {
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let (waiting, broken) = knowledge::pending(&dirs, &loaded)?;
    for broken in &broken {
        eprintln!(
            "ohjaa: unreadable proposal {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    if waiting.is_empty() {
        println!("nothing is waiting for a decision");
        return Ok(0);
    }
    if list {
        for pending in &waiting {
            show_pending(pending);
        }
        println!(
            "\n{} waiting; decide with `ohjaa review` at a terminal",
            waiting.len()
        );
        return Ok(0);
    }
    knowledge::require_person("review")?;
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let today = knowledge::today();
    for pending in &waiting {
        show_pending(pending);
        print!("  [a]ccept  [r]eject  [s]kip  [q]uit > ");
        std::io::stdout().flush()?;
        let answer = lines.next().transpose()?.unwrap_or_default();
        let result = match answer.trim() {
            "a" | "accept" => knowledge::accept(&dirs, &pending.node.id, today).map(Some),
            "r" | "reject" => knowledge::reject(&dirs, &pending.node.id).map(Some),
            "q" | "quit" => break,
            _ => Ok(None),
        };
        match result {
            Ok(Some(outcome)) => report(&outcome),
            Ok(None) => println!("  skipped"),
            Err(error) => println!("  refused: {error:#}"),
        }
    }
    Ok(0)
}

fn cmd_check(budget: usize, strict: bool) -> Result<i32> {
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let mut errors = 0;
    for broken in &loaded.broken {
        println!("error    {}: {}", broken.path.display(), broken.reason);
        errors += 1;
    }
    for error in fk::validate_set(&loaded.nodes) {
        println!("error    {error}");
        errors += 1;
    }
    let flags = knowledge::flags(&loaded, dirs.repo_root.as_deref(), knowledge::today());
    for flag in &flags {
        println!("flag     {}: {}", flag.id(), flag.describe());
    }
    let size = knowledge::render(&loaded, &flags).len();
    let over = size > budget;
    if over {
        println!("flag     the briefing is {size} characters, over the budget of {budget}: merge or retire");
    }
    let (waiting, broken_proposals) = knowledge::pending(&dirs, &loaded)?;
    for broken in &broken_proposals {
        println!(
            "error    proposal {}: {}",
            broken.path.display(),
            broken.reason
        );
        errors += 1;
    }
    let current = loaded
        .nodes
        .iter()
        .filter(|l| l.node.status == Status::Current)
        .count();
    for scope in Scope::ALL {
        match dirs.scope_dir(scope) {
            Some(dir) => println!("scope    {:<8} {}", scope.name(), dir.display()),
            None => println!("scope    {:<8} (no home here)", scope.name()),
        }
    }
    println!(
        "summary  {current} current, {} waiting for a decision, {errors} error(s), {} flag(s), briefing {size}/{budget}",
        waiting.len(),
        flags.len() + usize::from(over)
    );
    Ok(if errors > 0 || (strict && (!flags.is_empty() || over)) {
        1
    } else {
        0
    })
}

fn default_drafter() -> String {
    knowledge::config_home()
        .ok()
        .and_then(|config| std::fs::read_to_string(config.join("ohjaa").join("drafter")).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "claude -p".to_owned())
}

#[allow(clippy::too_many_arguments)]
fn cmd_intake(
    roots: &[PathBuf],
    since: Option<&str>,
    sources: &[String],
    drafter: Option<&str>,
    batch: usize,
    max_batches: usize,
    dry_run: bool,
) -> Result<i32> {
    let dirs = dirs()?;
    let started = intake::now_seconds() * 1000;
    let since = match since {
        Some(since) => intake::normalize_since(since)?,
        None => match intake::read_cursor(&dirs)? {
            Some(cursor) => cursor,
            None => started - 14 * 86_400_000,
        },
    };
    let sources: Vec<&str> = if sources.is_empty() {
        vec!["transcripts", "reviews", "reverts"]
    } else {
        sources.iter().map(String::as_str).collect()
    };
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();
    let git_repos = || -> Result<Vec<PathBuf>> {
        let roots = roots::load(roots, git_root().as_deref());
        roots::git_repos(&roots)
    };
    for source in &sources {
        match *source {
            "transcripts" => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .context("HOME is not set")?;
                candidates.extend(intake::transcripts(&home, since, &mut warnings));
            }
            "reviews" => candidates.extend(intake::reviews(&git_repos()?, since, &mut warnings)),
            "reverts" => candidates.extend(intake::reverts(&git_repos()?, since, &mut warnings)),
            other => bail!("`{other}` is not a source: transcripts, reviews or reverts"),
        }
    }
    for warning in &warnings {
        eprintln!("ohjaa: intake: {warning}");
    }
    candidates.sort_by_key(|candidate| candidate.at_ms);
    let batch = batch.max(1);
    let limit = batch.saturating_mul(max_batches);
    let taken = candidates.len().min(limit);
    eprintln!(
        "ohjaa: {} candidate(s) since {}; drafting from {taken}",
        candidates.len(),
        intake::rfc3339(since.div_euclid(1000))
    );
    if candidates.is_empty() {
        if !dry_run && warnings.is_empty() {
            intake::write_cursor(&dirs, started)?;
        }
        println!("nothing new to draft from");
        return Ok(0);
    }
    let loaded = knowledge::load(&dirs)?;
    let drafter = drafter.map(str::to_owned).unwrap_or_else(default_drafter);
    if dry_run {
        let first = &candidates[..candidates.len().min(batch)];
        println!("{}", intake::prompt(&loaded, first));
        eprintln!(
            "ohjaa: dry-run; would send {} batch(es) to `{drafter}`",
            taken.div_ceil(batch)
        );
        return Ok(0);
    }

    let today = knowledge::today();
    let (mut proposed, mut refused) = (0, 0);
    let mut completed_through: Option<i64> = None;
    let mut drafted = 0usize;
    for chunk in candidates[..taken].chunks(batch) {
        // Re-read each time, so a draft from an earlier batch counts as existing.
        let loaded = knowledge::load(&dirs)?;
        let drafts = match intake::draft(&drafter, &intake::prompt(&loaded, chunk)) {
            Ok(drafts) => drafts,
            Err(error) => {
                eprintln!("ohjaa: intake stopped: {error:#}");
                break;
            }
        };
        for draft in drafts {
            let id = draft.id.clone();
            let result = draft
                .into_proposal()
                .and_then(|proposal| knowledge::propose(&dirs, &loaded, proposal, today));
            match result {
                Ok(_) => {
                    proposed += 1;
                    println!("proposed {id}");
                }
                Err(error) => {
                    refused += 1;
                    eprintln!("ohjaa: draft `{id}` refused: {error:#}");
                }
            }
        }
        completed_through = chunk.last().map(|c| c.at_ms);
        drafted += chunk.len();
    }
    // Move the cursor only past what was actually drafted from, so an
    // interrupted or partial run resumes instead of skipping. It never moves
    // backwards (`write_cursor` refuses to).
    // Finished only when every candidate went through the drafter: a batch
    // that failed part-way leaves the rest for the next run.
    let finished_all = drafted == candidates.len();
    if let Some(through) = completed_through {
        let cursor = if finished_all && warnings.is_empty() {
            started
        } else {
            through
        };
        intake::write_cursor(&dirs, cursor)?;
    }
    println!(
        "{proposed} proposal(s) waiting for review (`ohjaa review`), {refused} draft(s) refused"
    );
    Ok(if finished_all { 0 } else { INCOMPLETE })
}

// ---- launching --------------------------------------------------------------

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

    // A footgun guard: ohjaa flags after `--` belong to the agent.
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

    // The briefing: knowledge first (what to know before acting), then the
    // perimeter (who and what is around). Delivered inside the task so it
    // travels in toimija's composed launch prompt.
    let task = if no_briefing {
        task.map(String::from)
    } else {
        let knowledge = knowledge_briefing(&Dirs::resolve(Some(&root))?)?;
        let fleet = repos(roots)?;
        let view = system::system_view(&fleet);
        let perimeter = system::briefing(&view, &root);
        let briefing = if knowledge.is_empty() {
            perimeter
        } else {
            format!("{knowledge}\n{perimeter}")
        };
        Some(match task {
            Some(task) => format!("{task}\n\n{briefing}"),
            None => briefing,
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
    // parent above the agent tree, and signals aimed at this PID reach toimija.
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
    let mut words = vec![shell_word(command.get_program().to_string_lossy().as_ref())];
    words.extend(
        command
            .get_args()
            .map(|arg| shell_word(&arg.to_string_lossy())),
    );
    words.join(" ")
}

/// Quote a word so `sh` reads it back unchanged.
fn shell_word(word: &str) -> String {
    // Anything outside a small, always-literal set is quoted.
    let literal = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '=' | '@' | '%' | '+' | ',')
    };
    if word.is_empty() || !word.chars().all(literal) {
        format!("'{}'", word.replace('\'', "'\\''"))
    } else {
        word.to_string()
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
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
        Cmd::Brief {
            knowledge_only,
            write,
        } => cmd_brief(&cli.root, *knowledge_only, write),
        Cmd::Fleet { json } => cmd_fleet(&cli.root, *json),
        Cmd::System { json } => cmd_system(&cli.root, *json),
        Cmd::List { all } => cmd_list(*all),
        Cmd::Propose {
            scope,
            id,
            kind,
            origin,
            refines,
            supersedes,
            holds_while,
            repo,
            body,
        } => cmd_propose(
            scope,
            id,
            kind,
            origin,
            refines.as_deref(),
            supersedes,
            holds_while,
            repo.as_deref(),
            body,
        ),
        Cmd::Review { list } => cmd_review(*list),
        Cmd::Accept { id } => cmd_decide("accept", id, None),
        Cmd::Reject { id } => cmd_decide("reject", id, None),
        Cmd::Retire { id, why } => cmd_decide("retire", id, Some(why)),
        Cmd::Confirm { id } => cmd_decide("confirm", id, None),
        Cmd::Check { budget, strict } => cmd_check(*budget, *strict),
        Cmd::Intake {
            since,
            sources,
            drafter,
            batch,
            max_batches,
            dry_run,
        } => cmd_intake(
            &cli.root,
            since.as_deref(),
            sources,
            drafter.as_deref(),
            *batch,
            *max_batches,
            *dry_run,
        ),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("ohjaa: {error:#}");
            // A refusal to act (a person-only change, a set that would not
            // hold) is the one error with its own code.
            let refused = error
                .chain()
                .any(|cause| cause.downcast_ref::<knowledge::Refused>().is_some());
            std::process::exit(if refused { REFUSED } else { 1 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words_read_back_unchanged() {
        assert_eq!(shell_word("plain"), "plain");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("don't"), r"'don'\''t'");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word("a;b"), "'a;b'");
        for special in ["a[1]", "{x}", "a!b", "'*'", "~"] {
            assert!(shell_word(special).starts_with('\''), "{special}");
        }
    }
}
