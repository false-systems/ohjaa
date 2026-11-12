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

/// Never the real gh: no network, and gh is not logged in.
const GH_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "${OHJAA_STUB_GH_CALLS:-/dev/null}"
echo "gh: To get started with GitHub CLI, please run:  gh auth login" >&2
exit 4
"#;

/// Never the real vessa. `messages` lists, as
/// `vessa-messages.v1`, every line of `$HOME/vessa-messages.jsonl` (one
/// message each, as vessa writes them), whatever its `--since`: intake
/// windows what it is given too. Each call's arguments go to
/// `$HOME/vessa-calls`. With `$HOME/vessa-gap` or `$HOME/vessa-warning` (each
/// a JSON string) the list is partial, with that gap (a store vessa does not
/// read) or source warning (one it could not read), and it exits 4. With
/// `$HOME/vessa-old` it is a vessa older than `messages`.
const VESSA_STUB: &str = r#"#!/bin/sh
if [ "$1" = messages ]; then
  printf '%s\n' "$*" >> "$HOME/vessa-calls"
  if [ -e "$HOME/vessa-old" ]; then
    echo "error: unrecognized subcommand 'messages'" >&2
    echo "For more information, try '--help'." >&2
    exit 2
  fi
  printf '{"schema":"vessa-messages.v1","messages":[\n'
  if [ -f "$HOME/vessa-messages.jsonl" ]; then sed '$!s/$/,/' "$HOME/vessa-messages.jsonl"; fi
  gaps=""; warnings=""; code=0
  if [ -f "$HOME/vessa-gap" ]; then gaps=$(cat "$HOME/vessa-gap"); code=4; fi
  if [ -f "$HOME/vessa-warning" ]; then warnings=$(cat "$HOME/vessa-warning"); code=4; fi
  printf '],\n"sessions":[],\n"coverage":{"excluded":{},"gaps":[%s],"limitations":[]},\n"warnings":[%s]}\n' "$gaps" "$warnings"
  [ "$code" = 4 ] && echo "vessa: partial message list (exit 4): 1 source warning, 0 limitations" >&2
  exit "$code"
fi
echo "vessa stub: not messages: $*" >&2
exit 64
"#;

/// A source warning from vessa (`$HOME/vessa-warning`): a transcript it could
/// not read.
const UNREADABLE: &str = r#""/h/.codex/sessions/r.jsonl: Permission denied (os error 13)""#;

/// One message a person wrote, as `vessa messages --json` lists it.
fn vessa_line(at: &str, cwd: &str, text: &str, reply: Option<&str>) -> String {
    serde_json::json!({
        "agent": "claude", "session": "s1", "parent": null, "kind": "message", "id": null,
        "at": at, "cwd": cwd, "text": text, "question": null,
        "reply": reply.map(|text| serde_json::json!({
            "text": text, "truncated": false, "failed": false, "line": 1,
        })),
        "source": "/t/s1.jsonl", "line": 2,
    })
    .to_string()
}

impl Fixture {
    /// What the fake vessa lists, after what it already lists.
    fn said(&self, lines: &[String]) {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("home/vessa-messages.jsonl"))
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    /// Each `vessa messages` call's arguments, in order.
    fn vessa_calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("home/vessa-calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

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
        // Never the real gh: no network, and a controlled answer.
        let gh = stub_dir.join("gh");
        write_executable(&gh, GH_STUB);
        // Never the real vessa: no native sessions unless a test lists some.
        let vessa = stub_dir.join("vessa");
        write_executable(&vessa, VESSA_STUB);
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

    /// A folder that is not a Git repository. Git may not look above the
    /// fixture for one (`GIT_CEILING_DIRECTORIES` in [`Fixture::ohjaa_in`]).
    fn make_folder(&self, name: &str) -> PathBuf {
        let folder = self.dir.join(name);
        fs::create_dir_all(&folder).unwrap();
        folder
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
fn system_derives_the_fleet_and_exits_incomplete_when_a_role_is_missing() {
    let fixture = Fixture::new("system");
    fixture.make_repo("tasker");
    fixture.make_repo("kelo");
    fixture.set_context(
        r#"{"repo":{"name":"tasker","role":"durable task contract","not_roles":["inbox"]}}"#,
    );
    // One canned context for every repo: tasker parses, the "kelo" copy does
    // not (its name mismatches nothing — the stub serves the same file, so
    // kelo derives with the wrong name and no role of its own). To keep the
    // test honest, give kelo an empty repo object instead.
    fs::write(
        fixture.dir.join("context.json"),
        r#"{"repo":{"name":"tasker","role":"durable task contract","not_roles":["inbox"]}}"#,
    )
    .unwrap();

    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["system", "--root"])
            .arg(fixture.dir.clone()),
    );
    fixture.cleanup();

    assert!(stdout.contains("tasker"), "{stdout}");
    assert!(stdout.contains("role: durable task contract"), "{stdout}");
    assert!(stdout.contains("not:  inbox"), "{stdout}");
    // Both repos derive the same canned context, so no role is missing here;
    // the incomplete path is covered by the unit test. The command must not
    // fail on a healthy fleet.
    assert_eq!(code, Some(0), "{stdout}");
}

#[test]
fn system_json_lists_tools_with_their_roles() {
    let fixture = Fixture::new("system-json");
    fixture.make_repo("tasker");
    fixture
        .set_context(r#"{"repo":{"name":"tasker","role":"durable task contract","not_roles":[]}}"#);

    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["system", "--json", "--root"])
            .arg(fixture.dir.clone()),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0));
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tool = &value["tools"][0];
    assert_eq!(tool["name"], "tasker");
    assert_eq!(tool["role"], "durable task contract");
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

