//! CLI integration tests: the real binary against a stub `toimija` on PATH, in
//! a sandboxed HOME and XDG config/state, so no test ever reads or writes the
//! person's real knowledge.
//!
//! The stub differentiates per repo by cwd (`*broken*`) and per invocation by
//! environment, so one script covers the fleet and its refusals.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$OHJAA_STUB_CALLS"
case "$PWD" in
  *broken*) echo "boom: not a Git repository" >&2; exit 1;;
esac
case "$1" in
  sessions)
    case " $* " in
      *" --outside-repos "*)
        if [ -n "${OHJAA_STUB_NO_OUTSIDE:-}" ]; then
          echo "error: unexpected argument '--outside-repos' found" >&2
          echo "For more information, try '--help'." >&2
          exit 2
        fi
        cat "${OHJAA_STUB_OUTSIDE_SESSIONS:?}"
        exit 0;;
    esac
    if [ -n "${OHJAA_STUB_SESSIONS_EXIT:-}" ]; then
      echo "stub failure on demand" >&2
      exit "$OHJAA_STUB_SESSIONS_EXIT"
    fi
    cat "${OHJAA_STUB_SESSIONS:?}"
    exit 0;;
  context) cat "${OHJAA_STUB_CONTEXT:?}"; exit 0;;
esac
exit 0
"#;

/// A session payload exactly as `toimija sessions --json` emits it.
fn sessions_json(sessions: &str, coverage: &str) -> String {
    format!(
        "{{\"sessions\":[{sessions}],\"warnings\":[],\"coverage\":{{\"status\":\"{coverage}\"}}}}"
    )
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Write an executable that no process holds open for writing when it runs.
/// Linux refuses to exec a file any process has open for writing (ETXTBSY),
/// and a child another test thread forks holds a copy of every descriptor
/// this process has open until that child execs: a file written here, then
/// closed, even renamed, can still be open for writing in such a child at
/// the moment it is run. So this process writes only the text, beside it,
/// and `cp`, a process of its own that forks nothing, writes the executable.
fn write_executable(path: &Path, text: impl AsRef<str>) {
    let name = path.file_name().unwrap().to_string_lossy();
    let source = path.with_file_name(format!(".{name}.text"));
    fs::write(&source, text.as_ref()).unwrap();
    let copied = Command::new("cp").arg(&source).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    fs::remove_file(&source).unwrap();
    make_mode(path, 0o755);
}

fn make_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).unwrap();
}

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git {args:?} failed in {}",
        repo.display()
    );
}

