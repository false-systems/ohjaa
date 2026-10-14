//! ohjaa: steering knowledge for agents.
//!
//! Toimija reports facts and refuses to decide. Ohjaa holds the other half:
//! the graph of what the people an agent works for know and have decided
//! (faija-ontology.v2, knowledge-node.v1), and it hands that graph to every
//! agent *before* it acts, together with the live perimeter of the fleet.
//! Agents may propose Knowledge; only a person makes it current.

mod adopt;
mod apply;
mod doctor;
mod flat_toml;
mod gate;
mod gitconfig;
mod home;
mod hook;
mod intake;
mod integrated;
mod judgement;
mod knowledge;
mod lifecycle;
mod mark;
mod process;
mod report;
mod roots;
mod scan;
mod sessions;
mod shell_init;
mod system;
mod usage;

use std::io::{BufRead, IsTerminal, Write};
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
        /// Why this session exists. A new Intent, with an id of its own,
        /// unless --intent-id continues one.
        #[arg(long, required_unless_present = "intent_id")]
        intent: Option<String>,
        /// Continue an existing Intent in this new session: its id, as a
        /// session that worked on it was given (`in-…`). Its text comes with
        /// it unless --intent gives another.
        #[arg(long, value_name = "ID")]
        intent_id: Option<String>,
        /// What to do; forwarded to `toimija run --task`, and recorded.
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
        /// transcripts, reviews, reverts or report (repeatable). Default: all four.
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
    /// What went around the workflow: the marks on each entry's branch (✓
    /// proven, ✗ failed, ? unproven, tip first), sessions without read-back or
    /// handover, default-branch commits without passing evidence, and agent
    /// sessions run without ohjaa (through vessa). Derived now; exits 4 when it
    /// could not read everything.
    Report {
        /// The window: `7d`, `24h`, a date or an RFC 3339 time.
        #[arg(long, default_value = "7d")]
        since: String,
        /// Print `ohjaa-marks.v1` JSON instead (see the README): the marks,
        /// the proposals waiting for a person, the sessions still open or
        /// orphaned, and the window's report. Writes nothing, not even the
        /// report's bookmark.
        #[arg(long)]
        json: bool,
        /// The marks alone (with --json, also the proposals and the open
        /// sessions): no window, no vessa; two git processes per entry, and
        /// sykli verify only where a receipt says it passed.
        #[arg(long)]
        marks: bool,
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
    /// Apply the workflow from the home (~/.config/ohjaa), outside every
    /// repository: git runs ohjaa's hooks only in repositories whose remote
    /// names an entry. Also the global ignore block and the shell launcher.
    /// Idempotent; --undo takes it all back.
    #[command(after_help = "Exit codes:\n  \
        0  applied, undone, or nothing to change\n  \
        1  git is older than 2.36, or a change could not be made\n  \
        2  refused: no roots file and no --root (apply does not guess where the\n     \
           repositories are: run it once with --root <dir>)\n  \
        3  refused: a global core.hooksPath (see --force), an ohjaa that is not the\n     \
           one on PATH (see --bin), or --prune without a roots file\n\n\
        Writes in the home:\n  \
        repos/<o>/<r>/hooks/<name>    exec <ohjaa> hook <name> \"$@\": pre-push, and each\n                                \
        hook that entry's clones under the roots have of their own\n  \
        repos/<o>/<r>/gated.config    core.hooksPath = that entry's hooks/\n  \
        git/config                    includeIf \"hasconfig:remote.*.url:<pattern>\" per entry\n  \
        roots                         from --root on the first run: where the repositories are\n  \
        applied.toml                  what apply changes outside the home, kept before each change\n\
        Changes outside it, and nothing else:\n  \
        git's global config    [include] path = <home>/git/config (once)\n  \
        git's global excludes  a block marked \"ohjaa apply\" holding .sykli/ .teko/\n  \
        ~/.zshrc               [[ -x <ohjaa> ]] && eval \"$(<ohjaa> shell-init zsh)\", in place\n                         \
        of a line that sources ~/projects/ohjaa-launch.zsh\n\
        <ohjaa> is the ohjaa on PATH (or --bin), and the pre-push dispatchers name\n\
        the sykli on PATH, both by absolute path. Dispatchers are only added;\n\
        --prune removes the ones no clone needs. --undo puts the three changes back\n\
        as they were (a hooksPath --force removed comes back at the end of [core],\n\
        a global config apply created is removed), and removes what apply wrote\n\
        in the home except the entries and the roots.")]
    Apply {
        /// Print every change, and make none.
        #[arg(long)]
        dry_run: bool,
        /// Remove a global core.hooksPath (it would run in every repository).
        #[arg(long)]
        force: bool,
        /// Also remove dispatchers no clone under the roots needs (needs a roots file).
        #[arg(long, conflicts_with = "undo")]
        prune: bool,
        /// The ohjaa the hooks and the launcher run, instead of the one on PATH.
        #[arg(long, value_name = "PATH", conflicts_with = "undo")]
        bin: Option<PathBuf>,
        /// Take back what apply changed outside the home, and what it generated.
        #[arg(long)]
        undo: bool,
    },
    /// Report what differs from what `apply` would make it and, clone by
    /// clone, whether git runs ohjaa's hooks where an entry says it should.
    /// Read-only.
    #[command(after_help = "Exit codes:\n  \
        0  healthy\n  \
        4  drift: a line marked DRIFT says what, and what fixes it\n\n\
        For each clone under the roots whose remote names an entry: git must\n\
        resolve its core.hooksPath to <home>/hooks (a pattern that missed, or the\n\
        repository's own core.hooksPath, means its pushes are not observed), and\n\
        each hook of its own must have a dispatcher (else git skips it).")]
    Doctor,
    /// Write a repository's entry into the home: the branch ohjaa observes
    /// (origin's default branch) and the gate it is judged by, copied from the
    /// repository's sykli.json on that branch, else written by
    /// `sykli init --contract`; then regenerate the home's git config. Neither
    /// git's global config nor the repository is written.
    Adopt {
        /// The repository (any directory inside it).
        path: PathBuf,
        /// The branch to observe, when origin/HEAD does not name it.
        #[arg(long)]
        branch: Option<String>,
        /// Rewrite an existing entry.
        #[arg(long)]
        force: bool,
    },
    /// The git hook dispatcher every hook in <home>/hooks execs. pre-push
    /// observes a push to the entry's branch, when the home has an entry for
    /// the repository, and never refuses it; every hook then runs the
    /// repository's own hook, with the same arguments and stdin.
    #[command(after_help = "Exit codes:\n  \
        0  the repository's own hook, if any, passed\n  \
        2  not a git hook, or the repository's own hooks cannot be found\n  \
        N  otherwise, the repository's own hook's status\n\n\
        The repository is found by the URL git pushes to; its entry is\n\
        <home>/repos/<owner>/<repo>/. A push that moves the entry's branch is\n\
        handed, quietly, to\n  \
        sykli hook pre-push --contract <entry>/sykli.json --branch <branch> <remote> <url>\n\
        for at most 15 s. When sykli does not accept it, the commit is asked about\n\
        as the marks ask (a receipt in any worktree of the clone). When that does\n\
        not prove it either, or sykli cannot judge it (no sykli, one without\n\
        --contract and --branch, an entry without a gate), the push goes on and\n\
        one line flags it: ohjaa: UNPROVEN push to <branch> (<why>). A push ohjaa\n\
        cannot observe at all (no workflow home, an unreadable entry) goes on\n\
        with: ohjaa: UNOBSERVED push to <owner/repo>: <why>. The repository's\n\
        own hook is $(git rev-parse --git-common-dir)/hooks/<name>, run when\n\
        executable; it still refuses what it refuses.")]
    Hook {
        /// The git hook's name, e.g. pre-push.
        name: String,
        /// The arguments git passed to the hook.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Prove HEAD: run the gate for the repository here, as a push to its
    /// observed branch is judged: the contract of the entry origin names (else the
    /// repository's own sykli.json), run into .sykli/receipt.json and verified
    /// for HEAD. Prints the contract and every command it runs.
    #[command(after_help = "Exit codes:\n  \
        0  passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven\n  \
        N  sykli's status: the run or the verify failed\n  \
        2  no gate here: origin names no entry, and there is no sykli.json\n  \
        75 not proven and not failed: a durable sykli run is still going after\n     \
        --wait, or sykli says it ended without completing; it is recorded\n\n\
        Runs, from the repository root:\n  \
        mkdir -p .sykli && sykli run <contract> --json > .sykli/receipt.json &&\n  \
        sykli verify .sykli/receipt.json --contract <contract> --commit HEAD\n\
        With a sykli that has durable runs (sykli run --detach), the run is\n\
        handed over detached and asked about by its id until it ends.")]
    Gate {
        /// Print the contract and the commands, and run nothing.
        #[arg(long)]
        print: bool,
        /// With durable sykli runs: how long to wait for the run, in seconds,
        /// before saying it is still running (exit 75; the run goes on).
        #[arg(long, value_name = "SECONDS", default_value_t = 1800)]
        wait: u64,
    },
    /// Record an independent Judgement of a Change (base..head) and its
    /// exact State. The reviewer is observed: the live ohjaa session whose
    /// process group holds this process (or an ancestor), whatever
    /// OHJAA_SESSION says; outside every one, the person git names here.
    /// Refused when the reviewer made any commit of the Change (law 2). Kept
    /// in the session log, and attached to the head as a git note under
    /// refs/notes/ohjaa/judgements.
    #[command(
        args_conflicts_with_subcommands = true,
        subcommand_negates_reqs = true,
        after_help = "Exit codes:\n  \
        0  recorded\n  \
        1  recorded, but not shared: --push did not reach origin\n  \
        2  nothing to judge: the range cannot be resolved, or holds no commit\n  \
        3  refused, nothing recorded: the reviewer made a commit of the Change\n     \
           (law 2), OHJAA_SESSION names no live session in the log, or --push\n     \
           for a repository whose Judgements are not shared\n  \
        4  not recorded: the session log could not be read or written (busy\n     \
           past 20s, or broken), or the repository or its entry cannot be read\n\n\
        The judge's procedural independence is `established` only when every\n\
        commit of the Change is held by an ended session's record, none the\n\
        reviewer's; this process runs inside no live session but the reviewer's\n\
        own; and OHJAA_SESSION is unset (a person, whose git identity is on no\n\
        commit of the Change) or names that session. Otherwise it is recorded,\n\
        with `established: false` and each reason: ohjaa cannot tell a person\n\
        from an agent, and a reflog is the agent's own testimony.\n\n\
        RANGE is base..head, or a head alone: its base is then where it leaves\n\
        origin's copy of the entry's branch. The evidence considered is what\n\
        `sykli status --commit <head> --contract <gate> --json` names (runs and\n\
        receipts, by reference). Judgements of a private repository (its entry's\n\
        sharing.toml says visibility = \"private\") are shared when asked; of any\n\
        other, only with share_judgements = true there, and their notes hold ids,\n\
        hashes and the verdict only. --push (or push_judgements = true) pushes\n\
        refs/notes/ohjaa/judgements to origin as the union of this clone's lines,\n\
        origin's and the session log's: no line is dropped, and none is replaced."
    )]
    Judge {
        #[command(subcommand)]
        action: Option<JudgeCmd>,
        /// base..head, or a head (default HEAD).
        #[arg(default_value = "HEAD")]
        range: String,
        #[arg(long, value_enum, required = true)]
        verdict: Option<judgement::Verdict>,
        /// What was found (repeatable).
        #[arg(long = "finding", value_name = "TEXT")]
        findings: Vec<String>,
        #[arg(long, value_enum)]
        severity: Option<judgement::Severity>,
        /// The findings block the Change.
        #[arg(long)]
        blocking: bool,
        /// What the reviewer could not check, or is unsure of.
        #[arg(long, value_name = "TEXT")]
        uncertainty: Option<String>,
        /// The Intent the Change serves, when its sessions do not say.
        #[arg(long, value_name = "ID")]
        intent_id: Option<String>,
        /// Push the notes ref to origin once recorded (shared repositories).
        #[arg(long)]
        push: bool,
    },
    /// Every Judgement of a commit (and the answers to them), from this
    /// machine's log and the notes, each marked with where it was read
    /// (log, note, merged, fetched), whether it is authoritative here, and
    /// whether another record of its id says otherwise (a conflict).
    #[command(
        after_help = "--rebuild-public rewrites this clone's refs/notes/ohjaa/judgements as one\n\
        commit by ohjaa <ohjaa@invalid> with no history, holding only redacted lines:\n\
        every redacted line of its notes, and this machine's Judgements of the clone\n\
        in their redacted form. Every other line is dropped from the ref (the session\n\
        log keeps this machine's Judgements), and the old history is no longer\n\
        reachable from it, so no push publishes it; git's reflog keeps it on this\n\
        machine only. Nothing is pushed. For an entry made public after it was\n\
        private: a note removed with `git notes remove` is still in the history."
    )]
    Judgements {
        /// The commit (default HEAD).
        #[arg(default_value = "HEAD")]
        commit: String,
        #[arg(long)]
        json: bool,
        /// First fetch origin's shared notes (refs/notes/ohjaa/judgements)
        /// into refs/notes/ohjaa/fetched/judgements.
        #[arg(long)]
        fetch: bool,
        /// Rewrite this clone's notes ref with redacted lines only, and no
        /// history (see below).
        #[arg(long, conflicts_with_all = ["json", "fetch"])]
        rebuild_public: bool,
    },
    /// Print the shell functions that route `claude` and `codex` through
    /// `ohjaa run`. Load them with: eval "$(ohjaa shell-init zsh)"
    ShellInit {
        /// zsh is the only one.
        shell: String,
    },
}

