//! ohjaa: steering knowledge for agents.
//!
//! Toimija reports facts and refuses to decide. Ohjaa holds the other half:
//! the graph of what the people an agent works for know and have decided
//! (faija-ontology.v2, knowledge-node.v1), and it hands that graph to every
//! agent *before* it acts, together with the live perimeter of the fleet.
//! Agents may propose Knowledge; only a person makes it current.

mod adopt;
mod apply;
mod collect;
mod derived;
mod doctor;
mod evidence;
mod flat_toml;
mod gate;
mod gitconfig;
mod home;
mod hook;
mod human;
mod instructions;
mod intake;
mod integrated;
mod knowledge;
mod lessons;
mod lifecycle;
mod mark;
mod process;
mod repo_docs;
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
    /// The system perimeter: what every tool in the fleet is, derived now.
    System {
        #[arg(long)]
        json: bool,
    },
    /// List the knowledge that applies here, each current node with how often
    /// a person had to teach it again since it was accepted.
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
    /// goes into its repository's AGENTS.md, through a pull request ohjaa
    /// opens (`ohjaa lessons publish` delivers it later when it cannot now).
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
    /// Repository knowledge, in each repository's own AGENTS.md.
    Lessons {
        #[command(subcommand)]
        action: LessonsCmd,
    },
    /// Validate every node, flag what may no longer hold, and suggest what
    /// to reword or re-confirm (suggestions only: they never fail `--strict`).
    Check {
        /// Briefing size, in characters, past which it is too long to be read.
        #[arg(long, default_value_t = knowledge::DEFAULT_BUDGET)]
        budget: usize,
        /// Exit non-zero on flags too, not only on errors.
        #[arg(long)]
        strict: bool,
    },
    /// Draft proposals from what people said: what they wrote in agent
    /// sessions (through `vessa messages`), pull request comments and reverts.
    Intake {
        /// Start here (YYYY-MM-DD or RFC 3339). Default: where the last intake
        /// stopped, else fourteen days ago.
        #[arg(long)]
        since: Option<String>,
        /// Send every message people wrote, not only those the local
        /// pre-filter takes for a correction or a repeated instruction.
        #[arg(long)]
        no_prefilter: bool,
        /// transcripts, reviews, reverts or report (repeatable). Default: all four.
        #[arg(long = "source", value_name = "SOURCE")]
        sources: Vec<String>,
        /// The command that drafts: it reads the prompt on stdin and prints a
        /// JSON object: its drafts, and what each candidate re-teaches. Default:
        /// ~/.config/ohjaa/drafter, else `claude -p`.
        #[arg(long)]
        drafter: Option<String>,
        /// Candidates per drafter call.
        #[arg(long, default_value_t = 40)]
        batch: usize,
        /// At most this many batches a run (a batch the drafter refuses is
        /// sent again in halves); the rest wait for the next run.
        #[arg(long, default_value_t = 10)]
        max_batches: usize,
        /// Show what would be sent, and send nothing.
        #[arg(long)]
        dry_run: bool,
        /// Send again the candidates set aside after the drafter failed on
        /// each, alone, three times.
        #[arg(long)]
        retry_failed: bool,
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
    /// For a terminal's opening: only matters needing attention or your decision.
    /// Silent when there are none. Activity stays unread for `ohjaa digest`.
    #[command(
        after_help = "Prints, reads and moves nothing unless stdout is a terminal and no agent \
        session's marker is set (OHJAA_SESSION, TOIMIJA_SESSION, TOIMIJA_RUN, CLAUDECODE, \
        CLAUDE_CODE_ENTRYPOINT, CLAUDE_CODE_SESSION_ID, CODEX_THREAD_ID, CODEX_SESSION_ID, \
        CODEX_CI, CODEX_SANDBOX, CODEX_SANDBOX_NETWORK_DISABLED, CODEX_MANAGED_BY_NPM, \
        PI_SESSION_ID, PI_CODING_AGENT, OPENCODE, OPENCODE_SESSION_ID, AI_AGENT): an agent's \
        shell reads ~/.zshrc too. Terminals opened together say a line once.\n\n\
        Starts no git and touches no network: it reads the session log and the proposals, and \
        the rest from the snapshot `ohjaa collect` writes, said with its age when it is older \
        than 15 minutes. Then it starts one `ohjaa collect` in the background, unless one \
        runs; startup never waits for it. Notices are shown once per day or when they change. \
        Opening a terminal does not consume session or pull-request history. Always exits 0."
    )]
    Line,
    /// Attention details and unread activity: sessions, pull requests, proposed
    /// instructions and who is live. Reading it at a terminal marks activity seen.
    #[command(
        after_help = "When the snapshot is more than 5 minutes old it starts a collect \
        and waits for it at most 10 s; past that it shows the snapshot it has, and the collect \
        goes on in the background. Who is live is asked of each repository's toimija now.\n\n\
        Exit codes:\n  \
        0  everything could be read\n  \
        4  something could not be read (it says what), or nothing was collected yet"
    )]
    Digest {
        /// Show everything since then instead (`7d`, `24h`, a date or an RFC
        /// 3339 time); the cursor does not move.
        #[arg(long)]
        since: Option<String>,
        /// Use the snapshot as it is, however old.
        #[arg(long)]
        no_collect: bool,
    },
    /// Gather what `ohjaa line` cannot work out at a terminal's opening into
    /// its snapshot: the marks and open sessions, GitHub's pull requests,
    /// vessa's usage of recent sessions and the first words typed in each,
    /// and the secret-shaped tokens vessa found in agent sessions.
    #[command(after_help = "Each part asked once, at the same time:\n  \
        marks     as `ohjaa report --marks` finds them (git, sykli, ps)\n  \
        github    gh api graphql: the pull requests updated in the entries' owners, at most 2 calls\n  \
        vessa     vessa usage --json --input <transcript>…: only the sessions ended lately\n  \
        messages  vessa messages --json --since …: the first words typed in each of them\n  \
        secrets   vessa secrets --json --since …: where and how many, never a value\n\
        vessa is asked at most 3 times. The first words are kept only for the window the digest\n\
        shows. A part that fails keeps what it held before, said as of then, and so does a branch\n\
        whose marks cannot be read. Writes $XDG_STATE_HOME/ohjaa/human/collected.json,\n\
        holding human/collect.lock while it runs.\n\n\
        Exit codes:\n  \
        0  every part answered, and read all it asked for\n  \
        4  a part did not, or could not read all of it (the snapshot says what)\n  \
        1  the snapshot could not be written")]
    Collect {
        /// Started by `line` or `digest`: says nothing.
        #[arg(long, hide = true)]
        background: bool,
    },
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
           one on PATH (see --bin), or --prune without a roots file\n  \
        4  applied, except an agent's own instruction file apply cannot write (a\n     \
           block it cannot tell from the rest): each is named, with why, on every\n     \
           apply and dry run until it is fixed by hand; `ohjaa doctor` names it too\n\n\
        Writes in the home:\n  \
        repos/<o>/<r>/hooks/<name>    exec <ohjaa> hook <name> \"$@\": pre-push, and each\n                                \
        hook that entry's clones under the roots have of their own\n  \
        repos/<o>/<r>/gated.config    core.hooksPath = that entry's hooks/\n  \
        git/config                    includeIf \"hasconfig:remote.*.url:<pattern>\" per entry\n  \
        roots                         from --root on the first run: where the repositories are\n  \
        applied.toml                  what apply changes outside the home, kept before each change\n  \
        .git/                         the home's history: what apply changed, one commit (made\n                                \
        on the home's first commit; never pushed)\n\
        Changes outside it, and nothing else:\n  \
        git's global config    [include] path = <home>/git/config (once)\n  \
        git's global excludes  a block marked \"ohjaa apply\" holding .sykli/ .teko/ .ohjaa/\n  \
        ~/.zshrc               [[ -x <ohjaa> ]] && eval \"$(<ohjaa> shell-init zsh)\", in place\n                         \
        of a line that sources ~/projects/ohjaa-launch.zsh\n  \
        each agent's own       a block marked \"ohjaa:knowledge\" holding the team's, mine and machine\n  \
        instruction file       knowledge, in each that exists outside every repository:\n                         \
        ~/.claude/CLAUDE.md, ~/.codex/AGENTS.md, ~/.pi/agent/AGENTS.md,\n                         \
        ~/.config/opencode/AGENTS.md (none is created); each decision\n                         \
        rewrites it where it is\n\
        <ohjaa> is the ohjaa on PATH (or --bin), and the pre-push dispatchers name\n\
        the sykli on PATH, both by absolute path. Dispatchers are only added;\n\
        --prune removes the ones no clone needs. --undo puts the changes back\n\
        as they were (a hooksPath --force removed comes back at the end of [core],\n\
        a global config apply created is removed), and removes what apply wrote\n\
        in the home except the entries, the roots and the history.")]
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
    /// Write a repository's entry into the home: `repo.toml`, with the branch
    /// ohjaa observes (origin's default branch) and the `[cargo_env]` its gate
    /// gives cargo tasks; then regenerate the home's git config. The entry
    /// keeps no copy of a gate: every commit's gate is derived from that
    /// commit's own sykli.json, else from what `sykli init` writes for its
    /// tree, and a commit whose only contract is a sykli.rs has none; --force
    /// removes a copy an earlier adopt kept. Neither git's
    /// global config nor the repository is written.
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
        sykli hook pre-push --contract <gate> --branch <branch> <remote> <url>\n\
        for at most 15 s, where <gate> is the gate derived from the pushed commit\n\
        (its own sykli.json, else what sykli init writes for its tree), written\n\
        outside the repository. When sykli does not accept it, the commit is asked\n\
        about as the marks ask (a receipt in any worktree of the clone). When that\n\
        does not prove it either, or sykli cannot judge it (no sykli, one without\n\
        --contract and --branch, a commit with no gate: only a sykli.rs, or a tree\n\
        sykli init writes none for), the push goes on and\n\
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
    /// Print what a terminal runs as it opens: `ohjaa line`. Load it with:
    /// eval "$(ohjaa shell-init zsh)"
    ShellInit {
        /// zsh is the only one.
        shell: String,
    },
}