fn output_of(command: &mut Command) -> (String, String, Option<i32>) {
    let output = command.output().unwrap();
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

struct Fixture {
    dir: PathBuf,
    stub_dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stub_dir = dir.join("bin");
        fs::create_dir_all(&stub_dir).unwrap();
        fs::create_dir_all(dir.join("home")).unwrap();
        let stub = stub_dir.join("toimija");
        write_executable(&stub, STUB);
        let fixture = Fixture { dir, stub_dir };
        // Default: an empty, fully available scan, in repos and outside them.
        fixture.set_sessions(&sessions_json("", "available"));
        fixture.set_outside_sessions(&sessions_json("", "available"));
        fixture
    }

    fn set_sessions(&self, json: &str) {
        fs::write(self.dir.join("sessions.json"), json).unwrap();
    }

    /// What `toimija sessions --outside-repos` reports.
    fn set_outside_sessions(&self, json: &str) {
        fs::write(self.dir.join("outside.json"), json).unwrap();
    }

    /// The real binary run from `folder`, where no repository can be found.
    fn ohjaa_in(&self, folder: &Path) -> Command {
        let mut command = self.ohjaa();
        command
            .current_dir(folder)
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        command
    }

    fn set_context(&self, json: &str) {
        fs::write(self.dir.join("context.json"), json).unwrap();
    }

    /// A directory toimija-prepared for fleet discovery.
    fn make_repo(&self, name: &str) -> PathBuf {
        let repo = self.dir.join(name);
        fs::create_dir_all(repo.join(".toimija")).unwrap();
        repo
    }

    /// A real Git repository (cmd_run requires one).
    fn make_git_repo(&self, name: &str) -> PathBuf {
        let repo = self.make_repo(name);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "ohjaa@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Ohjaa Test"]);
        run_git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        repo
    }

    /// The real binary with the stubbed PATH, a sandboxed HOME and XDG dirs,
    /// and a shared-knowledge checkout inside the fixture.
    fn ohjaa(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ohjaa"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        command.env(
            "PATH",
            std::env::join_paths(
                std::iter::once(self.stub_dir.clone()).chain(std::env::split_paths(&path)),
            )
            .unwrap(),
        );
        command
            .env("HOME", self.dir.join("home"))
            // The agents' own stores, read at leaving, are the sandbox HOME's.
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("PI_CODING_AGENT_DIR")
            .env_remove("OPENCODE_CONFIG_DIR")
            // Nor the ids of the agent running these tests, if one is.
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("PI_SESSION_ID")
            // Nor the modes in which it reads no instruction file of its own.
            .env_remove("CLAUDE_CODE_SIMPLE")
            .env_remove("CLAUDE_CODE_SAFE_MODE")
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("OHJAA_OURS", self.dir.join("ours"))
            .env("OHJAA_TODAY", "2026-09-24")
            .env("OHJAA_STUB_CALLS", self.dir.join("calls.txt"))
            .env("OHJAA_STUB_SESSIONS", self.dir.join("sessions.json"))
            .env("OHJAA_STUB_OUTSIDE_SESSIONS", self.dir.join("outside.json"))
            .env("OHJAA_STUB_CONTEXT", self.dir.join("context.json"));
        command
    }

    /// Write a current node straight into a scope directory, as an accepted
    /// and committed node would be.
    fn node(&self, dir: &Path, id: &str, kind: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{id}.md")),
            format!(
                "+++\nid = \"{id}\"\nkind = \"{kind}\"\nstatus = \"current\"\n\
                 as_of = 2026-09-20\norigin = [\"a test\"]\n+++\n{body}\n"
            ),
        )
        .unwrap();
    }

    /// A current node as of `as_of`, as [`Fixture::node`] writes one.
    fn node_as_of(&self, dir: &Path, id: &str, kind: &str, as_of: &str, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{id}.md")),
            format!(
                "+++\nid = \"{id}\"\nkind = \"{kind}\"\nstatus = \"current\"\n\
                 as_of = {as_of}\norigin = [\"a test\"]\n+++\n{body}\n"
            ),
        )
        .unwrap();
    }

    fn cleanup(self) {
        fs::remove_dir_all(self.dir).ok();
    }
}

/// What opens every knowledge section of a briefing.
const KNOWLEDGE_PREAMBLE: &str =
    "Knowledge (ohjaa, knowledge-node.v1): what the people you work for know and\n\
     have decided, as of the date on each entry. Follow it. If an entry looks\n\
     wrong or stale, say so instead of quietly working around it. Propose new\n\
     knowledge with `ohjaa propose`; only a person makes it current.\n";

/// The block `ohjaa apply` writes into an agent's own instruction file,
/// holding these lines.
fn applied_block(lines: &[&str]) -> String {
    format!(
        "<!-- ohjaa:knowledge begin: mine and machine, written by `ohjaa apply` and after each \
         decision, removed by `ohjaa apply --undo`; edit the nodes (`ohjaa list`), not this \
         block -->\nKnowledge (ohjaa): what your team and the person you work for have \
         decided and know. Follow it; if an entry looks wrong or stale, say so instead of \
         quietly working around it. Propose changes with `ohjaa propose`; only a person makes \
         them current.\n{}\n<!-- ohjaa:knowledge end -->\n",
        lines.join("\n")
    )
}