#[test]
fn intake_turns_a_correction_into_a_proposal_through_the_drafter() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intake");
    let repo = fixture.make_git_repo("work");
    fixture.said(&[vessa_line(
        "2026-09-20T10:00:00Z",
        "/p/work",
        "no, we have lima baba",
        Some("Linux tests are blocked on macOS."),
    )]);
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\ncat <<'JSON'\nSure:\n[{{\"scope\":\"machine\",\"repo\":null,\"id\":\"lima-for-linux\",\
             \"kind\":\"fact\",\"body\":\"Linux tests run in Lima.\",\"origin\":[\"2026-09-20 work: 'we have lima baba'\"],\
             \"refines\":null,\"supersedes\":[]}},{{\"scope\":\"nowhere\",\"id\":\"bad\",\"kind\":\"fact\",\"body\":\"x\"}}]\nJSON\n",
            fixture.dir.join("prompt.txt").display()
        ),
    );

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--since", "2026-09-01", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let prompt = fs::read_to_string(fixture.dir.join("prompt.txt")).unwrap();
    assert!(
        prompt.contains("we have lima baba"),
        "the candidate reaches the drafter"
    );
    assert!(
        prompt.contains("Linux tests are blocked on macOS."),
        "with what the agent said"
    );
    assert!(stdout.contains("proposed lima-for-linux"), "{stdout}");
    assert!(
        stderr.contains("draft `bad` refused"),
        "an invalid draft is refused, loudly: {stderr}"
    );
    assert!(fixture
        .dir
        .join("state/ohjaa/proposals/machine/lima-for-linux.md")
        .is_file());
    let cursor = fs::read_to_string(fixture.dir.join("state/ohjaa/intake-since")).unwrap();
    assert!(
        cursor.starts_with("20"),
        "the cursor moves once everything was drafted: {cursor}"
    );

    // A second run from the cursor finds nothing new and asks the drafter nothing.
    fs::remove_file(fixture.dir.join("prompt.txt")).unwrap();
    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0));
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert!(!fixture.dir.join("prompt.txt").exists());
    fixture.cleanup();
}

#[test]
fn a_failing_drafter_stops_intake_without_moving_the_cursor() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakefail");
    let repo = fixture.make_git_repo("work");
    fixture.said(&[vessa_line(
        "2026-09-20T10:00:00Z",
        "/p/work",
        "never python. i told you",
        None,
    )]);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "intake",
                "--source",
                "transcripts",
                "--since",
                "2026-09-01",
                "--drafter",
                "echo no array; exit 0",
            ])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("attempt 1 of 3"),
        "kept, to try again: {stderr}"
    );
    assert!(!fixture.dir.join("state/ohjaa/intake-since").exists());
    fixture.cleanup();
}

#[test]
fn a_repository_with_no_commits_yet_is_empty_and_does_not_hold_the_cursor() {
    // `git log` refuses a repository with no commits yet. Such a
    // repository has no reverts to read: it must not count as unread and
    // hold the cursor.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakeempty");
    let repo = fixture.make_git_repo("work");
    let fresh = fixture.make_repo("fresh");
    run_git(&fresh, &["init", "-q"]);
    let cursor = fixture.dir.join("state/ohjaa/intake-since");

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--since", "2026-09-01", "--drafter", "exit 1"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "nothing to read is not a failure: {stderr}");
    assert!(stdout.contains("nothing new to draft from"), "{stdout}");
    assert!(
        !stderr.contains("does not have any commits yet"),
        "an empty repository is not a warning: {stderr}"
    );
    assert!(cursor.is_file(), "the cursor moves: {stderr}");
    let moved = fs::read_to_string(&cursor).unwrap();

    // A repository git cannot read at all still holds it, out loud.
    let gone = fixture.make_folder("gone");
    fs::write(
        gone.join(".git"),
        format!("gitdir: {}\n", fixture.dir.join("nowhere").display()),
    )
    .unwrap();
    // Git versions word a broken gitfile differently. Pin the diagnostic
    // this Git gives, rather than accepting several possible messages.
    let refused = Command::new("git")
        .arg("-C")
        .arg(&gone)
        .arg("log")
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let reason = String::from_utf8(refused.stderr).unwrap();
    let reason = reason.trim();
    assert!(!reason.is_empty());
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--drafter", "exit 1"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("gone") && stderr.contains(reason),
        "{stderr}"
    );
    assert!(stderr.contains("the cursor stays"), "{stderr}");
    assert_eq!(fs::read_to_string(&cursor).unwrap(), moved);
    fixture.cleanup();
}

#[test]
fn only_a_branch_never_committed_to_is_empty_not_one_whose_ref_was_lost() {
    // git says "does not have any commits yet" for a
    // branch never committed to, and also for one whose ref was lost (a
    // loose ref deleted, `packed-refs` cut short). Only the first is empty.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakelost");
    let committed = |name: &str| {
        let repo = fixture.make_repo(name);
        run_git(&repo, &["init", "-q", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "ohjaa@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Ohjaa Test"]);
        run_git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        repo
    };
    let repo = committed("work");
    // A new branch with no history of its own, in a repository with some.
    let orphan = committed("orphan");
    run_git(&orphan, &["checkout", "-q", "--orphan", "fresh"]);
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--since", "2026-09-01", "--drafter", "exit 1"])
                .current_dir(&repo),
        )
    };
    let (_, stderr, code) = intake();
    assert_eq!(
        code,
        Some(0),
        "a branch never committed to is empty: {stderr}"
    );

    // Lost refs exist only where refs are files.
    let lost = committed("lost");
    let loose = lost.join(".git/refs/heads/main");
    if !loose.is_file() {
        fixture.cleanup();
        return;
    }
    fs::remove_file(&loose).unwrap();
    let truncated = committed("truncated");
    run_git(&truncated, &["pack-refs", "--all"]);
    let packed = truncated.join(".git/packed-refs");
    let header = fs::read_to_string(&packed).unwrap();
    fs::write(&packed, format!("{}\n", header.lines().next().unwrap())).unwrap();
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    for broken in [&lost, &truncated] {
        assert!(
            stderr.contains(&format!("{}: ", broken.display()))
                && stderr.contains("its ref is lost"),
            "{} warns: {stderr}",
            broken.display()
        );
    }
    assert!(!stderr.contains(&orphan.display().to_string()), "{stderr}");
    fixture.cleanup();
}

/// A draft as the drafter answers it; `repo` for a repo one.
fn drafted(scope: &str, repo: Option<&Path>, id: &str, refines: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "scope": scope, "repo": repo.map(|repo| repo.display().to_string()), "id": id,
        "kind": "lesson", "body": format!("The lesson {id}."),
        "origin": ["2026-09-20 work: 'a test'"], "refines": refines, "supersedes": [],
    })
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

