//! ohjaa: steering knowledge for agents.
//!
//! Toimija reports facts and refuses to decide. Ohjaa holds the other half:
//! the graph of what the people an agent works for know and have decided
//! (knowledge-node.v1), and it hands that graph to every
//! agent *before* it acts, together with the live perimeter of the fleet.
//! Agents may propose Knowledge; only a person makes it current.

mod apply;
mod doctor;
mod duplicates;
mod flat_toml;
mod gitconfig;
mod home;
mod instructions;
mod knowledge;
mod lessons;
mod perimeter;
mod process;
mod repo_docs;
mod scan;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use knowledge_node::{self as fk, Kind, Scope, Status};

use knowledge::{Dirs, Outcome, Proposal};
use scan::{ago, scan_repo, Coverage, Scan};

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
                  an agent works for know and have decided, and writes it into each \
                  agent's own instruction file. Agents propose; only a person makes knowledge current."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the briefing an agent gets here, or write its knowledge into
    /// instruction files for agents launched some other way.
    Brief {
        /// Only the knowledge, without the live perimeter.
        #[arg(long)]
        knowledge_only: bool,
        /// Include knowledge and context for these repositories (repeatable).
        #[arg(long, value_name = "PATH", conflicts_with = "write")]
        scope: Vec<String>,
        /// The agent the briefing is for (claude, codex, pi, opencode): what
        /// that agent's own instruction file holds current is left out,
        /// unless its environment says it will not read the file, and the
        /// briefing says where the rest comes from.
        #[arg(long, value_name = "COMMAND", conflicts_with = "write")]
        agent: Option<String>,
        /// Write the knowledge as a generated block into this file (repeatable),
        /// keeping everything else in it.
        #[arg(long, value_name = "FILE")]
        write: Vec<PathBuf>,
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
    /// Make a proposal current. A person only. A repo proposal's line then
    /// goes into its repository's AGENTS.md with `ohjaa lessons`.
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
    /// Write the repository's lessons into the AGENTS.md of the working copy
    /// it runs in (or one named), for you to commit and push: current repo
    /// knowledge in, retired and superseded out. Nothing is committed or sent.
    #[command(after_help = "Exit codes:\n  \
        0  written, or nothing to change\n  \
        1  it could not run: not in a repository, or AGENTS.md cannot be read whole\n     \
           (one that is not UTF-8 is never written over), or is a link that leads\n     \
           outside the working copy\n  \
        4  written, but a repo node could not be read: AGENTS.md keeps what it holds\n     \
           of it, and each is named\n\n\
        It writes one file, <working copy>/AGENTS.md, its `## Lessons` section alone:\n\
        one line per node, each marked with its id. A line of its id written by hand\n\
        is replaced; every other line stays as it is.")]
    Lessons {
        /// A folder of the working copy. Default: the current folder.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
        /// Say what would change, and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate every node, and flag what may no longer hold.
    Check {
        /// Briefing size, in characters, past which it is too long to be read.
        #[arg(long, default_value_t = knowledge::DEFAULT_BUDGET)]
        budget: usize,
        /// Exit non-zero on flags too, not only on errors.
        #[arg(long)]
        strict: bool,
    },
    /// Write the knowledge into each agent's own instruction file, and the
    /// ignore block into git's global excludes. Idempotent; --undo takes it
    /// all back.
    #[command(after_help = "Exit codes:\n  \
        0  applied, undone, or nothing to change\n  \
        1  a change could not be made\n  \
        4  applied, except an agent's own instruction file apply cannot write (a\n     \
           block it cannot tell from the rest): each is named, with why, on every\n     \
           apply and dry run until it is fixed by hand; `ohjaa doctor` names it too;\n     \
           or --undo did not take back all applied.toml holds, and kept the rest\n\n\
        Writes in the home (~/.config/ohjaa):\n  \
        applied.toml  what apply changes outside the home, kept before each change\n  \
        .git/         the home's history: what apply changed, one commit (made on\n                \
        the home's first commit; never pushed)\n\
        Changes outside it, and nothing else:\n  \
        git's global excludes  a block marked \"ohjaa apply\" holding .sykli/ .ohjaa/\n  \
        each agent's own       a block marked \"ohjaa:knowledge\" holding the team's, mine and machine\n  \
        instruction file       knowledge, in each that exists outside every repository:\n                         \
        ~/.claude/CLAUDE.md, ~/.codex/AGENTS.md, ~/.pi/agent/AGENTS.md,\n                         \
        ~/.config/opencode/AGENTS.md (none is created); each decision\n                         \
        rewrites it where it is\n  \
        ~/.zshrc               only taken out of it: the line an earlier apply wrote, which ran\n                         \
        `ohjaa shell-init`, a command this ohjaa does not have\n\
        --undo puts the changes back as they were, and removes applied.toml; the\n\
        history stays.\n\n\
        Checks before a push are sykli's: `sykli hook install --pre-push` in each\n\
        repository.")]
    Apply {
        /// Print every change, and make none.
        #[arg(long)]
        dry_run: bool,
        /// Take back what apply changed outside the home.
        #[arg(long)]
        undo: bool,
    },
    /// Report what differs from what `apply` would make it. Read-only.
    #[command(after_help = "Exit codes:\n  \
        0  healthy\n  \
        4  drift: a line marked DRIFT says what, and what fixes it")]
    Doctor,
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

/// What to say about an incomplete scan: the failure if there was one, else
/// the first warning, else the bare verdict.
fn coverage_detail(scan: &Scan) -> &str {
    scan.error
        .as_deref()
        .or_else(|| scan.warnings.first().map(String::as_str))
        .unwrap_or("degraded coverage")
}

// ---- knowledge --------------------------------------------------------------

fn dirs() -> Result<Dirs> {
    Dirs::resolve(git_root().as_deref())
}

/// A knowledge section of a briefing.
struct Briefed {
    text: String,
    /// Where mine and machine come from, and where this repository's lessons
    /// are, said once after all the knowledge.
    source: Option<String>,
}

/// The line that says the repository's AGENTS.md holds `count` of its
/// lessons, and `agent` reads it (`how`), so the briefing does not repeat
/// them.
fn lessons_line(count: usize, agents_md: &Path, agent: &str, how: &str) -> String {
    format!(
        "This repository's lessons ({count} node{}) are not repeated here: {agent} reads its \
         AGENTS.md ({}), which holds them in its {} section ({how}).",
        if count == 1 { "" } else { "s" },
        agents_md.display(),
        lessons::HEADING
    )
}

/// The knowledge section of the briefing for these scopes, within its
/// budget (the one line about mine and machine included), and exactly which
/// nodes it delivers. For `agent`, the agent's command and its arguments: when that agent reads its own instruction file
/// and the file holds mine and machine current, the briefing leaves out
/// what the file holds, but for a flagged node, and carries what it does
/// not; either way that is said, in one line. Broken files are reported on
/// stderr, out loud, and so is a briefing the budget cut; a flag is shown
/// beside its node. With `in_agents_md`, the repository's own lessons that
/// its AGENTS.md holds are left to it, but for a flagged one, and that is
/// said in one line too; every other repo node says why it is not left to
/// AGENTS.md (not yet in it, or in other words).
fn knowledge_briefing_for(
    dirs: &Dirs,
    scopes: &[Scope],
    agent: Option<(&str, &[String])>,
    in_agents_md: bool,
) -> Result<Briefed> {
    let loaded = knowledge::load(dirs)?;
    for broken in &loaded.broken {
        eprintln!(
            "ohjaa: unreadable knowledge {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    for left in &loaded.shadowed {
        eprintln!(
            "ohjaa: left {}: ohjaa's state holds that node, so this copy is not read; \
             take it out of the checkout",
            left.display()
        );
    }
    for error in fk::validate_set(&loaded.nodes) {
        eprintln!("ohjaa: knowledge does not hold together: {error}");
    }
    let flags = knowledge::flags(
        &loaded,
        dirs.repo_root.as_deref(),
        knowledge::today(),
        &own_commands(),
    );
    let home = instructions::OWN_FILE_SCOPES;
    let current = knowledge::in_scopes(&loaded, scopes);
    // What the agent's own file holds (the team's, mine and machine), as it
    // would hold it now, and whether it does.
    let own = agent
        .filter(|_| current.iter().any(|located| home.contains(&located.scope)))
        .and_then(|(agent, args)| {
            let compact = knowledge::render_compact(&loaded, &home, knowledge::DEFAULT_BUDGET);
            instructions::source(agent, args, &compact).map(|source| (source, compact))
        });
    let left_out = match &own {
        Some((instructions::Source::OwnFile { file, .. }, compact)) => Some((file, compact)),
        _ => None,
    };
    let flagged = |id: &str| flags.iter().any(|flag| flag.id() == id);
    // What the agent's own file holds, when the briefing leaves it to it.
    let held = |located: &fk::Located| {
        left_out.is_some_and(|(_, compact)| {
            home.contains(&located.scope) && compact.held.contains(&located.node.id)
        })
    };
    // Where each of this repository's own nodes stands: in its AGENTS.md as
    // checked out, which the agent reads, or not yet.
    let standing = scopes
        .contains(&Scope::Repo)
        .then(|| lessons::Standing::read(dirs))
        .flatten();
    let in_file: Vec<&fk::Located> = current
        .iter()
        .copied()
        .filter(|located| {
            located.scope == Scope::Repo
                && standing
                    .as_ref()
                    .is_some_and(|standing| standing.in_agents_md(&located.node))
        })
        .collect();
    // Whether the agent is shown to read that file: only then are they left
    // to it (repo_docs). An agent not named, or not known to, gets them.
    let reads = (in_agents_md && !in_file.is_empty())
        .then(|| {
            let root = dirs.repo_root.as_deref()?;
            let launch = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());
            let end = in_file
                .iter()
                .filter_map(|located| standing.as_ref()?.end_of(&located.node))
                .max()
                .unwrap_or(0);
            Some(repo_docs::reads(agent, root, &launch, end))
        })
        .flatten();
    let left_to_file = matches!(reads, Some(repo_docs::Reads::Yes { .. }));
    let in_lessons = |located: &fk::Located| {
        left_to_file && in_file.iter().any(|held| std::ptr::eq(*held, located))
    };
    let shown: Vec<&fk::Located> = current
        .iter()
        .copied()
        .filter(|located| !held(located) || flagged(&located.node.id))
        .filter(|located| !in_lessons(located) || flagged(&located.node.id))
        .collect();
    let notes: Vec<(String, String)> = shown
        .iter()
        .filter(|located| located.scope == Scope::Repo)
        .filter_map(|located| {
            let note = standing.as_ref()?.note(&located.node);
            Some((located.node.id.clone(), note.to_owned()))
        })
        .collect();
    let lessons = match (&reads, standing.as_ref()) {
        (Some(repo_docs::Reads::Yes { agent, how }), Some(standing)) => {
            Some(lessons_line(in_file.len(), &standing.agents_md, agent, how))
        }
        (Some(repo_docs::Reads::No { why }), _) => Some(format!(
            "This repository's lessons are carried here, though its AGENTS.md holds {} of them: \
             {why}.",
            in_file.len()
        )),
        _ => None,
    };
    // What AGENTS.md as checked out still says of a node retired or
    // superseded, while its removal waits: said, whatever reads the file.
    let still = standing
        .as_ref()
        .map(|standing| standing.still_said(&loaded))
        .unwrap_or_default();
    let lessons = {
        let lines: Vec<String> = lessons.into_iter().chain(still).collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    };
    // The lines are part of the section, so their room is kept first: as
    // long as the longest they can come to.
    let repeatable: Vec<&str> = shown
        .iter()
        .filter(|located| held(located))
        .map(|located| located.node.id.as_str())
        .collect();
    let room = own.as_ref().map_or(0, |(source, compact)| {
        source_line(source, compact, &repeatable, compact.more).len() + 2
    }) + lessons.as_ref().map_or(0, |line| line.len() + 2);
    let within = knowledge::render_within(
        shown,
        &flags,
        &notes,
        knowledge::DEFAULT_BUDGET.saturating_sub(room),
    );
    if let Some(over) = within.over(knowledge::DEFAULT_BUDGET) {
        eprintln!("ohjaa: {over}");
    }
    let kept = |located: &fk::Located| within.kept.iter().any(|kept| std::ptr::eq(*kept, located));
    let source = own.as_ref().map(|(source, compact)| {
        let repeated: Vec<&str> = within
            .kept
            .iter()
            .filter(|located| held(located))
            .map(|located| located.node.id.as_str())
            .collect();
        let counted = current
            .iter()
            .filter(|located| home.contains(&located.scope) && !held(located) && !kept(located))
            .count();
        source_line(source, compact, &repeated, counted)
    });
    let source = match (lessons, source) {
        (Some(lessons), Some(source)) => Some(format!("{lessons}\n{source}")),
        (lessons, source) => lessons.or(source),
    };
    Ok(Briefed {
        text: within.text,
        source,
    })
}

/// The one line about where mine and machine come from. For the agent's own
/// file: how many nodes it holds, and when it holds not all of them, that
/// the others are above, `counted` of them only by count; `repeated`, the
/// ones it holds that are above too, being flagged.
fn source_line(
    source: &instructions::Source,
    compact: &knowledge::Compact,
    repeated: &[&str],
    counted: usize,
) -> String {
    let flagged = if repeated.is_empty() {
        String::new()
    } else {
        format!(" Flagged, so repeated above: {}.", repeated.join(", "))
    };
    match source {
        instructions::Source::OwnFile { agent, file } if compact.more == 0 => format!(
            "The team's, mine and machine ({} node{}) are not repeated here: {agent} has them from its own \
             instructions, {}, whose ohjaa block is current.{flagged}",
            compact.shown,
            if compact.shown == 1 { "" } else { "s" },
            file.display()
        ),
        instructions::Source::OwnFile { agent, file } => format!(
            "The team's, mine and machine: {agent} has {} of their {} nodes from its own instructions, {}, \
             whose ohjaa block is current; the other {} {} above{}.{flagged}",
            compact.shown,
            compact.shown + compact.more,
            file.display(),
            compact.more,
            if compact.more == 1 { "is" } else { "are" },
            if counted == 0 {
                String::new()
            } else {
                format!(", {counted} of them only by count")
            }
        ),
        instructions::Source::Briefing { agent, why } => {
            format!(
                "The team's, mine and machine are carried here, not left to {agent}'s own instructions: {why}."
            )
        }
    }
}

/// Shared/personal knowledge once, plus the knowledge of each named repo,
/// then where mine and machine come from for `agent` (see
/// [`knowledge_briefing_for`]). No keyword filter is allowed to hide a
/// binding decision. The launch repository's lessons its AGENTS.md holds
/// are left to it, which the agent reads; a scoped repository's are
/// carried, since the agent is not working there and need not read its
/// AGENTS.md.
fn knowledge_for_work(
    dirs: &Dirs,
    scoped: &[PathBuf],
    agent: Option<(&str, &[String])>,
) -> Result<String> {
    let Briefed { mut text, source } = knowledge_briefing_for(dirs, &Scope::ALL, agent, true)?;
    for repo in scoped {
        let mut scoped_dirs = dirs.clone();
        scoped_dirs.repo_root = Some(repo.clone());
        let Briefed {
            text: knowledge,
            source: still,
        } = knowledge_briefing_for(&scoped_dirs, &[Scope::Repo], None, false)?;
        // The preamble is said once, before the first knowledge of all; each
        // repository adds its section, with what its AGENTS.md still says of
        // a lesson no longer current.
        let mut knowledge = knowledge
            .strip_prefix(knowledge::PREAMBLE)
            .unwrap_or(&knowledge)
            .trim_start_matches('\n')
            .to_owned();
        if let Some(still) = still {
            knowledge.push_str(&still);
            knowledge.push('\n');
        }
        if !knowledge.is_empty() {
            if text.is_empty() {
                text.push_str(knowledge::PREAMBLE);
            }
            text.push_str(&format!(
                "\nScoped repository {}:\n{knowledge}",
                repo.display()
            ));
        }
    }
    if !scoped.is_empty() && text.len() > knowledge::DEFAULT_BUDGET {
        eprintln!(
            "ohjaa: combined scoped knowledge briefing is {} characters, over {}: each \
             repository's section keeps its decisions in full within a budget of its own; \
             merge or retire nodes",
            text.len(),
            knowledge::DEFAULT_BUDGET
        );
    }
    if let Some(source) = source {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&source);
        text.push('\n');
    }
    Ok(text)
}