#[test]
fn an_agent_proposes_and_only_a_person_accepts() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("propose");
    let repo = fixture.make_git_repo("work");
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "propose",
                "--scope",
                "ours",
                "--id",
                "rust-only",
                "--kind",
                "decision",
                "--origin",
                "2026-09-16: 'we dont do python'",
                "Code",
                "is",
                "Rust.",
            ])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let waiting = fixture.dir.join("state/ohjaa/proposals/ours/rust-only.md");
    assert!(
        waiting.is_file(),
        "the proposal waits outside every repository"
    );
    assert!(fs::read_to_string(&waiting)
        .unwrap()
        .contains("status = \"proposed\""));

    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("No knowledge recorded yet"),
        "a proposal is not knowledge: {stdout}"
    );

    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["review", "--list"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("rust-only") && stdout.contains("we dont do python"),
        "{stdout}"
    );

    // Tests run without a terminal, exactly like an agent.
    for decision in [
        vec!["accept", "rust-only"],
        vec!["reject", "rust-only"],
        vec!["review"],
    ] {
        let (_, stderr, code) = output_of(fixture.ohjaa().args(&decision).current_dir(&repo));
        assert_eq!(code, Some(3), "{decision:?}: {stderr}");
        assert!(stderr.contains("only a person may"), "{stderr}");
    }
    assert!(waiting.is_file(), "a refused decision changes nothing");
    assert!(!fixture.dir.join("ours/knowledge/rust-only.md").exists());
    fixture.cleanup();
}

#[test]
fn a_proposal_that_would_not_hold_is_refused_and_written_nowhere() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("refuse");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "team-rule",
        "decision",
        "Shared.",
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "propose",
                "--scope",
                "mine",
                "--id",
                "my-override",
                "--kind",
                "decision",
                "--origin",
                "why",
                "--supersedes",
                "team-rule",
                "Mine instead.",
            ])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert!(stderr.contains("cross-scope-supersedes"), "{stderr}");
    assert!(!fixture
        .dir
        .join("state/ohjaa/proposals/mine/my-override.md")
        .exists());
    fixture.cleanup();
}

#[test]
fn the_briefing_orders_knowledge_by_scope_then_kind() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("order");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("config/ohjaa/machine"),
        "lima",
        "fact",
        "Lima is here.",
    );
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "answer-first",
        "taste",
        "Answer first.",
    );
    fixture.node(
        &repo.join(".ohjaa/knowledge"),
        "uploader",
        "fact",
        "Sam owns the uploader.",
    );
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "plain",
        "taste",
        "Plain English.",
    );
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "rust",
        "decision",
        "Rust only.",
    );
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let order: Vec<usize> = [
        "Rust only.",
        "Plain English.",
        "Sam owns",
        "Answer first.",
        "Lima is here.",
    ]
    .iter()
    .map(|text| {
        stdout
            .find(text)
            .unwrap_or_else(|| panic!("{text} missing: {stdout}"))
    })
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{stdout}");
    assert!(stdout.contains("(rust · as of 2026-09-20)"), "{stdout}");
    fixture.cleanup();
}

/// Over its budget the briefing degrades: every decision in full, then
/// taste, lesson and fact, each newest first, while they fit with the count
/// of the rest, then that count. One node over: only the oldest fact goes.
#[test]
fn the_briefing_keeps_every_decision_then_fills_its_budget_by_priority() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("briefing-budget");
    let repo = fixture.make_git_repo("work");
    let (ours, mine, machine) = (
        fixture.dir.join("ours/knowledge"),
        fixture.dir.join("config/ohjaa/mine"),
        fixture.dir.join("config/ohjaa/machine"),
    );
    let long = |sentence: &str, times: usize| sentence.repeat(times).trim_end().to_owned();
    let (small, answer, menu, vm, lima) = (
        long("Keep it small. ", 115),
        long("Answer first. ", 125),
        long("A menu is stopping. ", 86),
        long("The VM empties its tmp. ", 72),
        long("Lima is here. ", 125),
    );
    fixture.node(&ours, "team", "decision", "Team rule.");
    fixture.node(&mine, "rust", "decision", "Rust only.");
    fixture.node_as_of(&mine, "small", "taste", "2026-09-20", &small);
    fixture.node_as_of(&mine, "answer", "taste", "2026-09-10", &answer);
    fixture.node_as_of(&mine, "menu", "lesson", "2026-09-20", &menu);
    fixture.node_as_of(&machine, "vm", "fact", "2026-09-20", &vm);
    fixture.node_as_of(&machine, "lima", "fact", "2026-09-10", &lima);
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "{KNOWLEDGE_PREAMBLE}\nOurs: decided by the team, binding everywhere:\n\
             - [decision] Team rule.\n  (team · as of 2026-09-20)\n\
             \nMine: how the person you work for likes to work:\n\
             - [decision] Rust only.\n  (rust · as of 2026-09-20)\n\
             - [taste] {answer}\n  (answer · as of 2026-09-10)\n\
             - [taste] {small}\n  (small · as of 2026-09-20)\n\
             - [lesson] {menu}\n  (menu · as of 2026-09-20)\n\
             \nThis machine:\n- [fact] {vm}\n  (vm · as of 2026-09-20)\n\
             \n1 more: `ohjaa list`\n"
        )
    );
    assert!(
        stderr.contains(
            "the briefing holds every decision and 4 more, and names the other 1 by count; \
             merge or retire some (`ohjaa check`)"
        ),
        "{stderr}"
    );
    fixture.cleanup();
}