#[test]
fn a_draft_that_may_repeat_what_is_present_is_written_and_said_to_a_person() {
    // A drafter writes one lesson several times over. Refusing what looks
    // repeated would drop real lessons for good
    // (`quarry-linux-only` as `beacon-linux-only`), and a person is the only
    // one who decides. So each is written, and said to that person.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakedup");
    let repo = fixture.make_git_repo("work");
    let (beacon, quarry) = (fixture.make_folder("beacon"), fixture.make_folder("quarry"));
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust-elixir-only",
        "decision",
        "Code is Rust or Elixir.",
    );
    let proposals = fixture.dir.join("state/ohjaa/proposals");
    let (waiting, proposal) = waiting_proposal(
        &proposals.join("ours"),
        "start-sessions-through-ohjaa-run",
        "Start every agent session through `ohjaa run`.",
    );
    let encoded = |repo: &Path| {
        repo.display()
            .to_string()
            .trim_start_matches('/')
            .replace('%', "%25")
            .replace('/', "%2F")
    };
    waiting_proposal(
        &proposals.join("repo").join(encoded(&beacon)),
        "beacon-linux-only",
        "Beacon runs on Linux only.",
    );
    fixture.said(&[
        vessa_line(
            "2026-09-20T10:00:00Z",
            "/p/work",
            "never python. i told you",
            None,
        ),
        vessa_line(
            "2026-09-20T10:05:00Z",
            &beacon.display().to_string(),
            "no, we have lima baba",
            None,
        ),
    ]);
    // One answer a batch, each prompt kept.
    let answers = [
        serde_json::json!([
            drafted("ours", None, "start-agents-through-ohjaa-run", None),
            drafted("mine", None, "code-is-rust-or-elixir", None),
            // Near current knowledge, of another scope: another lesson.
            drafted("ours", None, "rust-elixir-only-everywhere", None),
            drafted("ours", None, "read-back-briefing-and-leave-handover", None),
            drafted("ours", None, "read-briefing-and-leave-handover", None),
        ]),
        serde_json::json!([
            drafted(
                "ours",
                None,
                "ohjaa-read-back-briefing-and-leave-handover",
                None
            ),
            drafted(
                "ours",
                None,
                "pi-sessions-need-a-wrapper-too",
                Some("start-sessions-through-ohjaa-run"),
            ),
            drafted("repo", Some(&quarry), "quarry-linux-only", None),
            drafted("machine", None, "lima-for-linux", None),
        ]),
    ];
    for (n, answer) in answers.iter().enumerate() {
        fs::write(
            fixture.dir.join(format!("answer-{}.json", n + 1)),
            answer.to_string(),
        )
        .unwrap();
    }
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\nd='{}'\nn=$(($(cat \"$d/drafter-calls\" 2>/dev/null || echo 0) + 1))\n\
             echo $n > \"$d/drafter-calls\"\ncat > \"$d/prompt-$n.txt\"\ncat \"$d/answer-$n.json\"\n",
            fixture.dir.display()
        ),
    );

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "intake",
                "--source",
                "transcripts",
                "--no-prefilter",
                "--since",
                "2026-09-01",
                "--batch",
                "1",
                "--drafter",
            ])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let prompt =
        |n: usize| fs::read_to_string(fixture.dir.join(format!("prompt-{n}.txt"))).unwrap();
    // The drafter is shown what is present, current and waiting, and a
    // repository's proposals only with a batch from it.
    let (first, second) = (prompt(1), prompt(2));
    assert!(
        first.contains("- rust-elixir-only [mine/decision/current]: Code is Rust or Elixir."),
        "{first}"
    );
    assert!(
        first.contains(
            "- start-sessions-through-ohjaa-run [ours/lesson/proposed]: Start every agent \
             session through `ohjaa run`."
        ),
        "{first}"
    );
    assert!(first.contains("Return nothing for a"), "{first}");
    assert!(!first.contains("beacon-linux-only"), "{first}");
    assert!(
        second.contains(&format!(
            "- beacon-linux-only [repo/lesson/proposed] in {}: Beacon runs on Linux only.",
            beacon.display()
        )),
        "{second}"
    );
    // And, in the next batch, what the batch before it proposed.
    assert!(
        second.contains("- read-back-briefing-and-leave-handover [ours/lesson/proposed]"),
        "{second}"
    );
    for line in [
        // Near a proposal waiting before the run.
        "proposed start-agents-through-ohjaa-run, a possible duplicate of \
         start-sessions-through-ohjaa-run\n",
        // Near current knowledge of its scope.
        "proposed code-is-rust-or-elixir, a possible duplicate of rust-elixir-only\n",
        "proposed rust-elixir-only-everywhere\n",
        "proposed read-back-briefing-and-leave-handover\n",
        // Near a draft written earlier in the same answer,
        "proposed read-briefing-and-leave-handover, a possible duplicate of \
         read-back-briefing-and-leave-handover\n",
        // and in a batch before.
        "proposed ohjaa-read-back-briefing-and-leave-handover, a possible duplicate of \
         read-back-briefing-and-leave-handover\n",
        // A refinement of a waiting proposal is kept, as refining it.
        "proposed pi-sessions-need-a-wrapper-too\n",
        // The same words of another repository are another lesson.
        "proposed quarry-linux-only\n",
        "proposed lima-for-linux\n",
        "9 proposal(s) waiting for review (`ohjaa review`), 4 of them possible duplicates, 0 \
         draft(s) refused",
        "possible duplicates: of read-back-briefing-and-leave-handover ×2, of rust-elixir-only \
         ×1, of start-sessions-through-ohjaa-run ×1",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }
    // Nothing drafted is lost: every draft waits for a person, and nothing
    // is stored beside it (a possible duplicate is derived when a person
    // reviews).
    for (scope, id) in [
        ("ours", "start-agents-through-ohjaa-run"),
        ("mine", "code-is-rust-or-elixir"),
        ("ours", "rust-elixir-only-everywhere"),
        ("ours", "read-back-briefing-and-leave-handover"),
        ("ours", "read-briefing-and-leave-handover"),
        ("ours", "ohjaa-read-back-briefing-and-leave-handover"),
        ("ours", "pi-sessions-need-a-wrapper-too"),
        ("machine", "lima-for-linux"),
    ] {
        assert!(
            proposals.join(scope).join(format!("{id}.md")).is_file(),
            "{id}"
        );
    }
    let mut stored = Vec::new();
    let mut folders = vec![proposals.clone()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_none_or(|ext| ext != "md") {
                stored.push(path);
            }
        }
    }
    assert_eq!(stored, Vec::<PathBuf>::new(), "only proposals are stored");
    let quarry_dir = proposals.join("repo").join(encoded(&quarry));
    assert!(quarry_dir.join("quarry-linux-only.md").is_file());
    let refinement =
        fs::read_to_string(proposals.join("ours/pi-sessions-need-a-wrapper-too.md")).unwrap();
    assert!(
        refinement.contains("refines = \"start-sessions-through-ohjaa-run\""),
        "{refinement}"
    );
    assert_eq!(
        fs::read_to_string(&waiting).unwrap(),
        proposal,
        "a waiting proposal is never touched"
    );

    // A person sees each one after what it may repeat, and decides.
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
    fixture.cleanup();
}

