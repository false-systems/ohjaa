//! ohjaa: steering knowledge for agents.
//!
//! Toimija reports facts and refuses to decide. Ohjaa holds the other half:
//! the graph of what the people an agent works for know and have decided
//! (faija-ontology.v2, knowledge-node.v1), and it hands that graph to every
//! agent *before* it acts, together with the live perimeter of the fleet.
//! Agents may propose Knowledge; only a person makes it current.

mod intake;
mod knowledge;
mod lifecycle;
mod process;
mod roots;
mod scan;
mod sessions;
mod system;
mod usage;

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
/// The most a briefing may be: it travels as one command-line argument, to
/// toimija and on to the agent, and Linux caps one at 128 KiB.
const MAX_BRIEFING: usize = 100 * 1024;

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
        /// Import an agent-usage.v1 receipt written by the launched command at exit.
        /// The path is passed to the child as OHJAA_USAGE_FILE. No file is required at launch.
        #[arg(long)]
        usage_file: Option<PathBuf>,
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
        /// Include knowledge and context for these repositories (repeatable).
        #[arg(long, value_name = "PATH", conflicts_with = "write")]
        scope: Vec<String>,
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
    /// State 2, from inside a session: read back what you know before the
    /// first change. The goal in one line, the rules that apply, who else is
    /// working.
    Oriented {
        #[arg(required = true, num_args = 1..)]
        readback: Vec<String>,
    },
    /// State 5, from inside a session: the handover for whoever comes next.
    /// Write it before you finish. Run it again to add or change a field: each
    /// field given replaces only itself, `--next ''` clears one, and the fields
    /// not given are kept.
    Handover {
        /// What is not done, and where to pick it up.
        #[arg(long)]
        unfinished: Option<String>,
        /// What the next agent should know.
        #[arg(long)]
        next: Option<String>,
        /// What caught you out that the briefing did not say.
        #[arg(long)]
        surprised: Option<String>,
    },
    /// Show a session record: the latest one here, or one by id.
    Session { id: Option<String> },
    /// Import explicit usage attribution or export session history for analysis.
    Usage {
        #[command(subcommand)]
        action: UsageCmd,
    },
    /// A knowledge node and its neighbours: what it builds on, what builds on
    /// it, what it replaced, and which sessions were briefed with it.
    Graph { id: String },
}

#[derive(Subcommand)]
enum UsageCmd {
    /// Attach a native-session snapshot. Reimports replace, never add totals.
    Import {
        file: PathBuf,
        /// Wrapper session ID; defaults to OHJAA_SESSION inside a run.
        #[arg(long)]
        session: Option<String>,
    },
    /// Versioned JSON export; no database access needed by consumers.
    Export {
        #[arg(long)]
        session: Option<String>,
    },
}

/// Rows of the session log that could not be read while checking a usage
/// receipt was not attributed twice, named so they can be looked at.
fn report_unreadable_rows(ids: &[String]) {
    if !ids.is_empty() {
        eprintln!(
            "ohjaa: session log row(s) {} could not be read; skipped when checking this \
             receipt is not attributed to another session",
            ids.join(", ")
        );
    }
}