/// The agent's own instruction file holds mine and machine, current, byte
/// for byte the block apply would write now: the briefing leaves them out
/// and says where they are. Otherwise it carries them and says why, each in
/// one line, and a flag on a node it left out is still said.
#[test]
fn the_briefing_leaves_out_what_the_agents_own_file_holds_current() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("own-file");
    let repo = fixture.make_git_repo("work");
    let (ours, mine, machine) = (
        fixture.dir.join("ours/knowledge"),
        fixture.dir.join("config/ohjaa/mine"),
        fixture.dir.join("config/ohjaa/machine"),
    );
    fixture.node(&ours, "team", "decision", "Team rule.");
    fixture.node(&mine, "rust", "decision", "Rust only.");
    fixture.node(&machine, "lima", "fact", "Lima is here.");
    let file = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let current = applied_block(&[
        "- decision: Team rule.",
        "- decision: Rust only.",
        "- fact: Lima is here.",
    ]);
    fs::write(&file, format!("# My notes\n\n{current}")).unwrap();
    let brief = |agent: &str| {
        let (stdout, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["brief", "--knowledge-only", "--agent", agent])
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        stdout
    };
    let ours_part = "\nOurs: decided by the team, binding everywhere:\n\
                     - [decision] Team rule.\n  (team · as of 2026-09-20)\n";
    let file_shown = file.display().to_string();
    assert_eq!(
        brief("claude"),
        format!(
            "The team's, mine and machine (3 nodes) are not repeated here: \
             Claude Code has them from its own instructions, {file_shown}, whose ohjaa block is \
             current.\n"
        )
    );
    // The agent as a path: its file name says which it is.
    assert_eq!(brief("/opt/bin/claude"), brief("claude"));
    // A flag the block cannot carry: that node is repeated, with its flag.
    fs::write(
        machine.join("lima.md"),
        "+++\nid = \"lima\"\nkind = \"fact\"\nstatus = \"current\"\nas_of = 2026-09-20\n\
         origin = [\"a test\"]\nholds_while = [\"command:no-such-command-ohjaa-test\"]\n+++\n\
         Lima is here.\n",
    )
    .unwrap();
    assert_eq!(
        brief("claude"),
        format!(
            "{KNOWLEDGE_PREAMBLE}\nThis machine:\n- [fact] Lima is here.\n  \
             (lima · as of 2026-09-20)\n  ! may no longer hold: \
             command:no-such-command-ohjaa-test is false\n\nThe team's, mine and machine (3 nodes) are not \
             repeated here: Claude Code has them from its own instructions, {file_shown}, whose \
             ohjaa block is current. Flagged, so repeated above: lima.\n"
        )
    );
    fixture.node(&machine, "lima", "fact", "Lima is here.");
    // No agent ohjaa writes the own file of: the briefing as it always was,
    // and nothing said.
    let full = format!(
        "{KNOWLEDGE_PREAMBLE}{ours_part}\nMine: how the person you work for likes to work:\n\
         - [decision] Rust only.\n  (rust · as of 2026-09-20)\n\nThis machine:\n\
         - [fact] Lima is here.\n  (lima · as of 2026-09-20)\n"
    );
    assert_eq!(brief("sh"), full);
    let (unnamed, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only"])
            .current_dir(&repo),
    );
    assert_eq!((unnamed, code), (full.clone(), Some(0)), "{stderr}");
    // Carried in full, and why.
    let carried = |agent: &str, why: &str| {
        format!(
            "{full}\nThe team's, mine and machine are carried here, not left to {agent}'s own \
             instructions: {why}.\n"
        )
    };
    // Stale: what the block holds is not what apply would write now.
    fs::write(
        &file,
        format!(
            "# My notes\n\n{}",
            applied_block(&["- decision: Rust only."])
        ),
    )
    .unwrap();
    assert_eq!(
        brief("claude"),
        carried(
            "Claude Code",
            &format!(
                "the ohjaa block in {file_shown} is not the one `ohjaa apply` would write now"
            )
        )
    );
    // No block.
    fs::write(&file, "# My notes\n").unwrap();
    assert_eq!(
        brief("claude"),
        carried(
            "Claude Code",
            &format!("{file_shown} holds no ohjaa block (`ohjaa apply` writes it)")
        )
    );
    // A block apply did not write.
    fs::write(
        &file,
        "# My notes\n\n<!-- ohjaa:knowledge begin: generated by `ohjaa brief --write`; edit the \
         nodes, not this block -->\nRust only.\n<!-- ohjaa:knowledge end -->\n",
    )
    .unwrap();
    assert_eq!(
        brief("claude"),
        carried(
            "Claude Code",
            &format!(
                "the ohjaa block in {file_shown} is not apply's: `ohjaa brief --write` or the \
                 person wrote it"
            )
        )
    );
    // Two blocks: not one block ohjaa can read.
    fs::write(&file, format!("{current}\n{current}")).unwrap();
    assert_eq!(
        brief("claude"),
        carried(
            "Claude Code",
            &format!(
                "{file_shown} cannot be read as one ohjaa block (the file has an ohjaa knowledge \
                 marker without its partner, or more than one block; fix it by hand)"
            )
        )
    );
    // A file that cannot be read at all (unless the tests run as root).
    fs::write(&file, &current).unwrap();
    make_mode(&file, 0o000);
    if fs::read(&file).is_err() {
        assert_eq!(
            brief("claude"),
            carried(
                "Claude Code",
                &format!("{file_shown} cannot be read (Permission denied (os error 13))")
            )
        );
    }
    make_mode(&file, 0o644);
    // No file.
    fs::remove_file(&file).unwrap();
    assert_eq!(
        brief("claude"),
        carried("Claude Code", &format!("{file_shown} does not exist"))
    );
    let codex = fixture.dir.join("home/.codex/AGENTS.md");
    assert_eq!(
        brief("codex"),
        carried("Codex", &format!("{} does not exist", codex.display()))
    );
    // Codex reads its override first, where there is one.
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, &current).unwrap();
    let only_the_line = |file: &Path| {
        format!(
            "The team's, mine and machine (3 nodes) are not repeated here: Codex has them from \
             its own instructions, {}, whose ohjaa block is current.\n",
            file.display()
        )
    };
    assert_eq!(brief("codex"), only_the_line(&codex));
    let wins = codex.with_file_name("AGENTS.override.md");
    fs::write(&wins, "# Override\n").unwrap();
    assert_eq!(
        brief("codex"),
        carried(
            "Codex",
            &format!(
                "{} holds no ohjaa block (`ohjaa apply` writes it)",
                wins.display()
            )
        )
    );
    fixture.cleanup();
}