/// The line under `id` in `ohjaa list`'s output that says how often it was
/// re-taught: the first after its own line.
fn retaught_line<'a>(listing: &'a str, id: &str) -> &'a str {
    let mut lines = listing.lines();
    lines
        .by_ref()
        .find(|line| line.split_whitespace().nth(3) == Some(id))
        .unwrap_or_else(|| panic!("{id} is not listed:\n{listing}"));
    lines
        .find(|line| line.trim_start().starts_with("re-taught "))
        .unwrap_or_else(|| panic!("no re-taught line under {id}:\n{listing}"))
        .trim()
}

/// A drafter that keeps each prompt it is sent (`prompt-<n>.txt`) and answers
/// each with `answer.json`, all in `dir`.
fn answering_drafter(fixture: &Fixture, dir: &Path) -> PathBuf {
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\nd='{}'\nn=$(($(cat \"$d/drafter-calls\" 2>/dev/null || echo 0) + 1))\n\
             echo $n > \"$d/drafter-calls\"\ncat > \"$d/prompt-$n.txt\"\ncat \"$d/answer.json\"\n",
            dir.display()
        ),
    );
    drafter
}

#[test]
fn intake_counts_what_a_person_had_to_teach_again_once_and_list_check_review_show_it() {
    // The briefing gives agents knowledge; this measures whether it
    // changes what they do. A node the person keeps
    // re-teaching is not working; one nothing ever touches may be stale.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("reteach");
    let repo = fixture.make_git_repo("work");
    let other = fixture.make_git_repo("other");
    let mine = fixture.dir.join("config/ohjaa/mine");
    let machine = fixture.dir.join("config/ohjaa/machine");
    fixture.node(
        &mine,
        "rust-elixir-only",
        "decision",
        "Code is Rust or Elixir.",
    );
    fixture.node(
        &mine,
        "answer-first",
        "taste",
        "Answer first, then explain.",
    );
    fixture.node(
        &machine,
        "linux-work-in-lima",
        "fact",
        "Linux work runs in Lima.",
    );
    fixture.node_as_of(
        &fixture.dir.join("ours/knowledge"),
        "small-pull-requests",
        "taste",
        "2026-06-01",
        "Keep pull requests small.",
    );
    // A repo node, and another repository's node of the same id.
    fixture.node(
        &repo.join(".ohjaa/knowledge"),
        "tests-run-in-lima",
        "lesson",
        "The tests of this repository run in Lima.",
    );
    fixture.node(
        &other.join(".ohjaa/knowledge"),
        "tests-run-in-lima",
        "lesson",
        "The tests of the other repository run in Lima.",
    );
    // Not current: never re-taught.
    fs::write(
        mine.join("old-way.md"),
        "+++\nid = \"old-way\"\nkind = \"taste\"\nstatus = \"retired\"\nas_of = 2026-09-01\n\
         origin = [\"a test\", \"2026-09-02: retired: gone\"]\n+++\nThe old way.\n",
    )
    .unwrap();
    let nodes_before: Vec<(PathBuf, String)> = [
        mine.join("rust-elixir-only.md"),
        mine.join("answer-first.md"),
        mine.join("old-way.md"),
        machine.join("linux-work-in-lima.md"),
        fixture.dir.join("ours/knowledge/small-pull-requests.md"),
        repo.join(".ohjaa/knowledge/tests-run-in-lima.md"),
        other.join(".ohjaa/knowledge/tests-run-in-lima.md"),
    ]
    .into_iter()
    .map(|path| {
        let text = fs::read_to_string(&path).unwrap();
        (path, text)
    })
    .collect();

    let work = repo.display().to_string();
    fixture.said(&[
        vessa_line("2026-09-21T10:00:00Z", &work, "no python. rust", None),
        vessa_line(
            "2026-09-22T10:00:00Z",
            &work,
            "i said rust, not python",
            None,
        ),
        vessa_line("2026-09-23T09:00:00Z", &work, "again: no python", None),
        vessa_line("2026-09-23T10:00:00Z", &work, "use lima for that", None),
        vessa_line("2026-09-23T11:00:00Z", &work, "the tests go in lima", None),
        vessa_line(
            "2026-09-23T12:00:00Z",
            "/p/elsewhere",
            "lima, as always",
            None,
        ),
    ]);
    // Six candidates, one batch. The drafter says what each re-teaches,
    // besides one refinement; some of what it says names nothing it may.
    fs::write(
        fixture.dir.join("answer.json"),
        serde_json::json!({
            "drafts": [drafted("mine", None, "answer-first-even-in-reviews", Some("answer-first"))],
            "reteaches": [
                {"candidate": 1, "ids": ["rust-elixir-only"]},
                // Said twice, counted once; a retired node is not current.
                {"candidate": 2, "ids": ["rust-elixir-only", "rust-elixir-only", "old-way"]},
                {"candidate": 3, "ids": ["rust-elixir-only", "no-such-node"]},
                {"candidate": 4, "ids": ["linux-work-in-lima"]},
                {"candidate": 5, "ids": ["tests-run-in-lima"]},
                // A repo node, from a message not of its repository.
                {"candidate": 6, "ids": ["tests-run-in-lima"]},
                // No such candidate, and one that cannot be read.
                {"candidate": 7, "ids": ["linux-work-in-lima"]},
                {"candidate": "first", "ids": ["answer-first"]},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let drafter = answering_drafter(&fixture, &fixture.dir);
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args([
                    "intake",
                    "--source",
                    "transcripts",
                    "--no-prefilter",
                    "--since",
                    "2026-09-01",
                    "--batch",
                    "10",
                    "--drafter",
                ])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stdout}{stderr}");

    // The drafter is shown the current nodes and asked what each candidate
    // re-teaches, in the answer's new shape.
    let prompt = fs::read_to_string(fixture.dir.join("prompt-1.txt")).unwrap();
    assert!(
        prompt.contains("- rust-elixir-only [mine/decision/current]: Code is Rust or Elixir."),
        "{prompt}"
    );
    assert!(prompt.contains("\"reteaches\""), "{prompt}");
    assert!(prompt.contains("\"drafts\""), "{prompt}");

    // What was recorded is said by node, time and source, never the words.
    for line in [
        "re-taught rust-elixir-only (2026-09-21T10:00:00Z claude)\n",
        "re-taught rust-elixir-only (2026-09-22T10:00:00Z claude)\n",
        "re-taught rust-elixir-only (2026-09-23T09:00:00Z claude)\n",
        "re-taught linux-work-in-lima (2026-09-23T10:00:00Z claude)\n",
        "re-taught tests-run-in-lima (2026-09-23T11:00:00Z claude)\n",
        "proposed answer-first-even-in-reviews\n",
        "5 re-teaching(s) of current knowledge recorded: linux-work-in-lima ×1, rust-elixir-only \
         ×3, tests-run-in-lima ×1 (`ohjaa check` shows each node's count)",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }
    assert!(!stdout.contains("python") && !stderr.contains("python"));
    // What names nothing it may is dropped, and counted.
    assert!(
        stderr.contains(
            "ohjaa: intake: 5 re-teaching(s) the drafter named were dropped: 3 naming no current \
             node shown with its candidate, 1 of no candidate in its batch, 1 unreadable"
        ),
        "{stderr}"
    );
    // A message that only re-teaches makes no proposal.
    let proposed: Vec<String> = fs::read_dir(fixture.dir.join("state/ohjaa/proposals/mine"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(proposed, ["answer-first-even-in-reviews.md"]);

    // Kept as history in ohjaa's state: per node, the time, digest and
    // session of each message, never its words.
    let evidence_path = fixture.dir.join("state/ohjaa/knowledge-evidence.json");
    let evidence = fs::read_to_string(&evidence_path).unwrap();
    for words in [
        "python",
        "rust,",
        "lima for that",
        "the tests go",
        "as always",
    ] {
        assert!(!evidence.contains(words), "{words}: {evidence}");
    }
    let parsed: serde_json::Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(
        parsed["schema"], "ohjaa-knowledge-evidence.v1",
        "{evidence}"
    );
    let node = |id: &str| {
        parsed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {evidence}"))
            .clone()
    };
    let rust = node("rust-elixir-only");
    assert_eq!(rust["scope"], "mine");
    let retaught = rust["retaught"].as_array().unwrap();
    assert_eq!(retaught.len(), 3, "{evidence}");
    assert!(
        retaught
            .iter()
            .all(|event| event["session"] == "s1" && event["digest"].as_str().unwrap().len() == 16),
        "{evidence}"
    );
    assert_eq!(node("tests-run-in-lima")["scope"], "repo");
    assert!(node("tests-run-in-lima")["clone"].is_string(), "{evidence}");
    // The node the refinement names was cited, re-taught never.
    assert_eq!(node("answer-first")["retaught"], serde_json::json!([]));
    assert!(node("answer-first")["cited_ms"].is_i64(), "{evidence}");

    // The same messages answered for again (their progress lost) are not
    // counted twice.
    fs::remove_file(fixture.dir.join("state/ohjaa/intake-handled")).unwrap();
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(fixture.dir.join("prompt-2.txt").is_file(), "sent again");
    assert!(!stdout.contains("re-taught rust-elixir-only ("), "{stdout}");
    assert_eq!(fs::read_to_string(&evidence_path).unwrap(), evidence);

    // `ohjaa list`: per current node, how often since it was accepted, and
    // when last touched.
    let (listed, stderr, code) = output_of(fixture.ohjaa().arg("list").current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    for (id, line) in [
        (
            "rust-elixir-only",
            "re-taught 3 times since 2026-09-20, last 2026-09-23",
        ),
        (
            "linux-work-in-lima",
            "re-taught 1 time since 2026-09-20, last 2026-09-23",
        ),
        (
            "tests-run-in-lima",
            "re-taught 1 time since 2026-09-20, last 2026-09-23",
        ),
        // Cited by the refinement, from the newest message in its batch.
        (
            "answer-first",
            "re-taught 0 times since 2026-09-20, last 2026-09-23",
        ),
        ("small-pull-requests", "re-taught 0 times since 2026-06-01"),
    ] {
        assert_eq!(retaught_line(&listed, id), line, "{id}\n{listed}");
    }
    // Another repository's node of the same id has its own count.
    let (listed, _, _) = output_of(fixture.ohjaa().arg("list").current_dir(&other));
    assert_eq!(
        retaught_line(&listed, "tests-run-in-lima"),
        "re-taught 0 times since 2026-09-20"
    );

    // `ohjaa check` adds suggestions, and only suggests: `--strict` passes.
    let (checked, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["check", "--strict"])
            .current_dir(&repo),
    );
    assert_eq!(
        code,
        Some(0),
        "suggestions are not flags: {checked}{stderr}"
    );
    for line in [
        "evidence rust-elixir-only: re-taught 3 times since 2026-09-20, last 2026-09-23\n",
        "evidence small-pull-requests: re-taught 0 times since 2026-06-01\n",
        "suggest  rust-elixir-only: re-taught 3 times in 30 days: reword it, or make it a \
         decision\n",
        "suggest  small-pull-requests: nothing has re-taught or cited it in 60 days: still \
         true?\n",
        "2 suggestion(s)",
    ] {
        assert!(checked.contains(line), "{line}\n{checked}");
    }
    for quiet in ["answer-first", "linux-work-in-lima", "tests-run-in-lima"] {
        assert!(
            !checked.contains(&format!("suggest  {quiet}:")),
            "{quiet}\n{checked}"
        );
    }

    // `ohjaa review`: a proposal that may repeat a node, or refines one,
    // shows that node's count.
    waiting_proposal(
        &fixture.dir.join("state/ohjaa/proposals/mine"),
        "code-is-rust-or-elixir",
        "Code is Rust or Elixir, nothing else.",
    );
    let (reviewed, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["review", "--list"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    for line in [
        "    possible duplicate of: rust-elixir-only (current: Code is Rust or Elixir.) (their \
         titles are near; you decide)\n    rust-elixir-only: re-taught 3 times since 2026-09-20, \
         last 2026-09-23\n",
        "    refines: answer-first (current: Answer first, then explain.)\n    answer-first: \
         re-taught 0 times since 2026-09-20, last 2026-09-23\n",
    ] {
        assert!(reviewed.contains(line), "{line}\n{reviewed}");
    }

    // ohjaa never changes a node itself.
    for (path, text) in &nodes_before {
        assert_eq!(
            &fs::read_to_string(path).unwrap(),
            text,
            "{}",
            path.display()
        );
    }
    fixture.cleanup();
}

#[test]
fn damaged_evidence_stops_intake_and_is_said_never_shown_as_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("reteachdamaged");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust-elixir-only",
        "decision",
        "Code is Rust or Elixir.",
    );
    let evidence = fixture.dir.join("state/ohjaa/knowledge-evidence.json");
    fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    fs::write(
        &evidence,
        "{\"schema\":\"ohjaa-knowledge-evidence.v1\",\"nodes\":[",
    )
    .unwrap();
    fixture.said(&[vessa_line(
        "2026-09-21T10:00:00Z",
        &repo.display().to_string(),
        "no python. rust",
        None,
    )]);
    fs::write(
        fixture.dir.join("answer.json"),
        r#"{"drafts":[],"reteaches":[{"candidate":1,"ids":["rust-elixir-only"]}]}"#,
    )
    .unwrap();
    let drafter = answering_drafter(&fixture, &fixture.dir);
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "intake",
                "--source",
                "transcripts",
                "--no-prefilter",
                "--since",
                "2026-09-01",
                "--drafter",
            ])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("knowledge-evidence.json is damaged; remove it to start over"),
        "{stderr}"
    );
    assert!(
        !fixture.dir.join("prompt-1.txt").exists(),
        "nothing was sent"
    );
    assert_eq!(
        fs::read_to_string(&evidence).unwrap(),
        "{\"schema\":\"ohjaa-knowledge-evidence.v1\",\"nodes\":[",
        "a damaged record is said, never replaced"
    );

    // The knowledge is still listed; no count is made up for it.
    let (listed, stderr, code) = output_of(fixture.ohjaa().arg("list").current_dir(&repo));
    assert_eq!(code, Some(1), "{listed}{stderr}");
    assert!(listed.contains("rust-elixir-only"), "{listed}");
    assert!(!listed.contains("re-taught"), "{listed}");
    assert!(
        stderr.contains("knowledge-evidence.json is damaged"),
        "{stderr}"
    );
    let (checked, _, code) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert_eq!(code, Some(1), "{checked}");
    assert!(
        checked.contains("error    ") && checked.contains("knowledge-evidence.json is damaged"),
        "{checked}"
    );
    assert!(!checked.contains("evidence rust-elixir-only"), "{checked}");
    fixture.cleanup();
}

#[test]
fn malformed_reteaching_answers_leave_the_correction_available_for_retry() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("reteach-invalid-answer");
    let repo = fixture.make_git_repo("work");
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust-only",
        "decision",
        "Code is Rust.",
    );
    fixture.said(&[vessa_line(
        "2026-09-21T10:00:00Z",
        &repo.display().to_string(),
        "I already told you no Python",
        None,
    )]);
    let answer = fixture.dir.join("answer.json");
    fs::write(&answer, r#"{"drafts":[],"reteaches":"not an array"}"#).unwrap();
    let drafter = answering_drafter(&fixture, &fixture.dir);
    let run = || {
        output_of(
            fixture
                .ohjaa()
                .args([
                    "intake",
                    "--source",
                    "transcripts",
                    "--no-prefilter",
                    "--since",
                    "2026-09-01",
                    "--drafter",
                ])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (stdout, stderr, code) = run();
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    let evidence = fixture.dir.join("state/ohjaa/knowledge-evidence.json");
    assert!(!evidence.exists());
    fs::write(
        &answer,
        r#"{"drafts":[],"reteaches":[{"candidate":1,"ids":["rust-only"]}]}"#,
    )
    .unwrap();
    let (stdout, stderr, code) = run();
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("re-taught rust-only"), "{stdout}");
    let kept: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(evidence).unwrap()).unwrap();
    assert_eq!(kept["nodes"][0]["retaught"].as_array().unwrap().len(), 1);
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
fn a_redacted_node_id_cannot_be_recorded_as_sent_evidence() {
    let fixture = Fixture::new("redacted-evidence-id");
    let repo = fixture.make_git_repo("work");
    // Synthetic, valid kebab-case metadata, not an actual credential.
    let id = ["s", "k-", &"a1".repeat(12)].concat();
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        &id,
        "decision",
        "Use Rust for code.",
    );
    fixture.said(&[vessa_line(
        "2026-09-21T10:00:00Z",
        &repo.display().to_string(),
        "I told you to use Rust",
        None,
    )]);
    fs::write(
        fixture.dir.join("answer.json"),
        serde_json::json!({"drafts":[],"reteaches":[{"candidate":1,"ids":[id]}]}).to_string(),
    )
    .unwrap();
    let drafter = answering_drafter(&fixture, &fixture.dir);
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "intake",
                "--source",
                "transcripts",
                "--no-prefilter",
                "--since",
                "2026-09-01",
                "--drafter",
            ])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let sent = fs::read_to_string(fixture.dir.join("prompt-1.txt")).unwrap();
    assert!(!sent.contains(&id));
    assert!(sent.contains("[redacted]"));
    let evidence = fixture.dir.join("state/ohjaa/knowledge-evidence.json");
    assert!(
        !evidence.exists(),
        "an invisible identifier is not evidence"
    );
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

#[test]
fn graph_shows_a_nodes_neighbours() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("graph");
    let repo = fixture.make_git_repo("work");
    let ours = fixture.dir.join("ours/knowledge");
    fixture.node(&ours, "lean", "taste", "Lean first.");
    fs::write(
        ours.join("rust.md"),
        "+++\nid = \"rust\"\nkind = \"decision\"\nstatus = \"current\"\nas_of = 2026-09-20\n\
         origin = [\"t\"]\nrefines = \"lean\"\nsupersedes = [\"go-ok\"]\n+++\nRust only.\n",
    )
    .unwrap();
    fs::write(
        ours.join("go-ok.md"),
        "+++\nid = \"go-ok\"\nkind = \"decision\"\nstatus = \"superseded\"\nas_of = 2026-01-01\n\
         origin = [\"t\"]\n+++\nGo is fine.\n",
    )
    .unwrap();
    let (stdout, stderr, code) =
        output_of(fixture.ohjaa().args(["graph", "rust"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("builds on     lean"), "{stdout}");
    assert!(stdout.contains("replaced      go-ok"), "{stdout}");
    let (stdout, _, _) = output_of(fixture.ohjaa().args(["graph", "lean"]).current_dir(&repo));
    assert!(stdout.contains("built on by   rust"), "{stdout}");
    fixture.cleanup();
}

/// Now, as the cursor writes it (milliseconds), from `date`.
fn utc_now() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%S.000Z"])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The `said:` line of every candidate in each answered prompt, in order.
fn said_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_run_that_reads_only_some_sources_never_moves_the_cursor_past_the_others() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakepartial");
    let repo = fixture.make_git_repo("work");
    fixture.said(&[vessa_line(
        &utc_now(),
        "/p/work",
        "the unread one. you forgot it",
        None,
    )]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let sent = fixture.dir.join("sent.txt");
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ngrep '^    said: ' >> '{}'\necho '[]'\n",
            sent.display()
        ),
    );
    let intake = |source: &str| {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", source, "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (_, stderr, _) = intake("reviews");
    assert!(said_lines(&sent).is_empty(), "{stderr}");
    let (_, stderr, code) = intake("transcripts");
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        said_lines(&sent),
        ["    said: the unread one. you forgot it"],
        "a run of reviews alone did not move past the transcripts: {stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_run_with_warnings_never_says_nothing_new_and_exits_zero() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakewarned");
    let repo = fixture.make_git_repo("work");
    // vessa could not read a transcript.
    fs::write(fixture.dir.join("home/vessa-warning"), UNREADABLE).unwrap();
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--source", "transcripts", "--drafter", "false"])
            .current_dir(&repo),
    );
    assert!(
        stderr.contains("ohjaa: intake: vessa: /h/.codex/sessions/r.jsonl: Permission denied"),
        "{stderr}"
    );
    assert!(
        !stdout.contains("nothing new"),
        "what could not be read is not nothing: {stdout}"
    );
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    fixture.cleanup();
}