#[derive(Subcommand)]
enum LessonsCmd {
    /// Deliver repo knowledge accepted but not in its repository's AGENTS.md
    /// yet: each `<repo>/.ohjaa/knowledge/` file, and each accepted node not
    /// delivered. One pull request per repository adds their lines to the
    /// `## Lessons` section of its AGENTS.md; once it is open, each file moves
    /// out of the checkout into ohjaa's state, recorded with its URL.
    #[command(after_help = "Exit codes:\n  \
        0  every line is delivered: in a pull request, or in AGENTS.md already\n  \
        4  some are not: no GitHub remote, no gh, gh or git could not, or a line held back\n  \
        1  it could not run\n\n\
        Per repository, from a checkout of it, never in it:\n  \
        gh repo view <owner/repo> --json visibility,defaultBranchRef\n  \
        gh api user, gh api user/repos?… --paginate\n                              \
        for a repository that is not private, once a run: your login\n                              \
        and name, and every repository you can see that is not public\n  \
        gh pr list -R <owner/repo> --author @me\n                              \
        your open pull request of the lessons gets the next commit\n  \
        git fetch https://github.com/<owner/repo>.git <branch>, then a worktree off it in\n  \
        $XDG_STATE_HOME/ohjaa/lessons/.work/, git commit, git push to the same URL\n  \
        HEAD:refs/heads/agents-md-lessons/<date>-<n>, gh pr create, and the worktree removed;\n  \
        git reaches GitHub by HTTPS with gh's credentials (no SSH key's prompt holds it up)\n\
        What goes to a repository names no tool. For one that is not private, all of it is\n\
        checked first: a line that names ohjaa, one of your repositories that is not public,\n\
        your home path, your account or your name is held back and said, its text sent\n\
        nowhere and its file left; anything else that would stops the delivery. AGENTS.md is\n\
        read whole or not at all. A dry run reads AGENTS.md as last fetched here, and fetches,\n\
        writes and sends nothing.")]
    Publish {
        /// The repositories (any directory inside each). Default: every
        /// repository under the roots with a `.ohjaa/knowledge/`, and every
        /// one with accepted lessons not delivered.
        repos: Vec<PathBuf>,
        /// Say, per repository, its visibility, each line it would add and
        /// what a line names that is private; fetch, write and send nothing.
        #[arg(long)]
        dry_run: bool,
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

/// What to say about an incomplete scan: the failure if there was one, else
/// the first warning, else the bare verdict.
fn coverage_detail(scan: &Scan) -> &str {
    scan.error
        .as_deref()
        .or_else(|| scan.warnings.first().map(String::as_str))
        .unwrap_or("degraded coverage")
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

/// A knowledge section of a briefing.
struct Briefed {
    text: String,
    /// Exactly which nodes it delivers, and those the agent's own file did.
    delivered: Vec<sessions::Delivered>,
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
/// its AGENTS.md holds as checked out are left to it, but for a flagged
/// one, and that is said in one line too; every other repo node says why it
/// is not left to AGENTS.md (a pull request pending, not delivered).
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
            "ohjaa: left {}: ohjaa's state holds that node, so this copy is not read \
             (`ohjaa lessons publish` names it until it is taken out)",
            left.display()
        );
    }
    for error in fk::validate_set(&loaded.nodes) {
        eprintln!("ohjaa: knowledge does not hold together: {error}");
    }
    let flags = knowledge::flags(&loaded, dirs.repo_root.as_deref(), knowledge::today());
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
        .then(|| lessons::Standing::read(dirs, &loaded))
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
            let note = standing.as_ref()?.note(&located.node)?;
            Some((located.node.id.clone(), note))
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
    let delivered = current
        .iter()
        .filter_map(|located| {
            let via = if kept(located) {
                None
            } else if held(located) {
                // Delivered by the agent's own file.
                left_out.map(|(file, _)| file.to_string_lossy().into_owned())
            } else if in_lessons(located) {
                // Delivered by the repository's AGENTS.md.
                standing
                    .as_ref()
                    .map(|standing| standing.agents_md.to_string_lossy().into_owned())
            } else {
                // Named only by count.
                return None;
            };
            Some(sessions::Delivered {
                id: located.node.id.clone(),
                scope: located.scope.name().to_owned(),
                as_of: located.node.as_of.to_string(),
                repo: (located.scope == Scope::Repo)
                    .then(|| dirs.repo_root.as_ref().map(|p| repo_key(p)))
                    .flatten(),
                via,
            })
        })
        .collect();
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
        delivered,
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
) -> Result<(String, Vec<sessions::Delivered>)> {
    let Briefed {
        mut text,
        mut delivered,
        source,
    } = knowledge_briefing_for(dirs, &Scope::ALL, agent, true)?;
    for repo in scoped {
        let mut scoped_dirs = dirs.clone();
        scoped_dirs.repo_root = Some(repo.clone());
        let Briefed {
            text: knowledge,
            delivered: nodes,
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
        delivered.extend(nodes);
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
    format!("{}\n\nOnly the launch repository, declared scope and sessions reaching them are included.\nOther repositories were not scanned. Use `ohjaa system` or `toimija sessions` for a\nfleet-wide question; after expanding scope, use `ohjaa brief --scope <path>`.\nUse the delivered facts instead of repeating discovery. Refresh session observations\nbefore editing if work has moved on since this briefing.\n", system::briefing(view, here))
}

fn cmd_brief(
    roots: &[PathBuf],
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
    let (knowledge, _) = knowledge_for_work(&dirs, &scoped, agent.map(|agent| (agent, &[][..])))?;
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
                let caps = integrated::Caps::ask();
                let all = session_reader(&dirs)
                    .and_then(|log| log.all().ok())
                    .map(|(all, _)| all)
                    .unwrap_or_default();
                let head = gitconfig::read_head(top, &marks.gate.common);
                let view = integrated::at_launch(
                    marks,
                    &launch.sykli,
                    head.as_ref()
                        .map(|(commit, branch)| (commit.as_str(), branch.as_deref())),
                    &all,
                    &caps,
                );
                let text = view.briefing();
                if !text.is_empty() {
                    println!("{text}");
                }
            }
        }
    }
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