/// The agent's block and the briefing both cut: the line says how many of
/// the nodes the file does not hold the briefing names only by count.
#[test]
fn the_line_says_how_many_the_briefing_could_only_count() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("both-cut");
    let repo = fixture.make_git_repo("work");
    let (mine, machine) = (
        fixture.dir.join("config/ohjaa/mine"),
        fixture.dir.join("config/ohjaa/machine"),
    );
    fixture.node(&mine, "rust", "decision", "Rust only.");
    let mut lines = vec!["- decision: Rust only.".to_owned()];
    for n in 0..40 {
        let body = format!("Taste {n:02}: {}", "keep it short ".repeat(12))
            .trim_end()
            .to_owned();
        fixture.node(&mine, &format!("t{n:02}"), "taste", &body);
        lines.push(format!("- taste: {body}"));
    }
    let (lima, vm) = (
        "Lima is here. ".repeat(25).trim_end().to_owned(),
        "The VM is slim. ".repeat(22).trim_end().to_owned(),
    );
    fixture.node_as_of(&machine, "lima", "fact", "2026-09-10", &lima);
    fixture.node_as_of(&machine, "vm", "fact", "2026-09-12", &vm);
    // The block: every decision and taste, both facts by count.
    lines.push("2 more: `ohjaa list`".to_owned());
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, applied_block(&lines)).unwrap();
    let line = format!(
        "The team's, mine and machine: Claude Code has 41 of their 43 nodes from its own instructions, {}, \
         whose ohjaa block is current; the other 2 are above, 1 of them only by count.",
        file.display()
    );
    // Sized so that the briefing holds the repository's taste (no agent's
    // own file holds a repository's knowledge) and the newer fact, and the
    // older only by count.
    let team = "y".repeat(7000 - line.len());
    fixture.node(&repo.join(".ohjaa/knowledge"), "team", "taste", &team);
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only", "--agent", "claude"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "{KNOWLEDGE_PREAMBLE}\nThis repository:\n\
             - [taste] {team}\n  (team · as of 2026-09-20 · not yet in AGENTS.md)\n\n\
             This machine:\n- [fact] {vm}\n  \
             (vm · as of 2026-09-12)\n\n1 more: `ohjaa list`\n\n{line}\n"
        )
    );
    fixture.cleanup();
}