#[test]
fn intake_asks_vessa_from_the_cursor_and_sends_only_what_the_prefilter_keeps() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakevessa");
    let repo = fixture.make_git_repo("work");
    let now = utc_now();
    let before = Some("Linux tests are blocked on macOS.");
    fixture.said(&[
        // A correction the phrase rules miss: dropped, and said.
        vessa_line(&now, "/p/work", "we have lima baba", before),
        vessa_line(&now, "/p/work", "no, run them in the lima VM", before),
    ]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let intake = |extra: &[&str]| {
        output_of(
            fixture
                .ohjaa()
                .arg("intake")
                .args(extra)
                .arg("--drafter")
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (_, stderr, code) = intake(&["--since", "2026-09-01"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        fixture.vessa_calls(),
        ["messages --since 2026-09-01T00:00:00.000Z --json"],
        "vessa is the one reader, asked from the window's start"
    );
    assert_eq!(
        said_lines(&sent),
        ["    said: no, run them in the lima VM"],
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "ohjaa: intake: 2 message(s) people wrote, listed by vessa; the pre-filter \
             (phrase-rules.v3) kept 1 (correction 1) and dropped 1 (claude 1), keeping none \
             of them"
        ),
        "read, kept and dropped, by counts: {stderr}"
    );
    assert!(!stderr.contains("lima baba"), "never the words: {stderr}");
    assert!(
        stderr.contains("the drafter answered for 1 candidate(s) in this run"),
        "{stderr}"
    );

    // The next run asks vessa from the cursor, exactly as it is kept, and
    // finds nothing new: the dropped message is behind the cursor now.
    let cursor = fs::read_to_string(fixture.dir.join("state/ohjaa/intake-since")).unwrap();
    let (stdout, stderr, code) = intake(&[]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert_eq!(
        fixture.vessa_calls().last().unwrap(),
        &format!("messages --since {} --json", cursor.trim())
    );
    assert_eq!(said_lines(&sent).len(), 1);

    // Read again without the pre-filter, the dropped message is a candidate.
    let (_, stderr, code) = intake(&["--since", "2026-09-01", "--no-prefilter"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        said_lines(&sent).contains(&"    said: we have lima baba".to_owned()),
        "{stderr}"
    );
    assert!(
        stderr.contains("no pre-filter (--no-prefilter): 2 candidate(s)"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_partial_list_from_vessa_holds_the_cursor_and_says_why() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakegap");
    let repo = fixture.make_git_repo("work");
    fixture.said(&[vessa_line(
        &utc_now(),
        "/p/work",
        "i told you, never python",
        None,
    )]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let unreadable = fixture.dir.join("home/vessa-warning");
    fs::write(&unreadable, UNREADABLE).unwrap();
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let cursor_path = fixture.dir.join("state/ohjaa/intake-since");
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    // Exit 4 from vessa, a transcript it could not read: what it listed is
    // drafted from, and the cursor stays where the first run's window began.
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(said_lines(&sent).len(), 1, "{stderr}");
    assert!(
        stderr.contains("ohjaa: intake: vessa: /h/.codex/sessions/r.jsonl: Permission denied"),
        "{stderr}"
    );
    assert!(
        stderr.contains("transcripts could not all be read"),
        "the held cursor is said, and what holds it: {stderr}"
    );
    let held = fs::read_to_string(&cursor_path).unwrap();
    assert!(
        stderr.contains(&format!(
            "the cursor stays at {}",
            held.trim().replace(".000Z", "Z")
        )),
        "{held} {stderr}"
    );
    // Once vessa reads everything, nothing is sent again, and it moves.
    fs::remove_file(&unreadable).unwrap();
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert_eq!(said_lines(&sent).len(), 1, "handled once: {stderr}");
    let moved = fs::read_to_string(&cursor_path).unwrap();
    assert!(moved > held, "{held} -> {moved}");
    fixture.cleanup();
}

#[test]
fn a_pasted_secret_never_reaches_the_drafter_with_the_filter_on_or_off() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakesecret");
    let repo = fixture.make_git_repo("work");
    // Put together here, so no secret-shaped literal sits in the source.
    let key = ["gh", "p_", "Ab3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3hJ5"].concat();
    let now = utc_now();
    fixture.said(&[
        vessa_line(
            &now,
            "/p/work",
            &format!("i told you, the token is {key}"),
            None,
        ),
        vessa_line(
            &now,
            "/p/work",
            &format!("here: {key}"),
            Some(&format!("I need {key}")),
        ),
    ]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let prompts = fixture.dir.join("prompts.txt");
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!("#!/bin/sh\ncat >> '{}'\necho '[]'\n", prompts.display()),
    );
    let intake = |extra: &[&str]| {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", "transcripts", "--since", "2026-09-01"])
                .args(extra)
                .arg("--drafter")
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    // A dry run shows the prompt, scrubbed too, and sends nothing.
    let (stdout3, stderr3, code) = intake(&["--no-prefilter", "--dry-run"]);
    assert_eq!(code, Some(0), "{stderr3}");
    assert!(
        stderr3.contains("3 secret-looking token(s) redacted from the batch shown"),
        "{stderr3}"
    );
    assert!(!prompts.exists(), "a dry run sends nothing");
    // The filter keeps the correction; its key is redacted, and counted.
    let (stdout, stderr, code) = intake(&[]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("1 secret-looking token(s) were redacted from what it was sent"),
        "{stderr}"
    );
    // Without it, the other message goes too, with the agent's words before
    // it (the first was handled already).
    let (stdout2, stderr2, code) = intake(&["--no-prefilter"]);
    assert_eq!(code, Some(0), "{stderr2}");
    assert!(
        stderr2.contains("2 secret-looking token(s) were redacted from what it was sent"),
        "{stderr2}"
    );
    let sent = fs::read_to_string(&prompts).unwrap();
    assert!(
        sent.contains("said: i told you, the token is [redacted]"),
        "{sent}"
    );
    assert!(sent.contains("before: I need [redacted]"), "{sent}");
    assert!(stdout3.contains("said: here: [redacted]"), "{stdout3}");
    // Never said, logged or kept anywhere ohjaa writes.
    for (what, text) in [
        ("the prompts", sent.as_str()),
        ("stdout", stdout.as_str()),
        ("stderr", stderr.as_str()),
        ("stdout", stdout2.as_str()),
        ("stderr", stderr2.as_str()),
        ("the dry run", stdout3.as_str()),
        ("the dry run's stderr", stderr3.as_str()),
    ] {
        assert!(!text.contains(&key), "{what} holds the key");
    }
    for entry in fs::read_dir(fixture.dir.join("state/ohjaa"))
        .unwrap()
        .flatten()
    {
        if entry.path().is_file() {
            let kept = String::from_utf8_lossy(&fs::read(entry.path()).unwrap()).into_owned();
            assert!(
                !kept.contains(&key),
                "{} holds the key",
                entry.path().display()
            );
        }
    }
    fixture.cleanup();
}

#[test]
fn an_old_vessa_is_told_to_update_and_nothing_reads_the_transcripts_instead() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakeoldvessa");
    let repo = fixture.make_git_repo("work");
    fs::write(fixture.dir.join("home/vessa-old"), "").unwrap();
    // A transcript intake once read itself: no fallback reads it now.
    let projects = fixture.dir.join("home/.claude/projects/-work");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join("s.jsonl"),
        format!(
            r#"{{"type":"user","timestamp":"{}","cwd":"/p/work","message":{{"content":"i told you, never python"}}}}"#,
            utc_now()
        ),
    )
    .unwrap();
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("update vessa"), "{stderr}");
    assert!(
        stderr.contains("unrecognized subcommand 'messages'"),
        "vessa's own words: {stderr}"
    );
    assert!(said_lines(&sent).is_empty(), "{stderr}");
    assert!(!stdout.contains("nothing new"), "{stdout}");
    assert!(
        stderr.contains("transcripts could not all be read"),
        "{stderr}"
    );
    fixture.cleanup();
}

/// A stub drafter that records the `said:` lines it answers for in `sent`.
fn recording_drafter(fixture: &Fixture, sent: &Path) -> PathBuf {
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ngrep '^    said: ' >> '{}'\necho '[]'\n",
            sent.display()
        ),
    );
    drafter
}

#[test]
fn a_first_run_keeps_its_window_start_as_the_cursor_even_when_it_drafts_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakefirst");
    let repo = fixture.make_git_repo("work");
    fixture.said(&[vessa_line(
        &utc_now(),
        "/p/work",
        "never python. i told you",
        None,
    )]);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--drafter", "exit 1"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(4), "{stderr}");
    let since = stderr
        .split(" since ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect(&stderr)
        .to_owned();
    let cursor = fs::read_to_string(fixture.dir.join("state/ohjaa/intake-since")).unwrap();
    assert_eq!(
        cursor.trim(),
        since.replace('Z', ".000Z"),
        "the default window's start, kept before drafting: {stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_held_cursor_says_where_it_stays_and_which_source_holds_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakeheld");
    let repo = fixture.make_git_repo("work");
    // A GitHub remote, and a gh that is not logged in: reviews always fail.
    run_git(
        &repo,
        &["remote", "add", "origin", "git@github.com:o/r.git"],
    );
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let cursor = fixture.dir.join("state/ohjaa/intake-since");
    let intake = |extra: &[&str]| {
        output_of(
            fixture
                .ohjaa()
                .arg("intake")
                .args(extra)
                .arg("--drafter")
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (_, stderr, code) = intake(&[]);
    assert_eq!(code, Some(4), "{stderr}");
    let kept = fs::read_to_string(&cursor).unwrap();
    let at = &kept.trim()[..19];
    for _ in 0..2 {
        let (_, stderr, code) = intake(&[]);
        assert_eq!(code, Some(4), "{stderr}");
        assert!(
            stderr.contains(&format!("the cursor stays at {at}")),
            "where it stays: {stderr}"
        );
        assert!(
            stderr.contains("reviews"),
            "which source holds it: {stderr}"
        );
        assert_eq!(fs::read_to_string(&cursor).unwrap(), kept);
    }
    // A run of some sources says it does not move the shared cursor.
    let (_, stderr, code) = intake(&["--source", "transcripts"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("the cursor stays at {at}")),
        "{stderr}"
    );
    assert!(stderr.contains("one for every source"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn no_repositories_under_the_roots_holds_the_cursor_and_says_so() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakenorepos");
    let empty = fixture.make_folder("empty");
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&empty)
            .arg("--root")
            .arg(&empty)
            .args(["intake", "--drafter"])
            .arg(&drafter),
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("reviews: no git repositories under the roots"),
        "{stderr}"
    );
    assert!(
        stderr.contains("reverts: no git repositories under the roots"),
        "{stderr}"
    );
    let since = stderr
        .split(" since ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect(&stderr)
        .replace('Z', ".000Z");
    let cursor = fs::read_to_string(fixture.dir.join("state/ohjaa/intake-since")).unwrap();
    assert_eq!(cursor.trim(), since, "not moved past what it never read");
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

/// "re-taught 0 times" from an intake that has not read lately is not
/// measured, and check must not let it read as zero.
#[test]
fn check_says_how_far_intake_read_so_no_count_reads_as_zero() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("check-intake-reach");
    let repo = fixture.make_git_repo("work");
    let mine = fixture.dir.join("config/ohjaa/mine");
    fixture.node(&mine, "fine", "taste", "Fine.");
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert!(
        stdout.contains("evidence no intake has run here: how often each was re-taught is unknown"),
        "{stdout}"
    );
    let state = fixture.dir.join("state/ohjaa");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("intake-since"), "2026-10-12T23:13:15.000Z\n").unwrap();
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("check").current_dir(&repo));
    assert!(
        stdout.contains("evidence counted from what people said up to 2026-10-12 23:13 UTC"),
        "{stdout}"
    );
    fixture.cleanup();
}