#[derive(Subcommand)]
enum JudgeCmd {
    /// Answer a Judgement: a record of its own beside it, never a change to
    /// it. From inside a session, or as the person git names here.
    Respond {
        /// The Judgement's id (`jd-…`).
        judgement: String,
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
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
            // Opened only to read: with no log yet there is nothing to
            // export, and none is created.
            let (mut records, unreadable) =
                sessions::Reader::open(&sessions::store_path(&dirs.state))?.all()?;
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

/// The repository around the current directory, asked of git only when the
/// roots must be derived from it: none given, and no roots file.
fn git_root_for_roots(explicit: &[PathBuf]) -> Option<PathBuf> {
    let configured = roots::config_path().is_some_and(|path| path.is_file());
    (explicit.is_empty() && !configured)
        .then(git_root)
        .flatten()
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

/// The knowledge section of the briefing for these scopes, and exactly which
/// nodes it delivers. Broken files and flags are reported on stderr, out loud;
/// they never make the briefing silently smaller.
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
        // The preamble is said once, before the first knowledge of all; each
        // repository adds its section.
        let knowledge = knowledge
            .strip_prefix(knowledge::PREAMBLE)
            .unwrap_or(&knowledge)
            .trim_start_matches('\n')
            .to_owned();
        if !knowledge.is_empty() {
            if text.is_empty() {
                text.push_str(knowledge::PREAMBLE);
            }
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

/// The sessions toimija sees in the other worktrees of the clone `root` is
/// a checkout of (from the clone's layout; never `folder` itself, nor a
/// checkout ohjaa made to validate an integrated State), each asked as the
/// launch checkout is, side by side.
fn clone_scans(root: &Path, folder: &Path) -> Vec<Scan> {
    let Ok(common) = gitconfig::common_dir(root) else {
        return Vec::new();
    };
    let ours = lifecycle::validation_checkouts();
    let others: Vec<PathBuf> = lifecycle::layout_checkouts(&common)
        .into_iter()
        .filter(|checkout| checkout != folder && checkout.is_dir())
        .filter(|checkout| ours.as_ref().is_none_or(|ours| !checkout.starts_with(ours)))
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
    let here = dirs.repo_root.clone().unwrap_or(std::env::current_dir()?);
    // The entry's branch unproven at its tip comes first of all, then the
    // last session's warnings; its record after the knowledge.
    if !knowledge_only {
        if let Some(top) = dirs.repo_root.as_deref() {
            let launch = mark::at_launch(top);
            if let Some(flag) = &launch.flag {
                println!("{flag}\n");
            }
            // What sykli says of the tip and of HEAD here; nothing is asked for.
            if let Some(marks) = &launch.marks {
                let caps = integrated::Caps::ask(false);
                let all = session_reader(&dirs)
                    .and_then(|log| log.all().ok())
                    .map(|(all, _)| all)
                    .unwrap_or_default();
                let head = gitconfig::read_head(top, &marks.gate.common);
                let view = integrated::at_launch(
                    marks,
                    head.as_ref()
                        .map(|(commit, branch)| (commit.as_str(), branch.as_deref())),
                    &all,
                    integrated::Mode::Brief,
                    &dirs.state,
                    &sessions::store_path(&dirs.state),
                    &caps,
                );
                let text = view.briefing();
                if !text.is_empty() {
                    println!("{text}");
                }
            }
        }
    }
    // sykli is asked about each pending validation shown, once.
    let runs = lifecycle::RunStatus::default();
    let (warnings, last) = last_session_here(&dirs, &here, &runs).unzip();
    if let Some(warnings) = warnings.filter(|warnings| !knowledge_only && !warnings.is_empty()) {
        println!("{warnings}");
    }
    if knowledge.is_empty() {
        println!("No knowledge recorded yet (`ohjaa propose`, then `ohjaa review`).");
    } else {
        print!("{knowledge}");
    }
    if knowledge_only {
        return Ok(if unscoped.is_empty() { 0 } else { INCOMPLETE });
    }
    if let Some(last) = last.filter(|last| !last.is_empty()) {
        print!("\n{last}");
    }
    if let Some(top) = dirs.repo_root.as_deref() {
        let folder = std::fs::canonicalize(top).unwrap_or_else(|_| top.to_path_buf());
        let siblings = clone_sessions_text(&clone_scans(top, &folder));
        if !siblings.is_empty() {
            print!("\n{siblings}");
        }
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

/// Open the session log only to read it, or say why not: what only reads
/// the log never writes it (#41).
fn session_reader(dirs: &Dirs) -> Option<sessions::Reader> {
    match sessions::Reader::open(&sessions::store_path(&dirs.state)) {
        Ok(log) => Some(log),
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

/// How many earlier sessions here a briefing looks back through for flags
/// that still hold.
const STANDING_LOOKBACK: usize = 50;

/// What the log knows of this repository: the warnings for the very top of a
/// briefing (the last session's flags, and any earlier session's flag about a
/// branch that still sits at the flagged commit, including one a session only
/// found here saw), and "Last session here", with what sykli says now of the
/// validations it left pending (`runs`). `None` when it knows neither.
fn last_session_here(
    dirs: &Dirs,
    repo: &Path,
    runs: &lifecycle::RunStatus,
) -> Option<(String, String)> {
    let here = repo_key(repo);
    // Read in one short open, let go before any git is asked.
    let (latest, all) = {
        let log = session_reader(dirs)?;
        let latest = match log.latest_for(&here) {
            Ok(latest) => latest,
            Err(error) => {
                eprintln!("ohjaa: {error}");
                return None;
            }
        };
        (latest, log.all().map(|(all, _)| all).unwrap_or_default())
    };
    let latest_id = latest.as_ref().map(|s| s.id.clone());
    let pending = pending_here(&all, latest_id.as_deref(), repo, &here, runs);
    // The Judgements of this clone, and its Changes with none (D5).
    let judged = match judgement::logged() {
        Ok((judgements, responses)) => {
            let mut checkouts: Vec<String> = crate::gitconfig::common_dir(repo)
                .map(|common| lifecycle::layout_checkouts(&common))
                .unwrap_or_default()
                .iter()
                .map(|checkout| checkout.to_string_lossy().into_owned())
                .collect();
            if !checkouts.contains(&here) {
                checkouts.push(here.clone());
            }
            judgement::briefing(&all, &judgements, &responses, &checkouts)
        }
        Err(error) => format!("Judgements here: the session log could not be read ({error:#})\n"),
    };
    let mut earlier: Vec<sessions::Session> = all
        .into_iter()
        .filter(|s| Some(&s.id) != latest_id.as_ref() && sessions::observed_in(s, &here))
        .collect();
    earlier.sort_by_key(|s| std::cmp::Reverse(s.ended_rank()));
    earlier.truncate(STANDING_LOOKBACK);
    let warnings =
        sessions::render_warnings(latest.as_ref(), &earlier, &here, |repo, branch, commit| {
            lifecycle::still_at(Path::new(repo), branch, commit)
        });
    let mut last = latest
        .as_ref()
        .map(|session| sessions::render_last_with(session, &here, &|v| runs.ask(v)))
        .unwrap_or_default();
    last.push_str(&pending);
    if !judged.is_empty() {
        if !last.is_empty() {
            last.push('\n');
        }
        last.push_str(&judged);
    }
    (!warnings.is_empty() || !last.is_empty()).then_some((warnings, last))
}

/// How long a validation an earlier session left pending is still named in
/// a briefing, while sykli does not say it ended.
const PENDING_FOR_MS: i64 = 7 * 86_400_000;

/// The validations earlier sessions requested in this clone (`repo`, keyed
/// `here`) and left pending (a sykli run id, no receipt): a validation
/// outlives the session that asked for it. Each is asked about (`runs`) and
/// named, as sykli says it now, until sykli says it ended; one sykli cannot
/// be asked about is named for a week from its request, then counted. The
/// last session's own are in its block, not here. Empty when there are none.
fn pending_here(
    all: &[sessions::Session],
    latest: Option<&str>,
    repo: &Path,
    here: &str,
    runs: &lifecycle::RunStatus,
) -> String {
    use std::fmt::Write as _;
    let pending: Vec<(&sessions::Session, &sessions::ValidationRef)> = all
        .iter()
        .filter(|session| session.left.is_some() && Some(session.id.as_str()) != latest)
        .flat_map(|session| session.validations.iter().map(move |v| (session, v)))
        .filter(|(_, v)| v.pending())
        .collect();
    if pending.is_empty() {
        return String::new();
    }
    // This clone's common dir, from its layout: a sibling worktree's
    // validations are this clone's too.
    let common = crate::gitconfig::common_dir(repo)
        .ok()
        .map(|common| repo_key(&common));
    let mut pending: Vec<_> = pending
        .into_iter()
        .filter(|(_, v)| v.repo == here || (common.is_some() && v.common == common))
        .collect();
    if pending.is_empty() {
        return String::new();
    }
    pending.sort_by_key(|(_, v)| std::cmp::Reverse(v.requested_ms));
    let now_ms = intake::now_seconds() * 1000;
    let mut lines = String::new();
    let mut older = 0;
    for (session, validation) in pending {
        let said = runs.ask(validation);
        if said.as_ref().is_some_and(|said| said.ended) {
            continue;
        }
        let asked = said
            .as_ref()
            .is_some_and(|said| said.text.starts_with("sykli says"));
        if !asked && now_ms - validation.requested_ms > PENDING_FOR_MS {
            older += 1;
            continue;
        }
        let line =
            sessions::render_validations(std::slice::from_ref(validation), "- ", &|_| said.clone());
        let _ = writeln!(
            lines,
            "{} (session {}, pending since {})",
            line.trim_end(),
            session.id,
            sessions::clock(validation.requested_ms)
        );
    }
    if older > 0 {
        let _ = writeln!(
            lines,
            "- {older} more requested over a week ago that sykli cannot be asked about"
        );
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("Validations earlier sessions here left pending:\n{lines}")
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
    let store = sessions::Reader::open(&sessions::store_path(&dirs.state))
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
            // With what sykli says now of the validations it left pending.
            let runs = lifecycle::RunStatus::default();
            print!(
                "{}",
                sessions::render_record_with(&session, &|v| runs.ask(v))
            );
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
    if let Some(log) = session_reader(&dirs) {
        if let Ok((all, _)) = log.all() {
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

/// The git repositories a report reads: those under the roots.
fn report_repos(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    roots::git_repos(&roots::load(roots, git_root_for_roots(roots).as_deref()))
}

fn cmd_report(roots: &[PathBuf], since: &str, json: bool, marks: bool) -> Result<i32> {
    let dirs = dirs()?;
    let now_ms = intake::now_seconds() * 1000;
    let since_ms = report::parse_since(since, now_ms)?;
    let repos = report_repos(roots)?;
    let report = if marks {
        report::build_marks(&dirs, &repos, since_ms, now_ms)
    } else {
        report::build(&dirs, &repos, since_ms, now_ms)
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report::to_json(&report))?
        );
    } else {
        print!("{}", report::render(&report));
    }
    // A report a person read at a terminal was said; a complete one is the
    // week's. Never one for a script or an agent, nor the marks alone.
    if !json && !marks && std::io::stdout().is_terminal() {
        if let Err(error) = report::shown(&dirs, now_ms, report.complete()) {
            eprintln!("ohjaa: {error:#}");
        }
    }
    Ok(if report.complete() { 0 } else { INCOMPLETE })
}

/// Once a week, at a launch a person watches: the report is read while the
/// agent works, never delaying its start, and three lines of it are said
/// after the session's record, when the person is back at the shell. Never
/// into the agent's briefing.
struct Weekly {
    dirs: Dirs,
    now_ms: i64,
    reading: std::thread::JoinHandle<report::Report>,
}

fn weekly_report(dirs: &Dirs, roots: &[PathBuf]) -> Option<Weekly> {
    let now_ms = intake::now_seconds() * 1000;
    if !std::io::stderr().is_terminal() || !report::due(dirs, now_ms) {
        return None;
    }
    let (dirs, roots) = (dirs.clone(), roots.to_vec());
    let reading = {
        let dirs = dirs.clone();
        std::thread::spawn(move || {
            let repos = report_repos(&roots).unwrap_or_default();
            report::build(&dirs, &repos, now_ms - report::WEEK_MS, now_ms)
        })
    };
    Some(Weekly {
        dirs,
        now_ms,
        reading,
    })
}

/// Say the weekly report after the record. The week is marked only when it
/// was complete; one with a gap is said again, at most once a day.
fn say_weekly(weekly: Option<Weekly>) {
    let Some(weekly) = weekly else {
        return;
    };
    if !weekly.reading.is_finished() {
        eprintln!("ohjaa: reading the weekly report…");
    }
    let Ok(report) = weekly.reading.join() else {
        return;
    };
    eprint!("{}", report::summary(&report));
    if let Err(error) = report::shown(&weekly.dirs, weekly.now_ms, report.complete()) {
        eprintln!("ohjaa: {error:#}");
    }
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
        vec!["transcripts", "reviews", "reverts", "report"]
    } else {
        sources.iter().map(String::as_str).collect()
    };
    // Sessions that ended and were never drafted from: the agent's own
    // handover, and what was said in that session's window, even when the
    // window lies before the cursor. Read in one short, read-only open of the
    // log; no store is held across the slow calls below.
    let queued: Vec<sessions::Session> = session_reader(&dirs)
        .and_then(|log| log.all().ok())
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
            "report" => {
                // What the report finds going around the workflow: real
                // failures and bypasses only. What it could not read is said,
                // and does not hold the cursor back.
                let report = report::build(&dirs, &git_repos()?, since, started);
                for gap in &report.gaps {
                    eprintln!("ohjaa: intake: report: not read: {gap}");
                }
                if let Err(why) = &report.natives {
                    eprintln!("ohjaa: intake: report: {why}");
                }
                candidates.extend(report::candidates(&report));
            }
            other => bail!("`{other}` is not a source: transcripts, reviews, reverts or report"),
        }
    }
    if !queued.is_empty() {
        candidates.extend(intake::handover_candidates(&queued));
        candidates.extend(intake::observed_candidates(&queued));
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

/// The Intent a session works on: an id of its own, never a session's, and
/// its text (`--intent`).
struct Intent {
    id: String,
    text: String,
    origin: IntentOrigin,
}

enum IntentOrigin {
    /// Minted at this launch.
    New,
    /// Continued: the newest session in the log that carries it, and the
    /// repository it ran in when that is not this one.
    ContinuedFrom {
        session: String,
        elsewhere: Option<String>,
    },
    /// Continued, but the log could not be read to find it (why).
    Unchecked(String),
}

impl Intent {
    /// The line the briefing opens "How this session works" with.
    fn briefing_line(&self, session: &str) -> String {
        let origin = match &self.origin {
            IntentOrigin::New => "new".to_owned(),
            IntentOrigin::ContinuedFrom {
                session,
                elsewhere: None,
            } => format!("continued from session {session}"),
            IntentOrigin::ContinuedFrom {
                session,
                elsewhere: Some(repo),
            } => format!("continued from session {session}, which worked on it in {repo}"),
            IntentOrigin::Unchecked(why) => {
                format!("continued; the session log could not be read to check it ({why})")
            }
        };
        format!(
            "This session is {session}; its intent is {}, {origin}. Continue it in a later \
             session with: ohjaa run <agent> --intent-id {}\n",
            self.id, self.id
        )
    }
}

/// The Intent of a new session in `here`: continued with `--intent-id` (its
/// text from the newest session that carries it, unless `--intent` gives
/// another), or new, with an id minted now (`in-<ms>-<pid>`, real
/// milliseconds). An id no session in the log carries is refused; a log
/// that cannot be read never refuses a launch, and the continuation is then
/// said to be unchecked. An Intent last worked on in another repository is
/// continued (an Intent may span repositories), and that is said.
fn resolve_intent(
    dirs: &Dirs,
    here: &str,
    text: Option<&str>,
    id: Option<&str>,
) -> std::result::Result<Intent, String> {
    let Some(id) = id else {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64);
        return Ok(Intent {
            id: sessions::new_intent_id(now_ms),
            text: text.unwrap_or_default().to_owned(),
            origin: IntentOrigin::New,
        });
    };
    if sessions::pid_of(id).is_some() {
        return Err(format!(
            "{id} is a session's id, not an intent's: an intent id starts `in-`"
        ));
    }
    if !sessions::is_valid_id(id) || !id.starts_with("in-") {
        return Err(format!("{id} is not an intent id (`in-…`)"));
    }
    let carried = session_reader(dirs)
        .ok_or_else(|| "the session log could not be opened".to_owned())
        .and_then(|log| log.all().map_err(|error| error.to_string()));
    // A blank --intent gives no text: the Intent's own comes with it.
    let text = text.filter(|text| !text.trim().is_empty());
    let all = match carried {
        Ok((all, _)) => all,
        Err(why) => {
            return Ok(Intent {
                id: id.to_owned(),
                text: text.map_or_else(|| format!("continuing intent {id}"), str::to_owned),
                origin: IntentOrigin::Unchecked(why),
            })
        }
    };
    let Some(newest) = all
        .into_iter()
        .filter(|s| s.intent_id.as_deref() == Some(id))
        .max_by_key(|s| s.started_ms)
    else {
        return Err(format!("no session in the log carries intent {id}"));
    };
    let here_too = newest.repo == here || newest.scoped.iter().any(|s| s.repo == here);
    Ok(Intent {
        id: id.to_owned(),
        text: text.map_or(newest.intent, str::to_owned),
        origin: IntentOrigin::ContinuedFrom {
            session: newest.id,
            elsewhere: (!here_too).then_some(newest.repo),
        },
    })
}

struct RunOptions<'a> {
    agent: &'a str,
    intent: Option<&'a str>,
    intent_id: Option<&'a str>,
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
        intent_id,
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
    // The sessions in the clone's other worktrees: toimija keeps each
    // worktree's apart, so each is asked (D5, audit finding 9).
    let clone_scans = if in_repo {
        clone_scans(&root, &folder)
    } else {
        Vec::new()
    };
    if in_repo {
        eprintln!("ohjaa: {live} sibling session(s) in this checkout at launch");
        let elsewhere: Vec<String> = clone_scans
            .iter()
            .flat_map(|scan| scan.occupying())
            .map(|s| {
                format!(
                    "{} ({}, {})",
                    s.id,
                    s.agent,
                    s.worktree.file_name().unwrap_or_default().to_string_lossy()
                )
            })
            .collect();
        if !elsewhere.is_empty() {
            eprintln!(
                "ohjaa: {} more session(s) in other worktrees of this clone at launch: {}",
                elsewhere.len(),
                elsewhere.join(", ")
            );
        }
        for scan in clone_scans
            .iter()
            .filter(|scan| scan.coverage != Coverage::Available)
        {
            eprintln!(
                "ohjaa: the sessions in {}, a worktree of this clone, could not be read \
                 completely: {}",
                scan.repo.display(),
                coverage_detail(scan)
            );
        }
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
                "ohjaa: no --scope names a repository; at leaving, the repositories directly \
                 in this folder that moved are recorded, as found rather than named"
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
        if scope_path(raw).is_dir() {
            eprintln!(
                "ohjaa: --scope {raw} is not inside a Git repository; at leaving, the \
                 repositories directly in it that moved are recorded, as found rather than named"
            );
        } else {
            eprintln!(
                "ohjaa: --scope {raw} is not inside a Git repository; nothing there is observed"
            );
        }
    }
    // Where leaving looks for repositories nobody named: the launch folder
    // outside a repository, and every folder --scope named that is none.
    let search: Vec<PathBuf> = (!in_repo)
        .then(|| folder.clone())
        .into_iter()
        .chain(
            unscoped
                .iter()
                .map(|raw| scope_path(raw))
                .filter(|path| path.is_dir())
                .map(|path| std::fs::canonicalize(&path).unwrap_or(path)),
        )
        .collect();
    if quiet > 0 {
        eprintln!(
            "ohjaa: {quiet} of them stale — toimija cannot tell a finished agent \
             from a quiet one; `toimija sessions` shows their last heartbeat"
        );
    }
    for warning in &scan.warnings {
        eprintln!("ohjaa: warning: {warning}");
    }
    // Where an entry says ohjaa observes this repository: what doctor checks
    // for it, and whether the entry's branch is proven at its tip (a few git
    // calls and one sykli verify at most, none outside an observed one).
    let launch_marks = if in_repo {
        mark::at_launch(&root)
    } else {
        mark::AtLaunch::default()
    };
    for warning in &launch_marks.warnings {
        eprintln!("ohjaa: warning: {warning}");
    }
    if let Some(flag) = &launch_marks.flag {
        eprintln!("ohjaa: {flag}");
    }
    let mark_flag = launch_marks.flag.clone();

    // A footgun guard: ohjaa flags after `--` belong to the agent.
    for misplaced in [
        "--dry-run",
        "--no-briefing",
        "--intent",
        "--intent-id",
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
    // The Intent: continued, from the sessions that carry its id, or new.
    let intent = match resolve_intent(&dirs, &repo_key(&root), intent, intent_id) {
        Ok(intent) => intent,
        Err(why) => {
            eprintln!("ohjaa: {why}");
            eprintln!("ohjaa: refusing to launch");
            return Ok(REFUSED);
        }
    };
    if let IntentOrigin::ContinuedFrom {
        session,
        elsewhere: Some(repo),
    } = &intent.origin
    {
        eprintln!(
            "ohjaa: intent {} was last worked on in {repo} (session {session}); it is \
             continued here",
            intent.id
        );
    }
    // Sessions left open by an ohjaa that is gone (a SIGKILL, a crash, a
    // restart) are closed first, so they are queued and can be the last session
    // here. One whose own process group still runs is still running: left open.
    let (orphans, still_running) = close_orphans(&dirs, !dry_run);
    for session in &still_running {
        eprintln!(
            "ohjaa: session {} ({}, started {}, in {}) is still running in process group {}, \
             but the ohjaa that started it is gone; it is left open",
            session.id,
            session.agent,
            sessions::clock(session.started_ms),
            session.repo,
            session
                .pgid
                .map_or_else(|| "?".to_owned(), |pgid| pgid.to_string())
        );
    }
    // Read while the agent works; said after the record.
    let weekly = (!dry_run).then(|| weekly_report(&dirs, roots)).flatten();
    let here_key = repo_key(&root);
    for orphan in &orphans {
        eprintln!(
            "ohjaa: session {} ({}, started {}, in {}) was left open by an ohjaa that is \
             gone; {} as orphaned and queued for `ohjaa intake`",
            orphan.id,
            orphan.agent,
            sessions::clock(orphan.started_ms),
            orphan.repo,
            if dry_run { "would be closed" } else { "closed" }
        );
    }
    let here = |s: &sessions::Session| {
        s.repo == here_key || s.scoped.iter().any(|scoped| scoped.repo == here_key)
    };
    let orphaned_here: Vec<sessions::Session> = orphans.into_iter().filter(|s| here(s)).collect();
    let still_running_here: Vec<sessions::Session> =
        still_running.into_iter().filter(|s| here(s)).collect();
    let started_ms = intake::now_seconds() * 1000;
    let session_id = sessions::new_id(started_ms);
    let mut delivered = Vec::new();
    // What sykli says of the observed branch's tip (the integrated State)
    // and of HEAD (a candidate), with a stale base flagged; a launch asks
    // for a run of an unverified tip, recorded with this session first.
    let (mut validation, integrated_caps) = match (&launch_marks.marks, no_briefing) {
        (Some(marks), false) => {
            let caps = integrated::Caps::ask(true);
            let head = gitconfig::read_head(&root, &marks.gate.common);
            let all = session_reader(&dirs)
                .and_then(|log| log.all().ok())
                .map(|(all, _)| all)
                .unwrap_or_default();
            let view = integrated::at_launch(
                marks,
                head.as_ref()
                    .map(|(commit, branch)| (commit.as_str(), branch.as_deref())),
                &all,
                if dry_run {
                    integrated::Mode::DryRun
                } else {
                    integrated::Mode::Launch
                },
                &dirs.state,
                &sessions::store_path(&dirs.state),
                &caps,
            );
            for line in &view.lines {
                eprintln!("ohjaa: {line}");
            }
            (view, Some(caps))
        }
        _ => (integrated::AtLaunch::default(), None),
    };
    let briefing = if no_briefing {
        None
    } else {
        let (knowledge, nodes) = knowledge_for_work(&dirs, &scoped_repos)?;
        delivered = nodes;
        // sykli is asked about each pending validation shown, once.
        let runs = lifecycle::RunStatus::default();
        let (warnings, last) = last_session_here(&dirs, &root, &runs).unwrap_or_default();
        repos(roots)?;
        let scans = in_repo
            .then(|| scan.clone())
            .into_iter()
            .chain(scoped_scans.iter().cloned())
            .collect();
        let view = selected_perimeter(&root, in_repo, &scoped_repos, scans, outside.clone());
        let perimeter = scoped_perimeter_text(&view, &root);
        let how = lifecycle_briefing(
            &intent.briefing_line(&session_id),
            &root,
            in_repo,
            &scoped_repos,
        );
        let orphaned = sessions::render_orphaned(&orphaned_here, &|v| runs.ask(v));
        let still = sessions::render_still_running(&still_running_here, &|v| runs.ask(v));
        // The entry's branch unproven at its tip comes first of all: what
        // everyone builds on; then what sykli says of it and of HEAD.
        let flagged = mark_flag.clone().unwrap_or_default();
        let validated = validation.briefing();
        let siblings = clone_sessions_text(&clone_scans);
        let parts: Vec<&str> = [
            flagged.as_str(),
            validated.as_str(),
            still.as_str(),
            orphaned.as_str(),
            warnings.as_str(),
            knowledge.as_str(),
            last.as_str(),
            how.as_str(),
            siblings.as_str(),
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
    command.arg("run").arg("--intent").arg(&intent.text);
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

    // What every checkout and branch of the launch and scoped repositories
    // held at entering, kept until leaving: none of it is this session's.
    // The sessions that had already ended when this one entered: none of them
    // can have shared its places, whatever whole-second times suggest.
    let ended_at_entering: std::collections::HashSet<String> = session_store(&dirs)
        .and_then(|store| store.all().ok())
        .map(|(all, _)| {
            all.into_iter()
                .filter(|s| s.left.is_some())
                .map(|s| s.id)
                .collect()
        })
        .unwrap_or_default();
    let starts: Vec<(PathBuf, lifecycle::Start)> = in_repo
        .then(|| folder.clone())
        .into_iter()
        .chain(scoped_repos.iter().cloned())
        .map(|repo| {
            let start = lifecycle::start_of(&repo);
            (repo, start)
        })
        .collect();
    let start_of = |repo: &Path| {
        let repo = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        starts
            .iter()
            .find(|(known, _)| *known == repo)
            .map(|(_, start)| start.clone())
            .unwrap_or_default()
    };
    let mut session = sessions::Session {
        id: session_id.clone(),
        agent: agent.to_owned(),
        repo: repo_key(&root),
        outside_repo: !in_repo,
        intent: intent.text.clone(),
        intent_id: Some(intent.id.clone()),
        task: task.map(str::to_owned),
        pgid: None,
        group_started_ms: None,
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
        // An integrated validation the briefing asked for: recorded with
        // the entering, before sykli is asked (invariant A).
        validations: validation
            .request
            .iter()
            .map(|request| request.validation.clone())
            .collect(),
    };
    // From here to the end of leaving, a signal is kept, not fatal.
    lifecycle::supervise();
    let entered = match session_store(&dirs) {
        Some(store) => match store.put(&session) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("ohjaa: {error}; this session will not be recorded");
                false
            }
        },
        None => false,
    };
    eprintln!("ohjaa: session {session_id} entering");
    if let (Some(request), Some(caps), Some(marks)) =
        (&validation.request, &integrated_caps, &launch_marks.marks)
    {
        if entered {
            let log = sessions::store_path(&dirs.state);
            match integrated::dispatch_recorded(
                &log,
                &dirs.state,
                &session_id,
                &marks.gate,
                request,
                caps,
            ) {
                integrated::Asked::Recorded { request: id, run } => match run {
                    Ok(run) => eprintln!(
                        "ohjaa: integrated validation {id}: sykli run {run}, in {}",
                        request.checkout.display()
                    ),
                    Err(why) => eprintln!(
                        "ohjaa: integrated validation {id} is recorded, and could not be handed \
                         to sykli: {why}"
                    ),
                },
                integrated::Asked::NotRecorded(why) => {
                    eprintln!("ohjaa: integrated validation not asked for: {why}")
                }
            }
        } else {
            let why = "this session could not be recorded first, so sykli was not asked";
            eprintln!("ohjaa: integrated validation not asked for: {why}");
            for validation in &mut session.validations {
                validation.not_dispatched = Some(why.to_owned());
            }
        }
    }
    // Handed over (or not to be): another launch in this clone may decide.
    drop(validation.lock.take());

    // Stay the parent: leaving must run when the session ends, however it ends.
    // The session leads a process group of its own (holding the terminal when
    // ohjaa does), so a later launch can tell whether it still runs; the
    // entering record names the group before the session execs.
    let record = {
        let (dirs, id) = (dirs.clone(), session_id.clone());
        Box::new(move |pgid: u32, at_ms: i64| {
            if let Some(store) = session_store(&dirs) {
                if let Err(error) = store.update(&id, |s| {
                    s.pgid = Some(pgid);
                    s.group_started_ms = Some(at_ms);
                }) {
                    eprintln!("ohjaa: {error}; the session's process group is not recorded");
                }
            }
        })
    };
    let group = lifecycle::own_group(&mut command, record);
    let spawned = command.spawn();
    let group = lifecycle::group_started(group, spawned.as_ref().ok());
    let mut child = match spawned {
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
                landed: Vec::new(),
                observed: Vec::new(),
                shared: Vec::new(),
                orphaned: false,
            };
            if let Some(store) = session_store(&dirs) {
                let _ = store.update(&session_id, |s| s.left = Some(left));
            }
            lifecycle::restore_signals();
            return Err(error).context("failed to run toimija");
        }
    };
    if let Some((pgid, at_ms)) = group {
        session.pgid = Some(pgid);
        session.group_started_ms = Some(at_ms);
    }
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
    // Other sessions in these repositories while this one ran: what was made
    // in a place they shared at that time is not this session's (law 3). From
    // the log, and the live sessions toimija saw at launch.
    let siblings: Vec<(String, PathBuf)> = in_repo
        .then_some(&scan)
        .into_iter()
        .chain(scoped_scans.iter())
        .chain(clone_scans.iter())
        .flat_map(|scan| scan.occupying().filter(|s| !s.is_quiet()))
        .map(|s| {
            let worktree =
                std::fs::canonicalize(&s.worktree).unwrap_or_else(|_| s.worktree.clone());
            (format!("toimija session {}", s.id), worktree)
        })
        .collect();
    let windows = unless_interrupted(|| {
        other_windows(
            &dirs,
            &session_id,
            started_ms,
            &siblings,
            &ended_at_entering,
        )
    })
    .unwrap_or_default();
    // Asked at most once, and only when a commit needs it. A repository with
    // an entry is judged by its gate.
    let sykli = lifecycle::sykli_with_entries();
    // Outside a repository the folder itself has no commits or pull request
    // to observe; the record says why, and asks neither git nor gh there.
    let launch = if in_repo {
        observe_repo(
            &root,
            session.head_at_start.as_deref(),
            started_ms,
            Some(&start_of(&root)),
            &windows,
            &sykli,
        )
    } else {
        Observed {
            phases: None,
            observed: Vec::new(),
            shared: Vec::new(),
            finished: sessions::Finished::Unknown {
                why: "not a Git repository".to_owned(),
            },
            landed: Vec::new(),
            made: false,
        }
    };
    // Every scoped repository is observed the same way as the launch one.
    let mut scoped: Vec<sessions::ScopedLeft> = session
        .scoped
        .iter()
        .map(|scoped| {
            let seen = observe_repo(
                Path::new(&scoped.repo),
                scoped.head_at_start.as_deref(),
                started_ms,
                Some(&start_of(Path::new(&scoped.repo))),
                &windows,
                &sykli,
            );
            sessions::ScopedLeft {
                repo: scoped.repo.clone(),
                phases: seen.phases,
                finished: seen.finished,
                found: false,
                landed: seen.landed,
                observed: seen.observed,
                shared: seen.shared,
            }
        })
        .collect();
    // And every repository nobody named, directly in the launch folder (or a
    // folder --scope named), where a commit was made while the session ran:
    // observed, never attributed to it.
    let known: Vec<PathBuf> = in_repo
        .then(|| folder.clone())
        .into_iter()
        .chain(session.scoped.iter().map(|s| PathBuf::from(&s.repo)))
        .collect();
    for repo in search
        .iter()
        .flat_map(|dir| unless_interrupted(|| lifecycle::repos_touched_in(dir, started_ms)))
        .flatten()
    {
        if known.contains(&repo) || scoped.iter().any(|s| Path::new(&s.repo) == repo) {
            continue;
        }
        let seen = observe_repo(&repo, None, started_ms, None, &windows, &sykli);
        if seen.made {
            scoped.push(sessions::ScopedLeft {
                repo: repo_key(&repo),
                phases: seen.phases,
                finished: seen.finished,
                found: true,
                landed: seen.landed,
                observed: seen.observed,
                shared: seen.shared,
            });
        }
    }
    let left = sessions::Left {
        ended_ms: intake::now_seconds() * 1000,
        exit,
        toimija_session,
        phases: launch.phases,
        finished: launch.finished,
        interrupted: process::cancelled(),
        scoped,
        landed: launch.landed,
        observed: launch.observed,
        shared: launch.shared,
        orphaned: false,
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
    // A landing this session saw reach an entry's observed branch: sykli is
    // asked to validate the integrated State, recorded in this session
    // first (D5). Only once the leaving is recorded, and not when a signal
    // cut leaving short.
    let mut integrated_lines = Vec::new();
    if recorded_ok && !process::cancelled() {
        integrated_lines = integrated_at_leaving(&dirs, &session, &sykli);
        if !integrated_lines.is_empty() {
            if let Some(updated) =
                session_reader(&dirs).and_then(|log| log.get(&session_id).ok().flatten())
            {
                session = updated;
            }
        }
    }
    let _ = std::io::stdout().flush();
    // Written with `let _`: a closed terminal must not turn printing into a
    // panic after the record is already safe.
    let mut stderr = std::io::stderr();
    // The validations it requested, with what sykli says of them now.
    let runs = lifecycle::RunStatus::default();
    let _ = writeln!(
        stderr,
        "\n{}",
        sessions::render_record_with(&session, &|v| runs.ask(v))
    );
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
    for line in &integrated_lines {
        let _ = writeln!(stderr, "ohjaa: {line}");
    }
    say_weekly(weekly);
    // Said again last, where the person looks when the session ends.
    for flag in sessions::flags(&session) {
        let _ = writeln!(stderr, "ohjaa: flagged: {}", flag.headline);
    }
    lifecycle::restore_signals();
    Ok(lifecycle::exit_code(exit))
}

/// At leaving: each commit the session saw land on an entry's observed
/// branch (`origin/<branch>`) that the branch still sits at, in the launch
/// repository and every one it named with `--scope`. sykli is asked what
/// applies to it, and an integrated validation is asked for where it has no
/// current receipt (see `integrated::at_landing`). The lines to say.
fn integrated_at_leaving(
    dirs: &Dirs,
    session: &sessions::Session,
    sykli: &lifecycle::Sykli,
) -> Vec<String> {
    let Some(left) = &session.left else {
        return Vec::new();
    };
    let launch = (!session.outside_repo).then(|| (session.repo.as_str(), &left.landed[..]));
    let scoped = left
        .scoped
        .iter()
        .filter(|scoped| !scoped.found)
        .map(|scoped| (scoped.repo.as_str(), &scoped.landed[..]));
    let log = sessions::store_path(&dirs.state);
    let mut caps = None;
    let mut lines = Vec::new();
    for (repo, landed) in launch.into_iter().chain(scoped) {
        let Some(gate) = sykli.gate_for(Path::new(repo)) else {
            continue;
        };
        let onto = format!("origin/{}", gate.branch);
        for landed in landed.iter().filter(|l| l.at_tip && l.onto == onto) {
            let caps = caps.get_or_insert_with(|| integrated::Caps::ask(true));
            // The log is read again under the clone's lock: another ohjaa may
            // have asked already.
            lines.push(integrated::at_landing(
                &log,
                &dirs.state,
                &session.id,
                &gate,
                &landed.commit,
                caps,
            ));
        }
    }
    lines
}

/// Sessions left open by an ohjaa that is gone (killed, crashed or restarted:
/// only its absence is observed), so its leaving never ran. Each is closed now
/// as orphaned (observed at this launch), which queues it for `ohjaa intake`
/// and ranks it by its start among the sessions where it ran; with `write`
/// false (a dry run) it is only said. A session whose ohjaa may still run, or
/// cannot be told, is left alone. So is one whose own process group (its
/// entering record names it) still runs: its agent outlived its ohjaa, and
/// it is returned apart, as still running.
fn close_orphans(dirs: &Dirs, write: bool) -> (Vec<sessions::Session>, Vec<sessions::Session>) {
    // A dry run only says what it would close: it opens the log to read.
    let store = if write {
        let Some(store) = session_store(dirs) else {
            return (Vec::new(), Vec::new());
        };
        Some(store)
    } else {
        None
    };
    let all = match &store {
        Some(store) => store.all(),
        None => match session_reader(dirs) {
            Some(log) => log.all(),
            None => return (Vec::new(), Vec::new()),
        },
    };
    let Ok((all, _)) = all else {
        return (Vec::new(), Vec::new());
    };
    let now_ms = intake::now_seconds() * 1000;
    let mut closed = Vec::new();
    let mut still_running = Vec::new();
    for session in all.into_iter().filter(|s| s.left.is_none()) {
        match sessions::observe_open(&session) {
            sessions::Open::Running => continue,
            // Its ohjaa is gone; its agent is not.
            sessions::Open::OhjaaGone => {
                still_running.push(session);
                continue;
            }
            sessions::Open::Orphaned => {}
        }
        let left = sessions::Left::orphaned(now_ms);
        let Some(store) = &store else {
            let mut session = session;
            session.left = Some(left);
            closed.push(session);
            continue;
        };
        let updated = store.update(&session.id, |s| {
            if s.left.is_none() {
                s.left = Some(left);
            }
        });
        match updated {
            Ok(Some(updated)) if updated.left.as_ref().is_some_and(|l| l.orphaned) => {
                closed.push(updated)
            }
            Ok(_) => {}
            Err(error) => eprintln!("ohjaa: {error}"),
        }
    }
    (closed, still_running)
}

/// Every other session's time in a checkout while this one ran (`started_ms`
/// to now): from the log, each session's launch checkout and the
/// repositories it named; and the live sessions toimija saw at launch
/// (`siblings`), for all of this session. A session that had ended before this
/// one entered (`ended_before`) shared nothing with it. Read in one short open
/// of the log.
fn other_windows(
    dirs: &Dirs,
    id: &str,
    started_ms: i64,
    siblings: &[(String, PathBuf)],
    ended_before: &std::collections::HashSet<String>,
) -> Vec<lifecycle::Window> {
    let now_ms = intake::now_seconds() * 1000 + 999;
    let mut windows: Vec<lifecycle::Window> = siblings
        .iter()
        .map(|(id, checkout)| lifecycle::Window {
            id: id.clone(),
            checkout: checkout.clone(),
            from_ms: started_ms,
            to_ms: now_ms,
        })
        .collect();
    let Some((all, _)) = session_store(dirs).and_then(|store| store.all().ok()) else {
        return windows;
    };
    for other in all
        .into_iter()
        .filter(|s| s.id != id && !ended_before.contains(&s.id))
    {
        let to_ms = other
            .left
            .as_ref()
            .map_or(now_ms, |left| left.ended_ms + 999);
        if to_ms < started_ms {
            continue;
        }
        let checkouts = (!other.outside_repo)
            .then(|| other.repo.clone())
            .into_iter()
            .chain(other.scoped.iter().map(|s| s.repo.clone()));
        for checkout in checkouts {
            windows.push(lifecycle::Window {
                id: other.id.clone(),
                checkout: PathBuf::from(checkout),
                from_ms: other.started_ms,
                to_ms,
            });
        }
    }
    windows
}

/// Run one step of leaving's observing, unless a signal has cut leaving short.
fn unless_interrupted<T>(step: impl FnOnce() -> T) -> Option<T> {
    (!process::cancelled()).then(step)
}

/// What leaving observed in one repository.
struct Observed {
    /// This session's own commits.
    phases: Option<Vec<sessions::Phase>>,
    /// Commits made during the session that are not shown to be its own.
    observed: Vec<sessions::Phase>,
    /// The other sessions that shared its places, said for people.
    shared: Vec<String>,
    finished: sessions::Finished,
    /// The commits that reached the default branch, each tip with what sykli
    /// said.
    landed: Vec<sessions::Landed>,
    /// Whether any commit was made, or pushed from here, during the session.
    made: bool,
}

/// Leaving's observation of one repository: the commits made there during
/// the session, in any of its checkouts, and which are this session's own
/// (see `lifecycle::observe`); with what sykli saw on each, and the pull
/// request of its own branch. `start` is what the repository held at
/// entering, read for a repository the session was launched in or named;
/// a repository only found at leaving has none, and nothing in it is the
/// session's. sykli is asked about the pull request's head and about the tip
/// of every push of the session's to the default branch.
fn observe_repo(
    repo: &Path,
    head_at_start: Option<&str>,
    started_ms: i64,
    start: Option<&lifecycle::Start>,
    windows: &[lifecycle::Window],
    sykli: &lifecycle::Sykli,
) -> Observed {
    let own = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let named = start.is_some().then_some(own.as_path());
    // Reflog times are whole seconds. With nothing read at entering, a move in
    // the second the session started may be from before it: only later ones
    // count.
    let (started_ms, start) = match start {
        Some(start) => (started_ms, start.clone()),
        None => (
            (started_ms.div_euclid(1000) + 1) * 1000,
            lifecycle::Start::default(),
        ),
    };
    let mut work = unless_interrupted(|| {
        lifecycle::observe(repo, head_at_start, started_ms, &start, named, windows)
    })
    .unwrap_or_default();
    let not_looked_up = || sessions::Finished::Unknown {
        why: "not looked up: this session made no commit on a branch".to_owned(),
    };
    // The pull request of this session's own branch, never whatever branch a
    // checkout holds now, else the one holding its commit no branch names any
    // more; with no commit of its own on a branch, none is asked.
    let mut finished = match &work.pr_commit {
        None => not_looked_up(),
        Some(commit) => match unless_interrupted(|| {
            lifecycle::finished(
                repo,
                work.pr_branch.as_deref(),
                Some(commit),
                &work.receipts,
                &work.checkouts,
                sykli,
            )
        }) {
            Some(sessions::Finished::Unknown { .. }) if process::cancelled() => None,
            other => other,
        }
        .unwrap_or_else(|| sessions::Finished::Unknown {
            why: "interrupted".to_owned(),
        }),
    };
    // Commits no reference names any more are work only when a pull request
    // holds them; otherwise (a stash, say) they are left out entirely.
    let lost = std::mem::take(&mut work.lost);
    if work.pr_branch.is_none() && !lost.is_empty() {
        if matches!(finished, sessions::Finished::PullRequest { .. }) {
            for (phase, by) in lost {
                match by {
                    None => work.phases.get_or_insert_with(Vec::new).push(phase),
                    Some(by) => {
                        if !work.shared.contains(&by) {
                            work.shared.push(by);
                        }
                        work.observed.push(phase);
                    }
                }
            }
        } else {
            finished = not_looked_up();
        }
    }
    let shared = work
        .shared
        .iter()
        .map(|w| {
            format!(
                "with {} in {}, {} to {}: what was made then where it was is not attributed",
                w.id,
                w.checkout.file_name().unwrap_or_default().to_string_lossy(),
                sessions::clock(w.from_ms),
                &sessions::clock(w.to_ms)[11..]
            )
        })
        .collect();
    let landed = work
        .landed
        .iter()
        .map(|arrived| {
            // sykli is asked about the tip of each of this session's pushes, as a
            // pre-push Boundary would be, and about where the remote default
            // branch is now, whoever pushed it: a broken main is the
            // repository's state. The commits before a tip are their receipts'
            // word.
            let check = ((arrived.tip && arrived.attributed)
                || (arrived.at_tip && arrived.onto.contains('/')))
            .then(|| {
                unless_interrupted(|| {
                    sykli.check(
                        repo,
                        &arrived.commit,
                        &arrived.tree,
                        &work.receipts,
                        &work.checkouts,
                        arrived.alive,
                    )
                })
                .unwrap_or_else(|| sessions::Check::Unverified {
                    why: "leaving was interrupted".to_owned(),
                })
            });
            // A proven commit: the receipt sykli accepted, so a reuse of its
            // cache is said as one.
            let proof = (check == Some(sessions::Check::Passed))
                .then(|| sykli.proof(&arrived.commit))
                .flatten();
            sessions::Landed {
                commit: arrived.commit.clone(),
                subject: arrived.subject.clone(),
                onto: arrived.onto.clone(),
                check,
                evidence: proof.or_else(|| {
                    work.receipts
                        .get(&arrived.tree)
                        .map(|receipt| receipt.evidence.clone())
                }),
                at_tip: arrived.at_tip,
                unattributed: !arrived.attributed,
            }
        })
        .collect();
    Observed {
        phases: work.phases,
        observed: work.observed,
        shared,
        finished,
        landed,
        made: work.made,
    }
}

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
        match lifecycle::repo_of(&scope_path(raw)) {
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
fn lifecycle_briefing(intent: &str, root: &Path, in_repo: bool, scoped: &[PathBuf]) -> String {
    let names = scoped
        .iter()
        .map(|repo| repo.file_name().unwrap_or_default().to_string_lossy())
        .collect::<Vec<_>>()
        .join(", ");
    let phases = if !in_repo && scoped.is_empty() {
        "2. This folder is not a Git repository, and no --scope named one. At leaving,
   ohjaa records the repositories directly in it whose branches or worktrees
   moved, but it cannot tell your work there from another agent's. Say in your
   handover what you changed, and in which repos. Declare any repo you work in
   with `toimija intent \"<why>\" --scope <path>` so sessions there can see you."
            .to_owned()
    } else if !in_repo {
        format!(
            "2. You were launched for: {names}. Work in small phases there; end each with a
   commit, and `sykli run` where that repository has a sykli contract. ohjaa
   records your commits and pull request in each of them. Another repository
   directly in this folder is recorded only as found, if it moved: name it in
   your handover."
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
{intent}If no concrete task is assigned, acknowledge and wait; the wrapper has already
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
    // `ohjaa hook <name> ARGS…` hands git's arguments on exactly as git gave
    // them. Through clap, one that looks like an ohjaa flag (`--root`) would be
    // taken as one, so the dispatcher's path goes around it; `--help` still
    // goes through clap.
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if argv.len() >= 3 && argv[1] == "hook" && !argv[2].to_string_lossy().starts_with('-') {
        let code = hook::run(&argv[2].to_string_lossy(), &argv[3..]).unwrap_or_else(|error| {
            eprintln!("ohjaa hook: {error:#}");
            2
        });
        std::process::exit(code);
    }
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::Run {
            agent,
            intent,
            intent_id,
            task,
            scope,
            dry_run,
            no_briefing,
            usage_file,
            passthrough,
        } => cmd_run(RunOptions {
            agent,
            intent: intent.as_deref(),
            intent_id: intent_id.as_deref(),
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
        Cmd::Report { since, json, marks } => cmd_report(&cli.root, since, *json, *marks),
        Cmd::Graph { id } => cmd_graph(id),
        Cmd::Apply {
            dry_run,
            force,
            prune,
            bin,
            undo,
        } => {
            if *undo {
                apply::undo(*dry_run)
            } else {
                apply::apply(
                    apply::Options {
                        dry_run: *dry_run,
                        force: *force,
                        prune: *prune,
                        bin: bin.as_deref(),
                    },
                    &cli.root,
                    git_root_for_roots(&cli.root).as_deref(),
                )
            }
        }
        Cmd::Doctor => doctor::doctor(&cli.root, git_root_for_roots(&cli.root).as_deref()),
        Cmd::Adopt {
            path,
            branch,
            force,
        } => adopt::adopt(path, branch.as_deref(), *force),
        Cmd::Hook { name, args } => hook::run(name, args),
        Cmd::ShellInit { shell } => shell_init::shell_init(shell),
        Cmd::Gate { print, wait } => gate::gate_here(*print, std::time::Duration::from_secs(*wait)),
        Cmd::Judge {
            action: Some(JudgeCmd::Respond { judgement, text }),
            ..
        } => judgement::respond(judgement, &text.join(" ")),
        Cmd::Judge {
            action: None,
            range,
            verdict,
            findings,
            severity,
            blocking,
            uncertainty,
            intent_id,
            push,
        } => match verdict {
            Some(verdict) => judgement::judge(&judgement::JudgeArgs {
                range,
                verdict: *verdict,
                findings,
                severity: *severity,
                blocking: *blocking,
                uncertainty: uncertainty.as_deref(),
                intent_id: intent_id.as_deref(),
                push: *push,
            }),
            None => Err(anyhow::anyhow!("--verdict is required")),
        },
        Cmd::Judgements {
            rebuild_public: true,
            ..
        } => judgement::rebuild_public(),
        Cmd::Judgements {
            commit,
            json,
            fetch,
            ..
        } => judgement::list(commit, *json, *fetch),
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