/// The one line about mine and machine is part of the knowledge section,
/// and so inside its budget: it takes the room of the last node to fit.
#[test]
fn the_line_about_mine_and_machine_is_inside_the_budget() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-budget");
    let repo = fixture.make_git_repo("work");
    let (mine, machine) = (
        fixture.dir.join("config/ohjaa/mine"),
        fixture.dir.join("config/ohjaa/machine"),
    );
    let pi = fixture.dir.join("home/.pi/agent/AGENTS.md");
    let line = format!(
        "The team's, mine and machine are carried here, not left to pi's own instructions: {} does not \
         exist.",
        pi.display()
    );
    // Sized so that the nodes fit the 8,000 characters alone, and with the
    // line are 10 over.
    let pad = "x".repeat(7513 - line.len());
    fixture.node(&mine, "rust", "decision", "Rust only.");
    fixture.node(&mine, "pad", "taste", &pad);
    fixture.node_as_of(&machine, "lima", "fact", "2026-09-10", "Lima is here.");
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only", "--agent", "pi"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "{KNOWLEDGE_PREAMBLE}\nMine: how the person you work for likes to work:\n\
             - [decision] Rust only.\n  (rust · as of 2026-09-20)\n- [taste] {pad}\n  \
             (pad · as of 2026-09-20)\n\n1 more: `ohjaa list`\n\n{line}\n"
        )
    );
    assert_eq!(stdout.len(), 8000 - 35);
    fixture.cleanup();
}