/// The session named in the environment, as an earlier `ohjaa run` set it.
fn current_session_id() -> Result<String> {
    let id = std::env::var(sessions::SESSION_ENV).map_err(|_| {
        anyhow::anyhow!(
            "no --session given, and {} is not set",
            sessions::SESSION_ENV
        )
    })?;
    if !sessions::is_valid_id(&id) {
        bail!("{} is not a session id: `{id}`", sessions::SESSION_ENV);
    }
    Ok(id)
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
    // Where each of this repository's own current nodes stands: in its
    // AGENTS.md, in a pull request, or not delivered, and why.
    let standing = lessons::Standing::read(&dirs, &loaded);
    // How often each current node was re-taught, and when last touched. A
    // record that cannot be read is said, never shown as no counts.
    let evidence = evidence::Evidence::read(&dirs);
    if let Err(error) = &evidence {
        eprintln!("ohjaa: {error:#}; how often each node was re-taught is not shown");
    }
    let today = knowledge::today();
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
            .then(|| standing.as_ref()?.status(node))
            .flatten();
        if let Some(status) = status {
            println!("{:<8} {status}", "");
        }
        if let (Ok(evidence), Status::Current) = (&evidence, node.status) {
            let tally = evidence.tally(located.scope, dirs.repo_root.as_deref(), node, today);
            println!("{:<8} {}", "", tally.line());
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
    Ok(if loaded.broken.is_empty() && evidence.is_ok() {
        0
    } else {
        1
    })
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

/// Before a repo proposal is accepted: what delivering it needs, asked now.
/// Refused, so that nothing is accepted and it stays waiting, when its line
/// would go to a repository that is not private and names something private.
/// `None` for any other proposal.
fn screen_accept(
    dirs: &Dirs,
    id: &str,
    asker: &mut lessons::Asker,
) -> Result<Option<Result<lessons::Ready, String>>> {
    let loaded = knowledge::load(dirs)?;
    let (waiting, _) = knowledge::pending(dirs, &loaded)?;
    let Some(pending) = waiting
        .iter()
        .find(|pending| pending.node.id == id && pending.scope == Scope::Repo)
    else {
        return Ok(None);
    };
    let Some(checkout) = pending.repo.clone().or_else(|| dirs.repo_root.clone()) else {
        return Ok(None);
    };
    lessons::screen(asker, &checkout, &pending.node).map(Some)
}

/// After a person's decision on repo knowledge ohjaa's state holds (an
/// accept, a retirement, a confirmation): its repository's AGENTS.md is
/// brought in line with it through the pull request (`lessons::deliver`),
/// with `ready` when it was asked before the decision. It never undoes or
/// fails the decision: what could not be delivered is said, and `ohjaa
/// list` says it after, until `ohjaa lessons publish` delivers it. Called
/// once the home's lock is let go: the delivery holds only the lessons lock
/// (`lessons::deliver`), so no other change to the home waits on it.
fn deliver_decided(
    dirs: &Dirs,
    outcome: &Outcome,
    ready: Option<Result<lessons::Ready, String>>,
    asker: &mut lessons::Asker,
    today: fk::Date,
) {
    let Some(repo) = outcome.repo.as_deref() else {
        return;
    };
    let accepted = knowledge::accepted_dir(&dirs.state, repo);
    if !outcome
        .written
        .iter()
        .any(|path| path.starts_with(&accepted))
    {
        println!(
            "  lessons: it is still untracked in {}; `ohjaa lessons publish` delivers it to \
             AGENTS.md",
            knowledge::repo_knowledge(repo).display()
        );
        return;
    }
    let ready = ready.unwrap_or_else(|| asker.ready(repo));
    println!(
        "  lessons: delivering, with the home's lock let go: no other change to the home waits \
         on the repository's hooks"
    );
    let delivered = lessons::deliver(&dirs.state, repo, ready, Vec::new(), false, today);
    for line in &delivered.said {
        println!("  lessons: {line}");
    }
    if !delivered.complete {
        println!(
            "  lessons: the decision stands; `ohjaa list` says where its line is, and `ohjaa \
             lessons publish` delivers it once that is fixed"
        );
    }
}

fn cmd_decide(action: &str, id: &str, why: Option<&str>) -> Result<i32> {
    knowledge::require_person(action)?;
    let lock = decision_lock();
    let dirs = dirs()?;
    let today = knowledge::today();
    let mut asker = lessons::Asker::new(roots::load(&[], dirs.repo_root.as_deref()));
    let ready = match action {
        "accept" => screen_accept(&dirs, id, &mut asker)?,
        _ => None,
    };
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
    // The decision is made and recorded: the home's lock goes before a
    // delivery, which may run a repository's hooks for minutes.
    drop(lock);
    if action != "reject" {
        deliver_decided(&dirs, &outcome, ready, &mut asker, today);
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

/// `tally` says, of a current node, how often it was re-taught (see
/// [`evidence::Tally::line`]); `None` for any other.
fn show_pending(
    pending: &knowledge::Pending,
    duplicate_of: Option<&str>,
    loaded: &knowledge::Loaded,
    waiting: &[knowledge::Pending],
    tally: &dyn Fn(&str) -> Option<String>,
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
    show_pending_relationships(pending, duplicate_of, loaded, waiting, tally, false);
}

fn show_pending_relationships(
    pending: &knowledge::Pending,
    duplicate_of: Option<&str>,
    loaded: &knowledge::Loaded,
    waiting: &[knowledge::Pending],
    tally: &dyn Fn(&str) -> Option<String>,
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
        if let Some(line) = tally(base) {
            println!("    {base}: {line}");
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
        if let Some(line) = tally(of) {
            println!("    {of}: {line}");
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
        Scope::Repo => "Accept to prepare a pull request; it is not a repository merge.",
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
    let marks = intake::possible_duplicates(&loaded, &waiting, dirs.repo_root.as_deref());
    let reviewed = intake::review_order(waiting.iter().cloned().zip(marks).collect());
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
    // How often a current node a proposal names was re-taught: the person
    // decides with it in view.
    let evidence = evidence::Evidence::read(&dirs);
    if let Err(error) = &evidence {
        eprintln!("ohjaa: {error:#}; how often a node was re-taught is not shown");
    }
    let mut incomplete = !broken.is_empty() || evidence.is_err();
    let today = knowledge::today();
    let tally = |id: &str| -> Option<String> {
        let evidence = evidence.as_ref().ok()?;
        let located = loaded
            .nodes
            .iter()
            .find(|l| l.node.id == id && l.node.status == Status::Current)?;
        Some(
            evidence
                .tally(
                    located.scope,
                    dirs.repo_root.as_deref(),
                    &located.node,
                    today,
                )
                .line(),
        )
    };
    if list {
        for (pending, of) in &reviewed {
            show_pending(pending, of.as_deref(), &loaded, &waiting, &tally);
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
    let mut asker = lessons::Asker::new(roots::load(&[], dirs.repo_root.as_deref()));
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
        show_pending_relationships(
            pending,
            of.as_deref(),
            &current,
            &still_waiting,
            &tally,
            true,
        );
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
                    show_pending(pending, of.as_deref(), &current, &still_waiting, &tally);
                    println!("  Proposal file: {}", pending.path.display());
                }
                _ => println!("  Choose one action: a, r, s, d or q, then Enter. Nothing changed."),
            }
        };
        let id = &pending.node.id;
        // Held for the decision, never while the person thinks.
        let lock = decision_lock();
        let mut ready = None;
        let action = if accepting { "accept" } else { "reject" };
        let result = if accepting {
            screen_accept(&dirs, id, &mut asker).and_then(|screened| {
                ready = screened;
                knowledge::accept(&dirs, id, today)
            })
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
                    deliver_decided(&dirs, &outcome, ready.take(), &mut asker, today);
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

/// `ohjaa lessons publish`: per repository, its accepted lessons not in its
/// AGENTS.md yet, delivered through one pull request (`lessons::deliver`).
fn cmd_lessons_publish(roots: &[PathBuf], repos: &[PathBuf], dry_run: bool) -> Result<i32> {
    let dirs = dirs()?;
    let today = knowledge::today();
    let mut checkouts = Vec::new();
    let mut complete = true;
    let roots = roots::load(roots, dirs.repo_root.as_deref());
    if repos.is_empty() {
        checkouts = lessons::waiting(&dirs.state, &roots::git_repos(&roots)?);
    } else {
        for repo in repos {
            match gitconfig::repo_at(repo) {
                Ok((top, _)) => checkouts.push(top),
                Err(error) => {
                    eprintln!("ohjaa: {error:#}");
                    complete = false;
                }
            }
        }
    }
    if checkouts.is_empty() && complete {
        println!(
            "nothing waits to be delivered: no repository under the roots has a \
             .ohjaa/knowledge/, and every accepted lesson is delivered"
        );
        return Ok(0);
    }
    let mut asker = lessons::Asker::new(roots);
    for checkout in &checkouts {
        let mut said = Vec::new();
        let extra = lessons::untracked(checkout, &mut said);
        let ready = asker.ready(checkout);
        let delivered = lessons::deliver(&dirs.state, checkout, ready, extra, dry_run, today);
        println!("{}", checkout.display());
        for line in said.iter().chain(&delivered.said) {
            println!("  {line}");
        }
        complete &= delivered.complete;
    }
    if dry_run {
        println!("(a dry run: nothing was fetched, written or sent)");
    }
    Ok(if complete { 0 } else { INCOMPLETE })
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
    // Does it work? Per current node, how often a person had to teach it
    // again, and what that suggests. Suggestions only: never a flag, and
    // ohjaa never changes a node itself.
    let mut suggestions = 0;
    match evidence::Evidence::read(&dirs) {
        Err(error) => {
            println!("error    {error:#}");
            errors += 1;
        }
        Ok(evidence) => {
            let today = knowledge::today();
            let mut nodes: Vec<&fk::Located> = loaded
                .nodes
                .iter()
                .filter(|l| l.node.status == Status::Current)
                .collect();
            nodes.sort_by(|a, b| {
                (a.scope, a.node.kind, &a.node.id).cmp(&(b.scope, b.node.kind, &b.node.id))
            });
            let mut suggested = Vec::new();
            for located in nodes {
                let node = &located.node;
                let tally = evidence.tally(located.scope, dirs.repo_root.as_deref(), node, today);
                println!("evidence {}: {}", node.id, tally.line());
                // A fact flagged as not re-confirmed already asks.
                let flagged_stale = flags.iter().any(
                    |flag| matches!(flag, knowledge::Flag::Stale { id, .. } if *id == node.id),
                );
                match tally.suggestion() {
                    Some(evidence::Suggestion::StillTrue) if flagged_stale => {}
                    Some(suggestion) => suggested.push((&node.id, suggestion)),
                    None => {}
                }
            }
            for (id, suggestion) in &suggested {
                println!("suggest  {id}: {}", suggestion.describe());
            }
            suggestions = suggested.len();
        }
    }
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
                        "scope    {:<8} {} (accepted before, not yet in AGENTS.md: `ohjaa \
                         lessons publish`)",
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
         {suggestions} suggestion(s), briefing {size}/{budget}",
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

#[allow(clippy::too_many_arguments)]
fn cmd_intake(
    roots: &[PathBuf],
    since: Option<&str>,
    sources: &[String],
    drafter: Option<&str>,
    batch: usize,
    max_batches: usize,
    dry_run: bool,
    retry_failed: bool,
    prefilter: bool,
) -> Result<i32> {
    let dirs = dirs()?;
    // One intake at a time: two would send the same candidates and overwrite
    // each other's progress. A dry run writes nothing, and takes no lock.
    let _lock = if dry_run {
        None
    } else {
        match process::lock_file(&dirs.state.join("intake.lock"), std::time::Duration::ZERO) {
            Ok(lock) => Some(lock),
            Err(why) if why.contains("held by another") => {
                eprintln!("ohjaa: intake already running ({why}); nothing was done");
                return Ok(REFUSED);
            }
            Err(why) => bail!(why),
        }
    };
    let started = intake::now_seconds() * 1000;
    let explicit = since.map(intake::normalize_since).transpose()?;
    let cursor = match explicit {
        Some(_) => None,
        None => intake::read_cursor(&dirs)?,
    };
    let (since, keep) = intake::window_start(explicit, cursor, started, 14 * 86_400_000);
    if let Some(since) = keep.filter(|_| !dry_run) {
        intake::write_cursor(&dirs, since)?;
    }
    let sources: Vec<&str> = if sources.is_empty() {
        vec!["transcripts", "reviews", "reverts", "report"]
    } else {
        sources.iter().map(String::as_str).collect()
    };
    // Sessions never drafted from, ended or not (`None`: the log could not be
    // read). Read in one short, read-only open of the log; no store is held
    // across the slow calls below.
    let undrafted: Option<Vec<sessions::Session>> = session_reader(&dirs)
        .and_then(|log| log.all().ok())
        .map(|(all, _)| all.into_iter().filter(|s| !s.drafted).collect());
    // Those that ended are queued: the agent's own handover, and what was
    // said in that session's window, even when the window lies before the
    // cursor.
    let queued: Vec<sessions::Session> = undrafted
        .iter()
        .flatten()
        .filter(|s| s.left.is_some())
        .cloned()
        .collect();
    // A session is drafted from with its transcript window, so only a run
    // that reads transcripts takes it on.
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
    // What earlier runs handled, read first: a retry reads back to the oldest
    // candidate set aside.
    let mut handled = intake::read_handled(&dirs)?;
    // What was seen of each node, before anything is sent: a damaged record
    // stops the run, and is never replaced. A dry run records nothing.
    let mut evidence = if dry_run {
        evidence::Evidence::default()
    } else {
        evidence::Evidence::read(&dirs)?
    };
    let retry: std::collections::HashSet<intake::Key> = if retry_failed {
        handled.set_aside().map(|(key, _)| *key).collect()
    } else {
        std::collections::HashSet::new()
    };
    let read_back = retry
        .iter()
        .map(|key| key.at_ms - 1)
        .min()
        .filter(|floor| *floor < since);
    let mut warnings = Vec::new();
    // Stores vessa knows of and does not read: said, and holding nothing.
    let mut gaps: Vec<String> = Vec::new();
    // The sources that could not all be read: they hold the cursor.
    let mut warned: Vec<&str> = Vec::new();
    let mut transcripts_read_cleanly = false;
    let mut candidates = Vec::new();
    let git_repos = || -> Result<Vec<PathBuf>> {
        let roots = roots::load(roots, git_root().as_deref());
        roots::git_repos(&roots)
    };
    // One source, from `from`, unfiltered: what vessa lists of what people
    // wrote is pre-filtered in the pass below, not when read back for a
    // retry, which takes only candidates already sent.
    let read = |source: &str,
                from: i64,
                warnings: &mut Vec<String>,
                gaps: &mut Vec<String>|
     -> Result<Vec<_>> {
        Ok(match source {
            "transcripts" => intake::transcripts(from, warnings, gaps)
                .into_iter()
                .filter(|said| !said.long)
                .map(|said| said.candidate)
                .collect(),
            "reviews" | "reverts" => {
                let repos = git_repos()?;
                if repos.is_empty() {
                    // Nothing read is not nothing there: the cursor stays.
                    warnings.push(format!("{source}: no git repositories under the roots"));
                    Vec::new()
                } else if source == "reviews" {
                    intake::reviews(&repos, from, warnings)
                } else {
                    intake::reverts(&repos, from, warnings)
                }
            }
            "report" => {
                // What the report finds going around the workflow: real
                // failures and bypasses only. What it could not read is said,
                // and does not hold the cursor back.
                let report = report::build(&dirs, &git_repos()?, from, started);
                for gap in &report.gaps {
                    eprintln!("ohjaa: intake: report: not read: {gap}");
                }
                report::candidates(&report)
            }
            other => bail!("`{other}` is not a source: transcripts, reviews, reverts or report"),
        })
    };
    for source in &sources {
        let before = warnings.len();
        if *source == "transcripts" {
            // One pass, from the earliest point anything needs: the cursor,
            // or the start of the oldest queued session.
            let from = queued
                .iter()
                .map(|s| s.started_ms - 1)
                .min()
                .map_or(since, |earliest| earliest.min(since));
            let said = intake::transcripts(from, &mut warnings, &mut gaps);
            transcripts_read_cleanly = warnings.len() == before;
            // What was said after `since`, and before it in a queued
            // session's window: only those are read, and pre-filtered.
            let said: Vec<intake::Said> = said
                .into_iter()
                .filter(|said| {
                    said.candidate.at_ms > since
                        || intake::within_sessions(&said.candidate, &queued)
                })
                .collect();
            let (kept, sifted) = intake::sift(said, prefilter);
            // A vessa that listed nothing because it failed has said so; no
            // count of nothing is shown for it.
            if transcripts_read_cleanly || sifted.read > 0 {
                eprintln!("ohjaa: intake: {}", sifted.said());
            }
            candidates.extend(kept);
        } else {
            candidates.extend(read(source, since, &mut warnings, &mut gaps)?);
        }
        if warnings.len() > before {
            warned.push(source);
        }
    }
    // A retry reads back to the oldest candidate set aside, for those alone:
    // the cursor may have passed them, and they are not a session's.
    if let Some(floor) = read_back {
        for source in &sources {
            let before = warnings.len();
            let back = read(source, floor, &mut warnings, &mut gaps)?;
            candidates.extend(back.into_iter().filter(|c| retry.contains(&c.key())));
            if warnings.len() > before && !warned.contains(source) {
                warned.push(source);
            }
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
    for warning in &warnings {
        eprintln!("ohjaa: intake: {warning}");
    }
    // Said on every run, once (a retry asks vessa twice); never a warning.
    gaps.sort();
    gaps.dedup();
    for gap in &gaps {
        eprintln!(
            "ohjaa: intake: {gap} (a store vessa does not read: nothing in it is drafted \
             from, and it does not hold the cursor)"
        );
    }
    let found = intake::found(candidates);
    // What earlier runs handled is never sent again: a run takes the oldest
    // candidates not handled yet, so every run makes progress.
    if retry_failed {
        let reached = found.iter().filter(|c| retry.contains(&c.key())).count();
        eprintln!(
            "ohjaa: {reached} of {} candidate(s) set aside are sent again{}",
            retry.len(),
            if reached < retry.len() {
                "; the others no source reads any more, and they stay set aside"
            } else {
                ""
            }
        );
        handled.retry_failed();
    }
    let left = found.iter().filter(|c| !handled.contains(c)).count();
    let batch = batch.max(1);
    let limit = batch.saturating_mul(max_batches);
    let taken = left.min(limit);
    eprintln!(
        "ohjaa: {} candidate(s) since {}, {} handled by earlier runs; drafting from {taken}",
        found.len(),
        intake::rfc3339(since.div_euclid(1000)),
        found.len() - left,
    );
    // What could not be read is never rendered as nothing new.
    let nothing = if warnings.is_empty() && gaps.is_empty() {
        "nothing new to draft from".to_owned()
    } else if warnings.is_empty() {
        format!(
            "nothing new to draft from in what vessa reads (it does not read {} store(s), \
             said above)",
            gaps.len()
        )
    } else {
        format!(
            "nothing found to draft from in what could be read ({} warning(s) above)",
            warnings.len()
        )
    };
    if dry_run {
        if left == 0 {
            println!("{nothing}");
            return Ok(if warnings.is_empty() { 0 } else { INCOMPLETE });
        }
        let loaded = knowledge::load(&dirs)?;
        let known = intake::Known::read(&dirs, &loaded)?;
        let drafter = drafter.map(str::to_owned).unwrap_or_else(default_drafter);
        let first: Vec<intake::Candidate> = found
            .iter()
            .filter(|c| !handled.contains(c))
            .take(batch)
            .cloned()
            .collect();
        let (prompt, redacted) = intake::prompt(&known, &first);
        println!("{prompt}");
        eprintln!(
            "ohjaa: dry-run; would send {} batch(es) to `{drafter}`; {redacted} \
             secret-looking token(s) redacted from the batch shown",
            taken.div_ceil(batch)
        );
        return Ok(0);
    }

    let today = knowledge::today();
    let (mut proposed, mut refused, mut redacted) = (0, 0, 0);
    // Proposals that may repeat what was present, by the id of what each may
    // repeat.
    let mut possible: std::collections::BTreeMap<String, usize> = Default::default();
    // Re-teachings recorded in this run, by node id; and those dropped.
    let mut recorded: std::collections::BTreeMap<String, usize> = Default::default();
    let mut dropped = intake::Dropped::default();
    let mut drafted = intake::Drafted::default();
    if left > 0 {
        let drafter = drafter.map(str::to_owned).unwrap_or_else(default_drafter);
        drafted = intake::draft_pending(
            &found,
            &mut handled,
            batch,
            limit,
            |chunk| {
                // Re-read each time, so a draft from an earlier batch counts
                // as present.
                let loaded = knowledge::load(&dirs).map_err(intake::Failure::Silent)?;
                let mut known =
                    intake::Known::read(&dirs, &loaded).map_err(intake::Failure::Silent)?;
                // Scrubbed of secrets, whatever the source or the filter.
                let (prompt, scrubbed, shown) = intake::prompt_with_evidence(&known, chunk);
                redacted += scrubbed;
                let answer = intake::draft(&drafter, &prompt)?;
                // Checked against what the drafter was shown, before the
                // drafts of this answer join it.
                let (retaught, dropped_here) = shown.retaught(&answer.reteaches, chunk);
                dropped.add(dropped_here);
                // The nodes the drafts written refine or supersede.
                let mut named: Vec<String> = Vec::new();
                for draft in answer.drafts {
                    let id = draft.id.clone();
                    let result = draft.into_proposal().and_then(|proposal| {
                        let near = known.repeated_by(&proposal).map(str::to_owned);
                        let present = known.present(&proposal);
                        let names: Vec<String> = proposal
                            .refines
                            .iter()
                            .chain(&proposal.supersedes)
                            .cloned()
                            .collect();
                        knowledge::propose(&dirs, &loaded, proposal, today)
                            .map(|_| (present, near, names))
                    });
                    match result {
                        Ok((present, near, names)) => {
                            named.extend(names);
                            // Present for the drafts after it in this answer.
                            known.add(present);
                            proposed += 1;
                            // Written, so never lost; said, and stored never:
                            // a person decides.
                            match near {
                                None => println!("proposed {id}"),
                                Some(of) => {
                                    println!("proposed {id}, a possible duplicate of {of}");
                                    *possible.entry(of).or_default() += 1;
                                }
                            }
                        }
                        Err(error) => {
                            refused += 1;
                            eprintln!("ohjaa: draft `{id}` refused: {error:#}");
                        }
                    }
                }
                // Kept before the batch is handled: a run killed between
                // the two sends it again, and a message counts once.
                let mut new = Vec::new();
                for r in &retaught {
                    if evidence.retaught(&r.node, r.key, r.session.as_deref()) {
                        new.push(r);
                    }
                }
                let mut changed = !new.is_empty();
                // A citation is as of the newest message of its batch.
                if let Some(newest) = chunk.iter().map(|c| c.at_ms).max() {
                    for node in shown.cited(&named, chunk) {
                        changed |= evidence.cited(&node, newest);
                    }
                }
                if changed {
                    evidence.write(&dirs).map_err(|error| {
                        intake::Failure::Silent(error.context("cannot keep the knowledge evidence"))
                    })?;
                }
                // Said by node, time and source, never by the words.
                for r in new {
                    println!("re-taught {} ({} {})", r.node.id, r.at, r.source);
                    *recorded.entry(r.node.id.clone()).or_default() += 1;
                }
                Ok(())
            },
            |handled| intake::write_handled(&dirs, handled),
        );
        // Said by time and source, never by its words.
        for (key, failing) in &drafted.failed {
            let at = intake::rfc3339(key.at_ms.div_euclid(1000));
            if failing.attempts >= intake::MAX_ATTEMPTS {
                eprintln!(
                    "ohjaa: intake: set aside {at} {}: the drafter failed on it alone {} times \
                     ({}); `ohjaa intake --retry-failed` sends it again",
                    failing.source, failing.attempts, failing.why
                );
            } else {
                eprintln!(
                    "ohjaa: intake: the drafter failed on {at} {} alone (attempt {} of {}: {}); \
                     it is tried again next run",
                    failing.source,
                    failing.attempts,
                    intake::MAX_ATTEMPTS,
                    failing.why
                );
            }
        }
        if let Some(error) = &drafted.stopped {
            eprintln!("ohjaa: intake stopped: {error:#}");
        }
        eprintln!(
            "ohjaa: intake: the drafter answered for {} candidate(s) in this run; {redacted} \
             secret-looking token(s) were redacted from what it was sent",
            drafted.sent
        );
        if dropped.total() > 0 {
            eprintln!(
                "ohjaa: intake: {} re-teaching(s) the drafter named were dropped: {}",
                dropped.total(),
                dropped.describe()
            );
        }
    }
    handled.done_retrying();
    // A session is drafted from once every candidate of it is handled, in
    // however many runs, and only when its whole window was read: a
    // transcript that could not be read leaves it queued for the next run.
    let sessions: Option<Vec<intake::Undrafted>> = undrafted.as_ref().map(|all| {
        all.iter()
            .map(|s| intake::Undrafted {
                id: s.id.clone(),
                started_ms: s.started_ms,
                members: (reads_transcripts && s.left.is_some())
                    .then(|| intake::members(s, &found)),
                whole: transcripts_read_cleanly,
            })
            .collect()
    });
    // The cursor is one for every source: it moves only after a run that
    // read them all, none with a warning.
    let every_source = ["transcripts", "reviews", "reverts", "report"]
        .iter()
        .all(|source| sources.contains(source));
    let window = intake::Window {
        since,
        started,
        complete: every_source && warnings.is_empty(),
    };
    let settled = intake::settle(&found, &mut handled, sessions.as_deref(), window);
    if let Some(cursor) = settled.cursor {
        intake::write_cursor(&dirs, cursor)?;
    }
    if !settled.drafted.is_empty() {
        // Opened again only for this short write, after the drafter is done.
        if let Some(store) = session_store(&dirs) {
            for id in &settled.drafted {
                if let Err(error) = store.update(id, |s| s.drafted = true) {
                    eprintln!("ohjaa: {error}");
                }
            }
        }
    }
    intake::write_handled(&dirs, &handled)?;
    // Every run says what is set aside, by time and source.
    let aside: Vec<String> = handled
        .set_aside()
        .map(|(key, failing)| {
            format!(
                "{} {}",
                intake::rfc3339(key.at_ms.div_euclid(1000)),
                failing.source
            )
        })
        .collect();
    if !aside.is_empty() {
        let more = aside.len().saturating_sub(5);
        eprintln!(
            "ohjaa: {} candidate(s) set aside, the drafter failed on each alone {} times: {}{} \
             (`ohjaa intake --retry-failed` sends them again)",
            aside.len(),
            intake::MAX_ATTEMPTS,
            aside[..aside.len() - more].join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
    }
    // A held cursor is said: where it stays, since when, and what holds it.
    if !window.complete {
        let mut why = Vec::new();
        if !every_source {
            why.push(format!(
                "this run read only {}, and the cursor is one for every source, so a run of \
                 some sources does not move it",
                sources.join(", ")
            ));
        }
        if !warned.is_empty() {
            why.push(format!(
                "{} could not all be read (see above); it moves after a run that reads every \
                 source",
                warned.join(", ")
            ));
        }
        match intake::read_cursor(&dirs)? {
            Some(at) => eprintln!(
                "ohjaa: intake: the cursor stays at {} ({}): {}",
                intake::rfc3339(at.div_euclid(1000)),
                scan::ago((started - at).div_euclid(1000)),
                why.join("; ")
            ),
            None => eprintln!("ohjaa: intake: no cursor is kept yet: {}", why.join("; ")),
        }
    }
    let newly_aside = drafted
        .failed
        .iter()
        .any(|(_, failing)| failing.attempts >= intake::MAX_ATTEMPTS);
    if left == 0 {
        println!("{nothing}");
    } else {
        let marked: usize = possible.values().sum();
        println!(
            "{proposed} proposal(s) waiting for review (`ohjaa review`), {marked} of them \
             possible duplicates, {refused} draft(s) refused"
        );
        if marked > 0 {
            let of: Vec<String> = possible
                .iter()
                .map(|(id, n)| format!("of {id} ×{n}"))
                .collect();
            println!(
                "possible duplicates: {} (`ohjaa review` shows each after what it may repeat)",
                of.join(", ")
            );
        }
        let retaught: usize = recorded.values().sum();
        if retaught > 0 {
            let by: Vec<String> = recorded
                .iter()
                .map(|(id, n)| format!("{id} ×{n}"))
                .collect();
            println!(
                "{retaught} re-teaching(s) of current knowledge recorded: {} (`ohjaa check` shows \
                 each node's count)",
                by.join(", ")
            );
        }
    }
    if settled.left > 0 {
        eprintln!("ohjaa: {} candidate(s) wait for the next run", settled.left);
    }
    let finished =
        settled.left == 0 && drafted.stopped.is_none() && !newly_aside && warnings.is_empty();
    Ok(if finished { 0 } else { INCOMPLETE })
}

// ---- launching --------------------------------------------------------------

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
        Cmd::Brief {
            knowledge_only,
            write,
            scope,
            agent,
        } => cmd_brief(&cli.root, *knowledge_only, write, scope, agent.as_deref()),
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
        Cmd::Lessons {
            action: LessonsCmd::Publish { repos, dry_run },
        } => cmd_lessons_publish(&cli.root, repos, *dry_run),
        Cmd::Usage { action } => cmd_usage(action),
        Cmd::Report { since, json, marks } => cmd_report(&cli.root, since, *json, *marks),
        Cmd::Line => {
            human::line(&cli.root);
            Ok(0)
        }
        Cmd::Digest { since, no_collect } => {
            human::digest(&cli.root, repos(&cli.root), since.as_deref(), *no_collect)
        }
        Cmd::Collect { background } => human::collect_now(report_repos(&cli.root), *background),
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
        Cmd::Intake {
            since,
            no_prefilter,
            sources,
            drafter,
            batch,
            max_batches,
            dry_run,
            retry_failed,
        } => cmd_intake(
            &cli.root,
            since.as_deref(),
            sources,
            drafter.as_deref(),
            *batch,
            *max_batches,
            *dry_run,
            *retry_failed,
            !*no_prefilter,
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