/// The sessions toimija sees in the other worktrees of the clone `root` is
/// a checkout of (from the clone's layout; never `folder` itself), each
/// asked as the launch checkout is, side by side.
fn clone_scans(root: &Path, folder: &Path) -> Vec<Scan> {
    let Ok(common) = gitconfig::common_dir(root) else {
        return Vec::new();
    };
    let others: Vec<PathBuf> = gitconfig::layout_checkouts(&common)
        .into_iter()
        .filter(|checkout| checkout != folder && checkout.is_dir())
        .collect();
    std::thread::scope(|scope| {
        others
            .iter()
            .map(|checkout| scope.spawn(move || scan_repo(checkout)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("scan thread panicked"))
            .collect()
    })
}

/// The briefing's part naming the sessions in the clone's other worktrees;
/// empty when there are none and every worktree could be read.
fn clone_sessions_text(scans: &[Scan]) -> String {
    use std::fmt::Write as _;
    let mut lines = String::new();
    for scan in scans {
        if scan.coverage != Coverage::Available {
            let _ = writeln!(
                lines,
                "- {}: its sessions could not be read completely ({})",
                scan.repo.display(),
                coverage_detail(scan)
            );
        }
        for session in scan.occupying() {
            let _ = writeln!(
                lines,
                "- {} ({}) in {} on {} ({}{}): {}",
                session.id,
                session.agent,
                session.worktree.display(),
                session.branch.as_deref().unwrap_or("no branch"),
                session.liveness,
                if session.idle_seconds >= 0 {
                    format!(", seen {}", ago(session.idle_seconds))
                } else {
                    String::new()
                },
                session.intent
            );
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "Sessions in the other worktrees of this clone at launch (toimija liveness; not proof of \
         current work):\n{lines}"
    )
}

fn selected_perimeter(
    root: &Path,
    in_repo: bool,
    scoped: &[PathBuf],
    mut scans: Vec<Scan>,
    mut outside: Scan,
) -> perimeter::Perimeter {
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
    perimeter::scoped_view(&selected, scans)
}

fn scoped_perimeter_text(view: &perimeter::Perimeter, here: &Path) -> String {
    format!("{}\n\nOnly the launch repository, declared scope and sessions reaching them are included.\nOther repositories were not scanned. Use `toimija sessions` in one for who is live there;\nafter expanding scope, use `ohjaa brief --scope <path>`.\nUse the delivered facts instead of repeating discovery. Refresh session observations\nbefore editing if work has moved on since this briefing.\n", perimeter::briefing(view, here))
}

fn cmd_brief(
    knowledge_only: bool,
    write: &[PathBuf],
    scope: &[String],
    agent: Option<&str>,
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
            let knowledge = knowledge_briefing_for(&dirs, scopes, None, false)?.text;
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
    let knowledge = knowledge_for_work(&dirs, &scoped, agent.map(|agent| (agent, &[][..])))?;
    for path in &unscoped {
        eprintln!("ohjaa: --scope {path} is not inside a Git repository; no repository knowledge or context was read there");
    }
    let here = dirs.repo_root.clone().unwrap_or(std::env::current_dir()?);
    if knowledge.is_empty() {
        println!("No knowledge recorded yet (`ohjaa propose`, then `ohjaa review`).");
    } else {
        print!("{knowledge}");
    }
    if knowledge_only {
        return Ok(if unscoped.is_empty() { 0 } else { INCOMPLETE });
    }
    if let Some(top) = dirs.repo_root.as_deref() {
        let folder = std::fs::canonicalize(top).unwrap_or_else(|_| top.to_path_buf());
        let siblings = clone_sessions_text(&clone_scans(top, &folder));
        if !siblings.is_empty() {
            print!("\n{siblings}");
        }
    }
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
    // An incomplete perimeter is said with the exit code.
    Ok(if unscoped.is_empty() {
        view.exit_code()
    } else {
        INCOMPLETE
    })
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
    // Where each of this repository's own current nodes stands: in its
    // AGENTS.md, or not yet.
    let standing = lessons::Standing::read(&dirs);
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
        let status = (located.scope == Scope::Repo && node.status == Status::Current)
            .then(|| standing.as_ref().map(|standing| standing.status(node)))
            .flatten();
        if let Some(status) = status {
            println!("{:<8} {status}", "");
        }
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

/// The home's lock, held across a decision: its write, its commit and the
/// agents' block, so decisions made at once are a commit each, in turn.
fn decision_lock() -> Option<process::FileLock> {
    match home::ohjaa_home() {
        Ok(home) => home::lock(&home::absolute(home)),
        Err(error) => {
            eprintln!("ohjaa: warning: no home to lock: {error:#}");
            None
        }
    }
}

/// After a person's decision on repo knowledge (an accept, a retirement, a
/// confirmation): how its line reaches the repository's AGENTS.md.
fn say_lessons(outcome: &Outcome) {
    if let Some(repo) = outcome.repo.as_deref() {
        review_text(
            "  ",
            &format!(
                "Run `ohjaa lessons` in a working copy of {} to bring its AGENTS.md in line, \
                 then commit and push it like any change.",
                repo.display()
            ),
        );
    }
}

fn cmd_decide(action: &str, id: &str, why: Option<&str>) -> Result<i32> {
    knowledge::require_person(action)?;
    let lock = decision_lock();
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
    after_decision(action, id, &outcome, why, lock.as_ref(), false);
    if action != "reject" {
        say_lessons(&outcome);
    }
    Ok(0)
}

/// What follows a person's decision, neither ever undoing it nor failing it:
/// a decision on the home's own knowledge (mine, machine) is one commit in
/// the home's history, under the home's `lock` the decision holds, and the
/// agents' own instruction files get their knowledge block rewritten where
/// it is.
fn after_decision(
    action: &str,
    id: &str,
    outcome: &Outcome,
    why: Option<&str>,
    lock: Option<&process::FileLock>,
    concise: bool,
) {
    if outcome.in_the_home() {
        let message = knowledge::decision_message(action, id, outcome, why);
        let subject = message.lines().next().unwrap_or_default();
        match home::ohjaa_home().map(home::absolute).and_then(|home| {
            home::commit(&home, &message, lock)?;
            Ok(home)
        }) {
            Ok(_) if concise => println!("  Saved in your local history."),
            Ok(home) => println!("  committed in {}: {subject}", home.display()),
            Err(error) => eprintln!(
                "ohjaa: warning: the decision stands, but \"{subject}\" is not in the home's \
                 history: {error:#}; the next commit there takes it in"
            ),
        }
    }
    for said in instructions::sync(concise) {
        if concise {
            review_text("  ", &said);
        } else {
            println!("  {said}");
        }
    }
}

fn show_pending(
    pending: &knowledge::Pending,
    duplicate_of: Option<&str>,
    loaded: &knowledge::Loaded,
    waiting: &[knowledge::Pending],
) {
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
    show_pending_relationships(pending, duplicate_of, loaded, waiting, false);
}

fn show_pending_relationships(
    pending: &knowledge::Pending,
    duplicate_of: Option<&str>,
    loaded: &knowledge::Loaded,
    waiting: &[knowledge::Pending],
    plain: bool,
) {
    let node = &pending.node;
    // What a node it names is now: current, still waiting, or gone.
    let standing = |id: &str| {
        if let Some(located) = loaded.get(id).filter(|l| l.node.status != Status::Proposed) {
            let first = located.node.body.lines().next().unwrap_or("");
            format!("{id} ({}: {first})", located.node.status.name())
        } else if waiting.iter().any(|p| p.node.id == id) {
            format!("{id} (waiting too)")
        } else {
            format!("{id} (no longer here)")
        }
    };
    if let Some(base) = &node.refines {
        let waits = loaded.get(base).is_none() && waiting.iter().any(|p| p.node.id == *base);
        let relation = format!(
            "{}{}",
            standing(base),
            if waits {
                "; it can be accepted once that one is"
            } else {
                ""
            }
        );
        if plain {
            review_text("  Extends: ", &relation);
        } else {
            println!("    refines: {relation}");
        }
    }
    if !node.supersedes.is_empty() {
        if plain {
            review_text(
                "  Replaces on acceptance: ",
                &node
                    .supersedes
                    .iter()
                    .map(|id| standing(id))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        } else {
            println!("    supersedes: {}", node.supersedes.join(", "));
        }
    }
    if let Some(of) = duplicate_of {
        let relation = format!("{} (their titles are near; you decide)", standing(of));
        if plain {
            review_text("  May repeat existing knowledge: ", &relation);
        } else {
            println!("    possible duplicate of: {relation}");
        }
    }
}

/// Preserve the text being decided on, including literal spacing and code
/// indentation. Let the terminal soft-wrap: reflowing words changes literals.
fn review_text(prefix: &str, text: &str) {
    for line in text.lines() {
        println!("{prefix}{line}");
    }
}

fn review_title(id: &str) -> String {
    let words = id.replace('-', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => words,
    }
}

fn show_review_card(pending: &knowledge::Pending, index: usize, total: usize) {
    let node = &pending.node;
    println!("\n── Review {index} of {total} ──\n");
    println!("  {}", review_title(&node.id));
    let scope = match pending.scope {
        Scope::Mine => "Personal",
        Scope::Machine => "This machine's",
        Scope::Ours => "Shared team",
        Scope::Repo => "Repository",
    };
    println!("  {scope} {}\n", node.kind.name());
    review_text("  ", &node.body);
    println!("\n  Why this was proposed");
    for why in &node.origin {
        review_text("    ", why);
    }
    println!();
    let effect = match pending.scope {
        Scope::Mine => "Accept to use this in your future agent sessions, across repositories.",
        Scope::Machine => "Accept to use this in future agent sessions on this machine.",
        Scope::Ours => {
            "Accept to stage shared knowledge; the team must review it in a pull request."
        }
        Scope::Repo => {
            "Accept to keep it for this repository; `ohjaa lessons` then writes it into its \
             AGENTS.md, for you to commit."
        }
    };
    review_text("  ", effect);
    if let Some(repo) = &pending.repo {
        review_text("  Repository: ", &repo.display().to_string());
    }
}

fn cmd_review(list: bool) -> Result<i32> {
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let (waiting, broken) = knowledge::pending(&dirs, &loaded)?;
    // What each may repeat, derived now, and each right after that one.
    let marks = duplicates::possible_duplicates(&loaded, &waiting, dirs.repo_root.as_deref());
    let reviewed = duplicates::review_order(waiting.iter().cloned().zip(marks).collect());
    for broken in &broken {
        eprintln!(
            "ohjaa: unreadable proposal {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    if waiting.is_empty() {
        if broken.is_empty() {
            println!("nothing is waiting for a decision");
            return Ok(0);
        }
        println!("No readable proposals; {} could not be read. Fix the files reported above, then run `ohjaa review` again.", broken.len());
        return Ok(INCOMPLETE);
    }
    let mut incomplete = !broken.is_empty();
    if list {
        for (pending, of) in &reviewed {
            show_pending(pending, of.as_deref(), &loaded, &waiting);
        }
        println!(
            "\n{} waiting; decide with `ohjaa review` at a terminal",
            waiting.len()
        );
        return Ok(if incomplete { INCOMPLETE } else { 0 });
    }
    knowledge::require_person("review")?;
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let today = knowledge::today();
    println!("Review knowledge proposals — nothing changes until you accept or reject.");
    println!(
        "Reject dismisses a proposal. Skip or Enter leaves it for later. Quit keeps the rest."
    );
    let (mut accepted, mut rejected, mut skipped) = (0, 0, 0);
    'proposals: for (index, (pending, of)) in reviewed.iter().enumerate() {
        // Earlier choices may have made a referenced proposal current.
        let current = knowledge::load(&dirs)?;
        let (still_waiting, _) = knowledge::pending(&dirs, &current)?;
        show_review_card(pending, index + 1, reviewed.len());
        show_pending_relationships(pending, of.as_deref(), &current, &still_waiting, true);
        let accepting = loop {
            print!("\n  [a]ccept  [r]eject  [s]kip  [d]etails  [q]uit > ");
            std::io::stdout().flush()?;
            let Some(answer) = lines.next().transpose()? else {
                break 'proposals;
            };
            match answer.trim().to_ascii_lowercase().as_str() {
                "a" | "accept" => break true,
                "r" | "reject" => break false,
                "s" | "skip" | "" => {
                    skipped += 1;
                    println!("  Skipped; still waiting for your decision.");
                    continue 'proposals;
                }
                "q" | "quit" => break 'proposals,
                "d" | "details" => {
                    show_pending(pending, of.as_deref(), &current, &still_waiting);
                    println!("  Proposal file: {}", pending.path.display());
                }
                _ => println!("  Choose one action: a, r, s, d or q, then Enter. Nothing changed."),
            }
        };
        let id = &pending.node.id;
        // Held for the decision, never while the person thinks.
        let lock = decision_lock();
        let action = if accepting { "accept" } else { "reject" };
        let result = if accepting {
            knowledge::accept(&dirs, id, today)
        } else {
            knowledge::reject(&dirs, id)
        };
        match result {
            Ok(outcome) => {
                if action == "accept" {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
                println!(
                    "  {}: {}",
                    if action == "accept" {
                        "Accepted"
                    } else {
                        "Rejected"
                    },
                    review_title(id)
                );
                if !outcome.in_the_home() {
                    report(&outcome);
                }
                after_decision(action, id, &outcome, None, lock.as_ref(), true);
                drop(lock);
                if action == "accept" {
                    say_lessons(&outcome);
                }
            }
            Err(error) => {
                incomplete = true;
                println!("  Could not {action} this proposal: {error:#}");
            }
        }
    }
    let (remaining, unreadable) = knowledge::pending(&dirs, &knowledge::load(&dirs)?)?;
    println!("\nReview finished: {accepted} accepted, {rejected} rejected, {skipped} skipped; {} still waiting.", remaining.len());
    if !unreadable.is_empty() {
        println!(
            "{} more proposal file(s) could not be read; the waiting count is incomplete.",
            unreadable.len()
        );
    }
    for broken in unreadable {
        incomplete = true;
        eprintln!(
            "ohjaa: unreadable proposal {}: {}",
            broken.path.display(),
            broken.reason
        );
    }
    if !remaining.is_empty() {
        println!("Continue with `ohjaa review`.");
    }
    println!("Inspect saved knowledge with `ohjaa list`.");
    Ok(if incomplete { INCOMPLETE } else { 0 })
}

/// The commands this ohjaa has, as clap knows them.
fn own_commands() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .chain(["help".to_owned()])
        .collect()
}

fn cmd_check(budget: usize, strict: bool) -> Result<i32> {
    let dirs = dirs()?;
    let loaded = knowledge::load(&dirs)?;
    let mut errors = 0;
    for broken in &loaded.broken {
        println!("error    {}: {}", broken.path.display(), broken.reason);
        errors += 1;
    }
    for left in &loaded.shadowed {
        println!(
            "flag     {}: left beside ohjaa's state's copy of the node, which is read",
            left.display()
        );
    }
    for error in fk::validate_set(&loaded.nodes) {
        println!("error    {error}");
        errors += 1;
    }
    let flags = knowledge::flags(
        &loaded,
        dirs.repo_root.as_deref(),
        knowledge::today(),
        &own_commands(),
    );
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
        match (scope, dirs.scope_dir(scope), dirs.repo_root.as_deref()) {
            (Scope::Repo, Some(legacy), Some(root)) => {
                println!(
                    "scope    {:<8} {} ({}), accepted in {}",
                    scope.name(),
                    root.join("AGENTS.md").display(),
                    lessons::HEADING,
                    knowledge::accepted_dir(&dirs.state, root).display()
                );
                if legacy.is_dir() {
                    println!(
                        "scope    {:<8} {} (accepted before ohjaa kept them in its state)",
                        scope.name(),
                        legacy.display()
                    );
                }
            }
            (_, Some(dir), _) => println!("scope    {:<8} {}", scope.name(), dir.display()),
            (_, None, _) => println!("scope    {:<8} (no home here)", scope.name()),
        }
    }
    println!(
        "summary  {current} current, {} waiting for a decision, {errors} error(s), {} flag(s), \
         briefing {size}/{budget}",
        waiting.len(),
        flags.len() + usize::from(over)
    );
    Ok(if errors > 0 || (strict && (!flags.is_empty() || over)) {
        1
    } else {
        0
    })
}

// ---- the briefing's scope ------------------------------------------------------

/// A `--scope` path as given: a relative path is taken from the current
/// directory, `~/` from the home directory.
fn scope_path(raw: &str) -> PathBuf {
    let path = knowledge::expand_home(raw);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

/// The repositories `--scope` names, other than `launch`, each once and
/// canonical; and the scope paths that lie in no repository (see
/// [`scope_path`]).
fn scoped_repos(scope: &[String], launch: Option<&Path>) -> (Vec<PathBuf>, Vec<String>) {
    let launch = launch.and_then(|path| std::fs::canonicalize(path).ok());
    let mut repos: Vec<PathBuf> = Vec::new();
    let mut unscoped = Vec::new();
    for raw in scope {
        match gitconfig::repo_of(&scope_path(raw)) {
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

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::Brief {
            knowledge_only,
            write,
            scope,
            agent,
        } => cmd_brief(*knowledge_only, write, scope, agent.as_deref()),
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
        Cmd::Lessons { path, dry_run } => match path {
            Some(path) => lessons::write(path, *dry_run),
            None => std::env::current_dir()
                .context("cannot tell the current folder")
                .and_then(|here| lessons::write(&here, *dry_run)),
        },
        Cmd::Apply { dry_run, undo } => {
            if *undo {
                apply::undo(*dry_run)
            } else {
                apply::apply(*dry_run)
            }
        }
        Cmd::Doctor => doctor::doctor(),
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