#[test]
fn a_scoped_repositorys_knowledge_alone_still_opens_with_the_preamble() {
    let fixture = Fixture::new("preamble-alone");
    let repo = fixture.make_git_repo("selected");
    fixture.set_context(r#"{"repo":{"name":"selected","role":"selected role","not_roles":[]}}"#);
    // No ours, mine or machine knowledge at all: only the scoped repository's.
    fixture.node(
        &repo.join(".ohjaa/knowledge"),
        "repo-rule",
        "decision",
        "Selected repository rule.",
    );
    let (brief, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "brief",
        "--scope",
        repo.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    let preamble = brief
        .find("Knowledge (ohjaa, knowledge-node.v1)")
        .expect(&brief);
    assert_eq!(
        brief
            .matches("Knowledge (ohjaa, knowledge-node.v1)")
            .count(),
        1,
        "{brief}"
    );
    assert!(
        preamble < brief.find("Selected repository rule.").unwrap(),
        "said once, before the first knowledge: {brief}"
    );
    fixture.cleanup();
}

#[test]
fn check_reports_broken_nodes_and_flags() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("check");
    let repo = fixture.make_git_repo("work");
    let mine = fixture.dir.join("config/ohjaa/mine");
    fixture.node(&mine, "fine", "taste", "Fine.");
    fs::write(mine.join("broken.md"), "no frontmatter\n").unwrap();
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert_eq!(code, Some(1), "{stdout}");
    assert!(
        stdout.contains("broken.md") && stdout.contains("missing-frontmatter"),
        "{stdout}"
    );

    fs::remove_file(mine.join("broken.md")).unwrap();
    let machine = fixture.dir.join("config/ohjaa/machine");
    fs::create_dir_all(&machine).unwrap();
    fs::write(
        machine.join("old-tool.md"),
        "+++\nid = \"old-tool\"\nkind = \"fact\"\nstatus = \"current\"\nas_of = 2026-01-01\n\
         origin = [\"t\"]\nholds_while = [\"command:no-such-command-ohjaa-test\"]\n+++\nA tool.\n",
    )
    .unwrap();
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert_eq!(code, Some(0), "flags alone are not errors: {stdout}");
    assert!(
        stdout.contains("command:no-such-command-ohjaa-test is false"),
        "{stdout}"
    );
    assert!(
        stdout.contains("fact last confirmed 266 days ago"),
        "{stdout}"
    );
    let (_, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["check", "--strict"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(1));
    fixture.cleanup();
}

#[test]
fn brief_write_replaces_only_its_own_block() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("write");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "rust",
        "decision",
        "Rust only.",
    );
    let target = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "# Mine\n\nKeep this line.\n").unwrap();
    for _ in 0..2 {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["brief", "--write"])
                .arg(&target)
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
    }
    let written = fs::read_to_string(&target).unwrap();
    assert!(
        written.starts_with("# Mine\n\nKeep this line.\n"),
        "{written}"
    );
    assert_eq!(
        written.matches("ohjaa:knowledge begin").count(),
        1,
        "{written}"
    );
    assert!(written.contains("Rust only."), "{written}");
    fixture.cleanup();
}

/// A proposal waiting for a person, written as `ohjaa propose` writes it.
fn waiting_proposal(dir: &Path, id: &str, body: &str) -> (PathBuf, String) {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{id}.md"));
    let text = format!(
        "+++\nid = \"{id}\"\nkind = \"lesson\"\nstatus = \"proposed\"\nas_of = 2026-09-20\n\
         origin = [\"a test\"]\n+++\n{body}\n"
    );
    fs::write(&path, &text).unwrap();
    (path, text)
}