fn cmd_usage(action: &UsageCmd) -> Result<i32> {
    let dirs = dirs()?;
    let open = || sessions::Store::open(&sessions::store_path(&dirs.state));
    match action {
        UsageCmd::Import { file, session } => {
            let id = session.clone().map(Ok).unwrap_or_else(current_session_id)?;
            anyhow::ensure!(sessions::is_valid_id(&id), "invalid session id");
            let skipped = open()?.attach_usage(&id, usage::Receipt::read(file)?)?;
            report_unreadable_rows(&skipped);
            println!("recorded usage snapshot for {id}");
            Ok(0)
        }
        UsageCmd::Export { session } => {
            // Read, then let the log go before writing: a reader that stops
            // reading must never hold every other ohjaa off the log.
            // With no log yet there is nothing to export, and none is created.
            let (mut records, unreadable) = if sessions::store_path(&dirs.state).exists() {
                open()?.all()?
            } else {
                (Vec::new(), Vec::new())
            };
            if let Some(id) = session {
                records.retain(|s| s.id == *id);
                anyhow::ensure!(
                    !unreadable.contains(id),
                    "session {id} is unreadable in the log"
                );
                anyhow::ensure!(!records.is_empty(), "session {id} not found");
            }
            // One session asked for and found is complete, whatever other rows hold.
            let complete = unreadable.is_empty() || session.is_some();
            let mut out = serde_json::to_vec_pretty(&serde_json::json!({
                "schema": "ohjaa-sessions.v1", "sessions": records, "unreadable": unreadable
            }))?;
            out.push(b'\n');
            std::io::stdout().lock().write_all(&out)?;
            Ok(if complete { 0 } else { INCOMPLETE })
        }
    }
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

    let repos = scans.iter().filter(|s| !s.outside);
    let seen = repos
        .clone()
        .filter(|s| s.coverage == Coverage::Available)
        .count();
    let outside_seen = scans
        .iter()
        .any(|s| s.outside && s.coverage == Coverage::Available);

    // stdout is block-buffered when piped; keep the report in order.
    let _ = std::io::stdout().flush();
    eprintln!(
        "\ncoverage: scanned {seen}/{} repo(s){}",
        repos.count(),
        if outside_seen {
            " and the sessions outside repositories"
        } else {
            ""
        }
    );
    if incomplete.is_empty() {
        return 0;
    }
    eprintln!("  incomplete — absence here is not evidence of absence:");
    for scan in incomplete {
        eprintln!("    {}: {}", scan.name(), coverage_detail(scan));
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
    scans.sort_by_key(|scan| (scan.outside, scan.name()));

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
        println!("{}", scan.name());
        if let Some(error) = &scan.error {
            println!("    ! scan failed: {error}");
        }
        for session in live {
            println!("    {}  {}", session.agent, session.id);
            // Outside a repo there is no branch: where it works is the folder.
            // In one, toimija's marker keeps the branch the session entered on
            // (later moves show as drift in its packet), so say "at launch":
            // a session that has since switched branches is not on it now.
            let place = if scan.outside {
                format!("folder: {}", session.worktree.display())
            } else {
                format!(
                    "branch at launch: {}",
                    session.branch.as_deref().unwrap_or("detached")
                )
            };
            println!(
                "      {place}   seen: {}{}",
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
/// The knowledge section, and exactly which nodes it delivers.
fn knowledge_briefing_for(
    dirs: &Dirs,
    scopes: &[Scope],
) -> Result<(String, Vec<sessions::Delivered>)> {
    let loaded = knowledge::load(dirs)?;
    let delivered = fk::briefing(&loaded.nodes)
        .into_iter()
        .filter(|located| scopes.contains(&located.scope))
        .map(|located| sessions::Delivered {
            id: located.node.id.clone(),
            scope: located.scope.name().to_owned(),
            as_of: located.node.as_of.to_string(),
            repo: (located.scope == Scope::Repo)
                .then(|| dirs.repo_root.as_ref().map(|p| repo_key(p)))
                .flatten(),
        })
        .collect();
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
    Ok((text, delivered))
}

/// Shared/personal knowledge once, plus the complete knowledge of each named repo.
/// No keyword filter is allowed to hide a binding decision.
fn knowledge_for_work(
    dirs: &Dirs,
    scoped: &[PathBuf],
) -> Result<(String, Vec<sessions::Delivered>)> {
    let (mut text, mut delivered) = knowledge_briefing_for(dirs, &Scope::ALL)?;
    for repo in scoped {
        let mut scoped_dirs = dirs.clone();
        scoped_dirs.repo_root = Some(repo.clone());
        let (knowledge, nodes) = knowledge_briefing_for(&scoped_dirs, &[Scope::Repo])?;
        if !knowledge.is_empty() {
            text.push_str(&format!(
                "\nScoped repository {}:\n{knowledge}",
                repo.display()
            ));
        }
        delivered.extend(nodes);
    }
    if !scoped.is_empty() && text.len() > knowledge::DEFAULT_BUDGET {
        eprintln!("ohjaa: combined scoped knowledge briefing is {} characters, over {}; merge or retire nodes, never truncate", text.len(), knowledge::DEFAULT_BUDGET);
    }
    Ok((text, delivered))
}

fn selected_perimeter(
    root: &Path,
    in_repo: bool,
    scoped: &[PathBuf],
    mut scans: Vec<Scan>,
    mut outside: Scan,
) -> system::SystemView {
    let selected: Vec<_> = in_repo
        .then(|| root.to_path_buf())
        .into_iter()
        .chain(scoped.iter().cloned())
        .collect();
    let folder = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    outside.sessions.retain(|s| {
        std::fs::canonicalize(&s.worktree).is_ok_and(|p| p == folder)
            || selected.iter().any(|repo| scope_reaches(s, repo))
    });
    scans.push(outside);
    system::scoped_view(&selected, scans)
}

fn scoped_perimeter_text(view: &system::SystemView, here: &Path) -> String {
    format!("{}\n\nOnly the launch repository, declared scope and sessions reaching them are included.\nOther repositories were not scanned. Use `ohjaa system` or `ohjaa fleet` for a\nfleet-wide question; after expanding scope, use `ohjaa brief --scope <path>`.\nUse the delivered facts instead of repeating discovery. Refresh session observations\nbefore editing if work has moved on since this briefing.\n", system::briefing(view, here))
}

fn cmd_brief(
    roots: &[PathBuf],
    knowledge_only: bool,
    write: &[PathBuf],
    scope: &[String],
) -> Result<i32> {
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
            let (knowledge, _) = knowledge_briefing_for(&dirs, scopes)?;
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
    let (scoped, unscoped) = scoped_repos(scope, dirs.repo_root.as_deref());
    let (knowledge, _) = knowledge_for_work(&dirs, &scoped)?;
    for path in &unscoped {
        eprintln!("ohjaa: --scope {path} is not inside a Git repository; no repository knowledge or context was read there");
    }
    if knowledge.is_empty() {
        println!("No knowledge recorded yet (`ohjaa propose`, then `ohjaa review`).");
    } else {
        print!("{knowledge}");
    }
    if knowledge_only {
        return Ok(if unscoped.is_empty() { 0 } else { INCOMPLETE });
    }
    let here = dirs.repo_root.clone().unwrap_or(std::env::current_dir()?);
    if let Some(last) = last_session_here(&dirs, &here) {
        print!("\n{last}");
    }
    // Validate configured discovery roots; do not derive unrelated repositories.
    repos(roots)?;
    let scans = dirs
        .repo_root
        .iter()
        .chain(&scoped)
        .map(|p| scan_repo(p))
        .collect();
    let view = selected_perimeter(
        &here,
        dirs.repo_root.is_some(),
        &scoped,
        scans,
        scan::scan_outside(),
    );
    println!("\n{}", scoped_perimeter_text(&view, &here));
    // An incomplete perimeter is said with the exit code, as `system` does.
    Ok(if unscoped.is_empty() {
        view.exit_code()
    } else {
        INCOMPLETE
    })
}

// ---- the session log ---------------------------------------------------------

/// Open the session log, or say why not. The log is never a reason to stop.
fn session_store(dirs: &Dirs) -> Option<sessions::Store> {
    match sessions::Store::open(&sessions::store_path(&dirs.state)) {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!("ohjaa: {error}; carrying on without the session log");
            None
        }
    }
}

fn repo_key(repo: &Path) -> String {
    std::fs::canonicalize(repo)
        .unwrap_or_else(|_| repo.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// "Last session here", when the log knows one.
fn last_session_here(dirs: &Dirs, repo: &Path) -> Option<String> {
    let store = session_store(dirs)?;
    let here = repo_key(repo);
    match store.latest_for(&here) {
        Ok(Some(session)) => Some(sessions::render_last(&session, &here)),
        Ok(None) => None,
        Err(error) => {
            eprintln!("ohjaa: {error}");
            None
        }
    }
}

/// The session this command runs inside, named by `ohjaa run`.
fn current_session_id() -> Result<String> {
    let id = std::env::var(sessions::SESSION_ENV).map_err(|_| {
        anyhow::anyhow!(
            "not inside an ohjaa session: start the agent with `ohjaa run` (it sets {})",
            sessions::SESSION_ENV
        )
    })?;
    if !sessions::is_valid_id(&id) {
        bail!("{} is not a session id: `{id}`", sessions::SESSION_ENV);
    }
    Ok(id)
}

/// Record something the agent says about its own session.
fn record_testimony(what: &str, change: impl FnOnce(&mut sessions::Session)) -> Result<i32> {
    let id = current_session_id()?;
    let dirs = dirs()?;
    let store = sessions::Store::open(&sessions::store_path(&dirs.state))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    match store
        .update(&id, change)
        .map_err(|error| anyhow::anyhow!("{error}"))?
    {
        Some(_) => {
            println!("recorded the {what} for session {id}");
            Ok(0)
        }
        None => bail!("session {id} is not in the log (its entering was not recorded)"),
    }
}

fn cmd_oriented(readback: &[String]) -> Result<i32> {
    let text = readback.join(" ").trim().to_owned();
    if text.is_empty() {
        bail!("say what you know: the goal in one line, the rules that apply, who else is working");
    }
    let at_ms = intake::now_seconds() * 1000;
    record_testimony("read-back", move |session| {
        session.readback = Some(sessions::Readback { at_ms, text });
    })
}

fn cmd_handover(
    unfinished: Option<&str>,
    next: Option<&str>,
    surprised: Option<&str>,
) -> Result<i32> {
    if unfinished.is_none() && next.is_none() && surprised.is_none() {
        bail!("a handover needs at least one of --unfinished, --next or --surprised");
    }
    // A field given replaces only itself; given as '' it is cleared; a field
    // not given is kept. So a handover can be written as the work goes.
    let apply = |field: &mut Option<String>, given: Option<&str>| {
        if let Some(given) = given {
            let given = given.trim();
            *field = (!given.is_empty()).then(|| given.to_owned());
        }
    };
    let at_ms = intake::now_seconds() * 1000;
    let (unfinished, next, surprised) = (
        unfinished.map(str::to_owned),
        next.map(str::to_owned),
        surprised.map(str::to_owned),
    );
    record_testimony("handover", move |session| {
        let handover = session.handover.get_or_insert_with(Default::default);
        handover.at_ms = at_ms;
        apply(&mut handover.unfinished, unfinished.as_deref());
        apply(&mut handover.next, next.as_deref());
        apply(&mut handover.surprised, surprised.as_deref());
    })
}

fn cmd_session(id: Option<&str>) -> Result<i32> {
    let dirs = dirs()?;
    let store = sessions::Store::open(&sessions::store_path(&dirs.state))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let session = match id {
        Some(id) => store.get(id).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => {
            // "Here" is the repository, or outside one the folder, as for `run`.
            let repo = match git_root() {
                Some(repo) => repo,
                None => std::env::current_dir().context("failed to read the current directory")?,
            };
            let key = repo_key(&repo);
            let latest = store.latest_for(&key).map_err(|e| anyhow::anyhow!("{e}"))?;
            if latest.is_none() {
                // Only ended sessions are the latest here; say what else is
                // recorded rather than "none".
                let (all, unreadable) = store.all().map_err(|e| anyhow::anyhow!("{e}"))?;
                let unended: Vec<_> = all
                    .into_iter()
                    .filter(|s| {
                        s.left.is_none()
                            && (s.repo == key || s.scoped.iter().any(|scoped| scoped.repo == key))
                    })
                    .collect();
                print!(
                    "{}",
                    sessions::render_none_ended(&unended, unreadable.len())
                );
                return Ok(1);
            }
            latest
        }
    };
    match session {
        Some(session) => {
            print!("{}", sessions::render_record(&session));
            Ok(0)
        }
        None => {
            println!("no session recorded with that id");
            Ok(1)
        }
    }
}

fn cmd_graph(id: &str) -> Result<i32> {
    use std::fmt::Write as _;
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let Some(node) = loaded.get(id) else {
        bail!("no knowledge named `{id}` here");
    };
    let describe = |located: &fk::Located| {
        format!(
            "{} [{} · {} · {} · as of {}]",
            located.node.id,
            located.scope.name(),
            located.node.kind.name(),
            located.node.status.name(),
            located.node.as_of
        )
    };
    let mut out = String::new();
    let _ = writeln!(out, "{}", describe(node));
    for line in node.node.body.lines() {
        let _ = writeln!(out, "    {line}");
    }
    // What it builds on, all the way up; the files are the graph.
    let mut chain = Vec::new();
    let mut cursor = node.node.refines.clone();
    while let Some(base) = cursor {
        if chain.contains(&base) || chain.len() > 32 {
            break;
        }
        cursor = loaded.get(&base).and_then(|l| l.node.refines.clone());
        chain.push(base);
    }
    let _ = writeln!(
        out,
        "builds on     {}",
        if chain.is_empty() {
            "—".to_owned()
        } else {
            chain.join(" → ")
        }
    );
    let built_on: Vec<&str> = loaded
        .nodes
        .iter()
        .filter(|l| l.node.refines.as_deref() == Some(id))
        .map(|l| l.node.id.as_str())
        .collect();
    let _ = writeln!(
        out,
        "built on by   {}",
        if built_on.is_empty() {
            "—".to_owned()
        } else {
            built_on.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "replaced      {}",
        if node.node.supersedes.is_empty() {
            "—".to_owned()
        } else {
            node.node.supersedes.join(", ")
        }
    );
    let replaced_by: Vec<&str> = loaded
        .nodes
        .iter()
        .filter(|l| l.node.supersedes.iter().any(|s| s == id))
        .map(|l| l.node.id.as_str())
        .collect();
    let _ = writeln!(
        out,
        "replaced by   {}",
        if replaced_by.is_empty() {
            "—".to_owned()
        } else {
            replaced_by.join(", ")
        }
    );
    if let Some(store) = session_store(&dirs) {
        if let Ok((all, _)) = store.all() {
            let briefed: Vec<&sessions::Session> = all
                .iter()
                .rev()
                .filter(|s| s.delivered.iter().any(|d| d.id == id))
                .collect();
            let recent: Vec<String> = briefed
                .iter()
                .take(5)
                .map(|s| {
                    let repo = Path::new(&s.repo)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    format!("{} ({repo})", s.id)
                })
                .collect();
            let _ = writeln!(
                out,
                "briefed in    {} session(s){}{}",
                briefed.len(),
                if recent.is_empty() { "" } else { ", latest: " },
                recent.join(", ")
            );
        }
    }
    print!("{out}");
    Ok(0)
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
    // Sessions that ended and were never drafted from: the agent's own
    // handover, and what was said in that session's window, even when the
    // window lies before the cursor. Read in one short open of the log; no
    // store is held across the slow calls below.
    let queued: Vec<sessions::Session> = session_store(&dirs)
        .and_then(|store| store.all().ok())
        .map(|(all, _)| {
            all.into_iter()
                .filter(|s| s.left.is_some() && !s.drafted)
                .collect()
        })
        .unwrap_or_default();
    // A session is drafted as a whole, its handover with its transcript
    // window, so only a run that reads transcripts takes it on.
    let reads_transcripts = sources.contains(&"transcripts");
    let queued = if reads_transcripts {
        queued
    } else {
        if !queued.is_empty() {
            eprintln!(
                "ohjaa: {} ended session(s) wait for a run that reads transcripts",
                queued.len()
            );
        }
        Vec::new()
    };
    let mut warnings = Vec::new();
    let mut transcripts_read_cleanly = false;
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
                // One pass, from the earliest point anything needs: the cursor,
                // or the start of the oldest queued session.
                let from = queued
                    .iter()
                    .map(|s| s.started_ms - 1)
                    .min()
                    .map_or(since, |earliest| earliest.min(since));
                let before = warnings.len();
                let read = intake::transcripts(&home, from, &mut warnings);
                transcripts_read_cleanly = warnings.len() == before;
                let (new, older): (Vec<_>, Vec<_>) = read
                    .into_iter()
                    .partition(|candidate| candidate.at_ms > since);
                candidates.extend(new);
                candidates.extend(intake::within_sessions(older, &queued));
            }
            "reviews" => candidates.extend(intake::reviews(&git_repos()?, since, &mut warnings)),
            "reverts" => candidates.extend(intake::reverts(&git_repos()?, since, &mut warnings)),
            other => bail!("`{other}` is not a source: transcripts, reviews or reverts"),
        }
    }
    if !queued.is_empty() {
        candidates.extend(intake::handover_candidates(&queued));
        eprintln!(
            "ohjaa: {} ended session(s) queued for drafting",
            queued.len()
        );
    }
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert((c.at_ms, c.text.clone())));
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
    // Drafted only when its whole window was read: a transcript that could
    // not be read leaves the session queued for the next run.
    let mark_drafted = || {
        if !transcripts_read_cleanly || queued.is_empty() {
            return;
        }
        // Opened again only for this short write, after the drafter is done.
        if let Some(store) = session_store(&dirs) {
            for session in &queued {
                if let Err(error) = store.update(&session.id, |s| s.drafted = true) {
                    eprintln!("ohjaa: {error}");
                }
            }
        }
    };
    if candidates.is_empty() {
        if !dry_run && warnings.is_empty() {
            intake::write_cursor(&dirs, started)?;
            mark_drafted();
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
    if finished_all {
        mark_drafted();
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
    usage_file: Option<&'a Path>,
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
        usage_file,
        passthrough,
        roots,
    } = options;
    let task = task.filter(|task| !task.trim().is_empty());
    // Outside any Git repository the session is the folder's: toimija keeps it
    // outside every repo, and there are no commits or pull request to observe.
    let (root, in_repo) = match git_root() {
        Some(root) => (root, true),
        None => (
            std::env::current_dir().context("failed to read the current directory")?,
            false,
        ),
    };

    let scan = if in_repo {
        scan_repo(&root)
    } else {
        scan::scan_outside()
    };
    if scan.coverage != Coverage::Available {
        eprintln!(
            "ohjaa: cannot completely read sessions {} {}: {}",
            if in_repo {
                "in"
            } else {
                "outside repositories, for"
            },
            root.display(),
            coverage_detail(&scan)
        );
        eprintln!("ohjaa: refusing to launch blind");
        return Ok(REFUSED);
    }

    // The repositories named with --scope, other than the launch one. Each is
    // observed like the launch repository: its sessions counted now (blind is
    // refused there too), its commits and pull request recorded at leaving.
    let (scoped_repos, unscoped) = scoped_repos(scope, in_repo.then_some(root.as_path()));
    let mut scoped_scans = Vec::new();
    for repo in &scoped_repos {
        let scoped_scan = scan_repo(repo);
        if scoped_scan.coverage != Coverage::Available {
            eprintln!(
                "ohjaa: cannot completely read sessions in {}, named with --scope: {}",
                repo.display(),
                coverage_detail(&scoped_scan)
            );
            eprintln!("ohjaa: refusing to launch blind");
            return Ok(REFUSED);
        }
        scoped_scans.push(scoped_scan);
    }
    // Sessions launched outside every repository count wherever their scope
    // reaches. Outside a repo, the scan above already is that list.
    let outside = if in_repo {
        scan::scan_outside()
    } else {
        scan.clone()
    };
    let outside_seen = outside.coverage == Coverage::Available;
    let reaching = |dir: &Path| {
        outside
            .occupying()
            .filter(|s| scope_reaches(s, dir))
            .count()
    };

    // Counts every session that may still be working here, not just the ones
    // toimija currently calls active: reporting an occupied checkout as empty
    // is the one thing this line must never do. Outside a repo, "here" is
    // this folder.
    let folder = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let here = |session: &&scan::Session| {
        in_repo || std::fs::canonicalize(&session.worktree).is_ok_and(|path| path == folder)
    };
    let live = scan.occupying().filter(here).count();
    let quiet = scan
        .occupying()
        .filter(here)
        .filter(|s| s.is_quiet())
        .count();
    if in_repo {
        eprintln!("ohjaa: {live} sibling session(s) in this checkout at launch");
        if !outside_seen {
            eprintln!(
                "ohjaa: could not read sessions outside repositories, so any scoped here are \
                 not counted: {}",
                coverage_detail(&outside)
            );
        } else if reaching(&folder) > 0 {
            eprintln!(
                "ohjaa: {} more, launched outside repositories, name this checkout in their --scope",
                reaching(&folder)
            );
        }
    } else {
        eprintln!(
            "ohjaa: {} is not a Git repository; {live} sibling session(s) in this folder at launch",
            root.display()
        );
        if scoped_repos.is_empty() {
            eprintln!(
                "ohjaa: no --scope names a repository, so nothing this agent does in one is \
                 observed"
            );
        }
    }
    for (repo, scoped_scan) in scoped_repos.iter().zip(&scoped_scans) {
        let from_outside = if outside_seen { reaching(repo) } else { 0 };
        eprintln!(
            "ohjaa: scoped {}: {} session(s) live there{} at launch; its commits and pull \
             request will be recorded",
            repo.file_name().unwrap_or_default().to_string_lossy(),
            scoped_scan.occupying().count(),
            if from_outside > 0 {
                format!(", plus {from_outside} from outside repositories")
            } else {
                String::new()
            }
        );
    }
    for raw in &unscoped {
        eprintln!("ohjaa: --scope {raw} is not inside a Git repository; nothing there is observed");
    }
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
        "--usage-file",
    ] {
        if passthrough.iter().any(|arg| arg == misplaced) {
            eprintln!(
                "ohjaa: `{misplaced}` after `--` was passed to the agent, not ohjaa — \
                 ohjaa flags go before `--`"
            );
        }
    }

    // State 1, entering. The briefing: knowledge first (what to know before
    // acting), then the last session here and how this one works, then the
    // perimeter (who and what is around). Keep it separate from a task so an
    // interactive Codex launch can wait for the person's first message.
    let dirs = Dirs::resolve(in_repo.then_some(root.as_path()))?;
    let started_ms = intake::now_seconds() * 1000;
    let session_id = sessions::new_id(started_ms);
    let mut delivered = Vec::new();
    let briefing = if no_briefing {
        None
    } else {
        let (knowledge, nodes) = knowledge_for_work(&dirs, &scoped_repos)?;
        delivered = nodes;
        let last = last_session_here(&dirs, &root).unwrap_or_default();
        repos(roots)?;
        let scans = in_repo
            .then(|| scan.clone())
            .into_iter()
            .chain(scoped_scans.iter().cloned())
            .collect();
        let view = selected_perimeter(&root, in_repo, &scoped_repos, scans, outside.clone());
        let perimeter = scoped_perimeter_text(&view, &root);
        let how = lifecycle_briefing(&root, in_repo, &scoped_repos);
        let parts: Vec<&str> = [
            knowledge.as_str(),
            last.as_str(),
            how.as_str(),
            perimeter.as_str(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();
        Some(parts.join("\n"))
    };
    if let Some(briefing) = briefing.as_ref().filter(|b| b.len() > MAX_BRIEFING) {
        eprintln!(
            "ohjaa: the briefing is {} KiB, over the {} KiB a briefing may be: it is one \
             command-line argument to toimija and again to the agent, and Linux caps one at \
             128 KiB. Merge or retire knowledge (`ohjaa check`), name fewer repositories \
             with --scope, or launch with --no-briefing",
            briefing.len().div_ceil(1024),
            MAX_BRIEFING / 1024
        );
        eprintln!("ohjaa: refusing to launch");
        return Ok(REFUSED);
    }

    let mut command = Command::new("toimija");
    command.arg("run").arg("--intent").arg(intent);
    if let Some(task) = task {
        command.arg("--task").arg(task);
    }
    if let Some(briefing) = &briefing {
        command.arg("--briefing").arg(briefing);
    }
    for scope_path in scope {
        command.arg("--scope").arg(scope_path);
    }
    let usage_path = usage_file.map(std::path::absolute).transpose()?;
    if let Some(path) = &usage_path {
        anyhow::ensure!(
            !path.exists(),
            "usage file already exists; use a fresh path for this run"
        );
    }
    command.arg("--").arg(agent);
    // A persistent Codex tool daemon is not a child of this wrapper. Deliver
    // our identity and log location through its per-thread shell configuration.
    if std::path::Path::new(agent)
        .file_name()
        .is_some_and(|name| name == "codex")
    {
        let mut handshake = vec![
            (sessions::SESSION_ENV, std::ffi::OsString::from(&session_id)),
            ("XDG_STATE_HOME", knowledge::state_home()?.into_os_string()),
        ];
        if let Some(path) = &usage_path {
            handshake.push(("OHJAA_USAGE_FILE", path.as_os_str().to_owned()));
        }
        for (key, value) in handshake {
            let value = value.to_str().with_context(|| {
                format!("{key} is not UTF-8; cannot deliver the Codex session handshake")
            })?;
            command.arg("-c").arg(format!(
                "shell_environment_policy.set.{key}={}",
                serde_json::to_string(value)?
            ));
        }
    }
    command.args(passthrough);
    command.current_dir(&root);
    if dry_run {
        eprintln!("ohjaa: dry-run; would run: {}", render_command(&command));
        return Ok(0);
    }
    command.env(sessions::SESSION_ENV, &session_id);
    if let Some(path) = &usage_path {
        command.env("OHJAA_USAGE_FILE", path);
    }

    let mut session = sessions::Session {
        id: session_id.clone(),
        agent: agent.to_owned(),
        repo: repo_key(&root),
        outside_repo: !in_repo,
        intent: intent.to_owned(),
        started_ms,
        branch: in_repo.then(|| lifecycle::branch(&root)).flatten(),
        head_at_start: in_repo.then(|| lifecycle::head(&root)).flatten(),
        scoped: scoped_repos
            .iter()
            .map(|repo| sessions::Scoped {
                repo: repo_key(repo),
                branch: lifecycle::branch(repo),
                head_at_start: lifecycle::head(repo),
            })
            .collect(),
        delivered,
        readback: None,
        handover: None,
        left: None,
        drafted: false,
        usage: Vec::new(),
        usage_refused: None,
    };
    // From here to the end of leaving, a signal is kept, not fatal.
    lifecycle::supervise();
    if let Some(store) = session_store(&dirs) {
        if let Err(error) = store.put(&session) {
            eprintln!("ohjaa: {error}; this session will not be recorded");
        }
    }
    eprintln!("ohjaa: session {session_id} entering");

    // Stay the parent: leaving must run when the session ends, however it ends.
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            // No session ran; still, no entering is left without its leaving.
            let left = sessions::Left {
                ended_ms: intake::now_seconds() * 1000,
                exit: sessions::Exit::Unknown,
                toimija_session: None,
                phases: None,
                finished: sessions::Finished::Unknown {
                    why: format!("toimija could not be started: {error}"),
                },
                interrupted: false,
                scoped: Vec::new(),
            };
            if let Some(store) = session_store(&dirs) {
                let _ = store.update(&session_id, |s| s.left = Some(left));
            }
            lifecycle::restore_signals();
            return Err(error).context("failed to run toimija");
        }
    };
    let waited = lifecycle::wait(&mut child);

    // State 5, leaving: observe what happened, keep the agent's testimony,
    // write the record, print it. Always, whatever the exit. A signal from
    // here on cuts the observing short, never the record.
    lifecycle::leaving();
    let exit = match &waited {
        Ok(status) => lifecycle::exit_of(*status),
        Err(error) => {
            eprintln!("ohjaa: lost track of the session: {error}");
            sessions::Exit::Unknown
        }
    };
    let toimija_session =
        unless_interrupted(|| lifecycle::toimija_session(&root, !in_repo, child.id(), started_ms))
            .flatten();
    // Outside a repository the folder itself has no commits or pull request
    // to observe; the record says why, and asks neither git nor gh there.
    let (phases, finished) = if in_repo {
        observe_repo(&root, session.head_at_start.as_deref(), started_ms)
    } else {
        let finished = sessions::Finished::Unknown {
            why: "not a Git repository".to_owned(),
        };
        (None, finished)
    };
    // Every scoped repository is observed the same way as the launch one.
    let scoped = session
        .scoped
        .iter()
        .map(|scoped| {
            let (phases, finished) = observe_repo(
                Path::new(&scoped.repo),
                scoped.head_at_start.as_deref(),
                started_ms,
            );
            sessions::ScopedLeft {
                repo: scoped.repo.clone(),
                phases,
                finished,
            }
        })
        .collect();
    let left = sessions::Left {
        ended_ms: intake::now_seconds() * 1000,
        exit,
        toimija_session,
        phases,
        finished,
        interrupted: process::cancelled(),
        scoped,
    };
    // The store is opened only now, after every slow call, and held only for
    // this one write.
    let recorded = session_store(&dirs).and_then(|store| {
        match store.update(&session_id, |s| s.left = Some(left.clone())) {
            Ok(Some(updated)) => Some(updated),
            Ok(None) => {
                // Entering was not recorded (the log was busy then): write it all now.
                let mut whole = session.clone();
                whole.left = Some(left.clone());
                store.put(&whole).ok().map(|()| whole)
            }
            Err(error) => {
                eprintln!("ohjaa: {error}");
                None
            }
        }
    });
    let recorded_ok = recorded.is_some();
    session = recorded.unwrap_or_else(|| {
        let mut unrecorded = session;
        unrecorded.left = Some(left);
        unrecorded
    });
    if let Some(path) = usage_file {
        let imported = (|| -> Result<()> {
            let receipt = usage::Receipt::read(path)?;
            anyhow::ensure!(
                receipt.observed_at_ms >= started_ms,
                "usage receipt predates this run"
            );
            let store = sessions::Store::open(&sessions::store_path(&dirs.state))?;
            report_unreadable_rows(&store.attach_usage(&session_id, receipt)?);
            session = store
                .get(&session_id)?
                .context("session disappeared after usage import")?;
            Ok(())
        })();
        if let Err(error) = imported {
            eprintln!("ohjaa: usage was not imported: {error:#}; session exit is unchanged");
            // Recorded as refused, with why: never shown as "not collected".
            let why = format!("{error:#}");
            let stored = session_store(&dirs).and_then(|store| {
                store
                    .update(&session_id, |s| s.usage_refused = Some(why.clone()))
                    .ok()
                    .flatten()
            });
            session = stored.unwrap_or_else(|| {
                let mut unsaved = session;
                unsaved.usage_refused = Some(why);
                unsaved
            });
        }
    }
    let _ = std::io::stdout().flush();
    // Written with `let _`: a closed terminal must not turn printing into a
    // panic after the record is already safe.
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "\n{}", sessions::render_record(&session));
    if session.left.as_ref().is_some_and(|left| left.interrupted) {
        let _ = writeln!(
            stderr,
            "ohjaa: leaving was interrupted; the record holds what was known by then"
        );
    }
    let _ = if recorded_ok {
        writeln!(stderr, "ohjaa: recorded; queued for `ohjaa intake`")
    } else {
        writeln!(
            stderr,
            "ohjaa: this record could not be saved; it is shown above only"
        )
    };
    lifecycle::restore_signals();
    Ok(lifecycle::exit_code(exit))
}

/// Run one step of leaving's observing, unless a signal has cut leaving short.
fn unless_interrupted<T>(step: impl FnOnce() -> T) -> Option<T> {
    (!process::cancelled()).then(step)
}

/// Leaving's observation of one repository: the commits this session made
/// there, with what sykli saw on each, and the branch's pull request.
fn observe_repo(
    repo: &Path,
    head_at_start: Option<&str>,
    started_ms: i64,
) -> (Option<Vec<sessions::Phase>>, sessions::Finished) {
    let receipts = unless_interrupted(|| lifecycle::receipts_by_tree(repo)).unwrap_or_default();
    let phases =
        unless_interrupted(|| lifecycle::phases(repo, head_at_start, started_ms, &receipts))
            .flatten();
    let finished = match unless_interrupted(|| lifecycle::finished(repo, &receipts)) {
        Some(sessions::Finished::Unknown { .. }) if process::cancelled() => None,
        other => other,
    }
    .unwrap_or_else(|| sessions::Finished::Unknown {
        why: "interrupted".to_owned(),
    });
    (phases, finished)
}

/// The repositories `--scope` names, other than `launch`, each once and
/// canonical; and the scope paths that lie in no repository. A relative path
/// is taken from the current directory, `~/` from the home directory.
fn scoped_repos(scope: &[String], launch: Option<&Path>) -> (Vec<PathBuf>, Vec<String>) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let launch = launch.and_then(|path| std::fs::canonicalize(path).ok());
    let mut repos: Vec<PathBuf> = Vec::new();
    let mut unscoped = Vec::new();
    for raw in scope {
        let path = knowledge::expand_home(raw);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        match lifecycle::repo_of(&path) {
            Some(repo) if launch.as_ref() == Some(&repo) => {}
            Some(repo) => {
                if !repos.contains(&repo) {
                    repos.push(repo);
                }
            }
            None => unscoped.push(raw.clone()),
        }
    }
    (repos, unscoped)
}

/// Whether a session's declared scope reaches `dir`: a scope path inside it,
/// or one that contains it. A relative scope path is the session's own, taken
/// from its worktree.
fn scope_reaches(session: &scan::Session, dir: &Path) -> bool {
    session.scope.iter().any(|raw| {
        let path = knowledge::expand_home(raw);
        let path = if path.is_absolute() {
            path
        } else {
            session.worktree.join(path)
        };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        path.starts_with(dir) || dir.starts_with(&path)
    })
}

/// How a session works, told to the agent at entering. `sykli run` is asked
/// for only where there is a contract for it to run. Launched outside a
/// repository, the agent is told which scoped repositories are observed, and
/// that work anywhere else has to be named in its handover.
fn lifecycle_briefing(root: &Path, in_repo: bool, scoped: &[PathBuf]) -> String {
    let names = scoped
        .iter()
        .map(|repo| repo.file_name().unwrap_or_default().to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    let phases = if !in_repo && scoped.is_empty() {
        "2. This folder is not a Git repository, and no --scope named one, so ohjaa
   cannot observe your commits or pull requests. Say in your handover what you
   changed, and in which repos. Declare any repo you work in with
   `toimija intent \"<why>\" --scope <path>` so sessions there can see you."
            .to_owned()
    } else if !in_repo {
        format!(
            "2. You were launched for: {names}. Work in small phases there; end each with a
   commit, and `sykli run` where that repository has a sykli contract. ohjaa
   records your commits and pull request in each of them. Work in any other
   repository is not observed: name it in your handover."
        )
    } else if lifecycle::has_sykli_contract(root) {
        "2. Work in small phases; end each with a commit and `sykli run`.".to_owned()
    } else {
        "2. Work in small phases; end each with a commit. There is no sykli contract
   here (sykli.rs or sykli.json), so there is no `sykli run` step."
            .to_owned()
    };
    let phases = if in_repo && !scoped.is_empty() {
        format!(
            "{phases}
   Also scoped: {names}. Your commits and pull requests there are recorded too."
        )
    } else {
        phases
    };
    format!(
        "\
How this session works (ohjaa):
If no concrete task is assigned, acknowledge and wait; the wrapper has already
recorded entry. Do not run orientation, discovery or handover tools just to start.
Once a task is assigned, use this briefing and the Toimija packet as your starting
knowledge. Read the named files to resolve gaps; do not repeat repository discovery.
1. Before your first change, read back what you know:
   ohjaa oriented \"<the goal in one line; the rules that apply; who else is working>\"
{phases}
3. When finishing the assigned work or leaving the session, write the handover
   for whoever comes next (not after every conversational reply):
   ohjaa handover --unfinished \"…\" --next \"…\" --surprised \"…\"
"
    )
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
            usage_file,
            passthrough,
        } => cmd_run(RunOptions {
            agent,
            intent,
            task: task.as_deref(),
            scope,
            dry_run: *dry_run,
            no_briefing: *no_briefing,
            usage_file: usage_file.as_deref(),
            passthrough,
            roots: &cli.root,
        }),
        Cmd::Brief {
            knowledge_only,
            write,
            scope,
        } => cmd_brief(&cli.root, *knowledge_only, write, scope),
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
        Cmd::Oriented { readback } => cmd_oriented(readback),
        Cmd::Handover {
            unfinished,
            next,
            surprised,
        } => cmd_handover(unfinished.as_deref(), next.as_deref(), surprised.as_deref()),
        Cmd::Session { id } => cmd_session(id.as_deref()),
        Cmd::Usage { action } => cmd_usage(action),
        Cmd::Graph { id } => cmd_graph(id),
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