/// Agents propose one lesson several times over. Refusing what looks
/// repeated would drop real lessons for good, and a person is the only one
/// who decides: so each waits, and review shows a possible duplicate right
/// after what it may repeat, saying so. Nothing of that is stored.
#[test]
fn review_shows_a_possible_duplicate_after_what_it_may_repeat() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("review-duplicates");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust-elixir-only",
        "decision",
        "Code is Rust or Elixir.",
    );
    let proposals = fixture.dir.join("state/ohjaa/proposals");
    waiting_proposal(
        &proposals.join("ours"),
        "start-sessions-through-ohjaa-run",
        "Start every agent session through `ohjaa run`.",
    );
    for (scope, id, refines) in [
        ("ours", "start-agents-through-ohjaa-run", None),
        ("mine", "code-is-rust-or-elixir", None),
        (
            "ours",
            "pi-sessions-need-a-wrapper-too",
            Some("start-sessions-through-ohjaa-run"),
        ),
    ] {
        let mut propose = fixture.ohjaa();
        propose
            .args(["propose", "--scope", scope, "--id", id, "--kind", "lesson"])
            .args(["--origin", "a test", "The lesson."])
            .current_dir(&repo);
        if let Some(base) = refines {
            propose.args(["--refines", base]);
        }
        let (stdout, stderr, code) = output_of(&mut propose);
        assert_eq!(code, Some(0), "{id}: {stdout}{stderr}");
    }
    let (listed, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["review", "--list"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let at = |text: &str| {
        listed
            .find(text)
            .unwrap_or_else(|| panic!("{text}\n{listed}"))
    };
    assert!(
        at("\nstart-sessions-through-ohjaa-run  [") < at("\nstart-agents-through-ohjaa-run  [")
            && at("\nstart-agents-through-ohjaa-run  [")
                < at("possible duplicate of: start-sessions-through-ohjaa-run (waiting too)"),
        "{listed}"
    );
    assert!(
        listed
            .contains("possible duplicate of: rust-elixir-only (current: Code is Rust or Elixir.)"),
        "{listed}"
    );
    assert!(
        listed.contains(
            "refines: start-sessions-through-ohjaa-run (waiting too); it can be accepted once \
             that one is"
        ),
        "{listed}"
    );
    assert!(listed.contains("4 waiting"), "{listed}");
    fixture.cleanup();
}

#[test]
fn review_does_not_claim_an_empty_queue_when_a_proposal_is_unreadable() {
    let fixture = Fixture::new("review-unreadable");
    let proposals = fixture.dir.join("state/ohjaa/proposals/mine");
    fs::create_dir_all(&proposals).unwrap();
    fs::write(proposals.join("broken.md"), "not a knowledge node\n").unwrap();
    let (stdout, stderr, code) = output_of(fixture.ohjaa().args(["review", "--list"]));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert!(stderr.contains("unreadable proposal"), "{stderr}");
    assert!(
        stdout.contains("No readable proposals; 1 could not be read"),
        "{stdout}"
    );
    assert!(!stdout.contains("nothing is waiting"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn a_file_inside_a_repository_gets_only_shared_knowledge() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sharedwrite");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "rust",
        "decision",
        "Rust only.",
    );
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "answer-first",
        "taste",
        "Answer first.",
    );
    fixture.node(
        &fixture.dir.join("config/ohjaa/machine"),
        "lima",
        "fact",
        "Lima is here.",
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--write", "AGENTS.md"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("shared (ours, repo)"), "{stderr}");
    let written = fs::read_to_string(repo.join("AGENTS.md")).unwrap();
    assert!(written.contains("Rust only."), "{written}");
    assert!(
        !written.contains("Answer first.") && !written.contains("Lima is here."),
        "one person's knowledge never lands in a shared file: {written}"
    );
    fixture.cleanup();
}

#[test]
fn check_counts_an_unreadable_proposal_as_an_error() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("badproposal");
    let repo = fixture.make_git_repo("work");
    let waiting = fixture.dir.join("state/ohjaa/proposals/mine");
    fs::create_dir_all(&waiting).unwrap();
    fs::write(waiting.join("half.md"), "+++\nid = \"half\"\n").unwrap();
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert_eq!(code, Some(1), "{stdout}");
    assert!(
        stdout.contains("proposal") && stdout.contains("half.md"),
        "{stdout}"
    );
    fixture.cleanup();
}

/// A rule that tells an agent to use an `ohjaa` command this ohjaa does not
/// have asks for what no agent can do: check flags it.
#[test]
fn check_flags_a_current_node_that_names_an_ohjaa_command_this_ohjaa_lacks() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("check-own-commands");
    let repo = fixture.make_git_repo("work");
    let mine = fixture.dir.join("config/ohjaa/mine");
    fixture.node(
        &mine,
        "launch",
        "decision",
        "Start through `ohjaa no-such-command --scope x`. Propose with `ohjaa propose`.",
    );
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert_eq!(code, Some(0), "a flag is not an error: {stdout}");
    assert!(
        stdout.contains(
            "flag     launch: names `ohjaa no-such-command`, which this ohjaa does not have"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("`ohjaa propose`"), "{stdout}");
    let (_, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["check", "--strict"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(1));
    fixture.cleanup();
}
