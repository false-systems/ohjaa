//! CLI integration tests: the real binary against a stub `toimija` on PATH, in
//! a sandboxed HOME and XDG config/state, so no test ever reads or writes the
//! person's real knowledge.
//!
//! The stub differentiates per repo by cwd (`*broken*`) and per invocation by
//! environment, so one script covers fleet, refusal and launch paths.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = r#"#!/bin/sh
if [ "$1" = run ]; then printf '%s\n' "$@" > "$OHJAA_STUB_ARGV"; fi
case "$PWD" in
  *broken*) echo "boom: not a Git repository" >&2; exit 1;;
esac
case "$1" in
  sessions)
    if [ -n "${OHJAA_STUB_SESSIONS_EXIT:-}" ]; then
      echo "stub failure on demand" >&2
      exit "$OHJAA_STUB_SESSIONS_EXIT"
    fi
    cat "${OHJAA_STUB_SESSIONS:?}"
    exit 0;;
  context) cat "${OHJAA_STUB_CONTEXT:?}"; exit 0;;
esac
# Anything else is `run`. With OHJAA_STUB_RUN_AGENT set it runs the agent
# command after `--`, as toimija would; its exit status is what ohjaa returns.
if [ -n "${OHJAA_STUB_RUN_AGENT:-}" ]; then
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do shift; done
  shift
  exec "$@"
fi
exit "${OHJAA_STUB_EXIT:-0}"
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

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
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

fn live_session() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    format!(
        "{{\"id\":\"9-9\",\"agent\":[\"codex\"],\"worktree\":\"/tmp/r\",\"branch\":\"main\",\
         \"intent\":\"change the parser\",\"scope\":[\"src/\"],\"liveness\":\"stale\",\
         \"last_seen_at\":{}}}",
        now - 600
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
        fs::write(&stub, STUB).unwrap();
        make_executable(&stub);
        // Never the real gh: no network, and a controlled answer.
        let gh = stub_dir.join("gh");
        fs::write(
            &gh,
            "#!/bin/sh\nif [ -n \"${OHJAA_STUB_GH_SLEEP:-}\" ]; then sleep \"$OHJAA_STUB_GH_SLEEP\"; fi\necho \"gh: To get started with GitHub CLI, please run:  gh auth login\" >&2\nexit 4\n",
        )
        .unwrap();
        make_executable(&gh);
        let fixture = Fixture { dir, stub_dir };
        // Default: an empty, fully available scan.
        fixture.set_sessions(&sessions_json("", "available"));
        fixture
    }

    fn set_sessions(&self, json: &str) {
        fs::write(self.dir.join("sessions.json"), json).unwrap();
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
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("OHJAA_OURS", self.dir.join("ours"))
            .env("OHJAA_TODAY", "2026-09-24")
            .env("OHJAA_STUB_ARGV", self.dir.join("argv.txt"))
            .env("OHJAA_STUB_SESSIONS", self.dir.join("sessions.json"))
            .env("OHJAA_STUB_CONTEXT", self.dir.join("context.json"));
        command
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("argv.txt"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
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

    fn cleanup(self) {
        fs::remove_dir_all(self.dir).ok();
    }
}

#[test]
fn fleet_renders_occupancy_and_exits_incomplete_when_a_scan_fails() {
    let fixture = Fixture::new("fleet");
    fixture.make_repo("alpha");
    fixture.make_repo("broken");
    fixture.set_sessions(&sessions_json(&live_session(), "available"));

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["fleet", "--root"])
            .arg(fixture.dir.clone()),
    );
    fixture.cleanup();

    assert_eq!(code, Some(4), "incomplete coverage must exit 4");
    assert!(stdout.contains("alpha"), "{stdout}");
    assert!(stdout.contains("codex  9-9"), "{stdout}");
    assert!(stdout.contains("stale"), "{stdout}");
    assert!(stdout.contains("intent: change the parser"), "{stdout}");
    // The broken repo is reported, not silently skipped.
    assert!(stderr.contains("coverage: scanned 1/2 repo(s)"), "{stderr}");
    assert!(stderr.contains("broken: boom"), "{stderr}");
}

#[test]
fn confirmed_dead_sessions_do_not_occupy_the_fleet_system_or_launch() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("dead-session");
    let repo = fixture.make_git_repo("only");
    fixture.set_context(r#"{"repo":{"name":"only","role":"test repo"}}"#);
    let mut dead: serde_json::Value = serde_json::from_str(&live_session()).unwrap();
    dead["pid_check"] = "dead".into();
    dead["intent"] = "obsolete session".into();
    fixture.set_sessions(&sessions_json(
        &format!("{dead},{}", live_session()),
        "available",
    ));
    for command in ["fleet", "system", "run"] {
        let mut cli = fixture.ohjaa();
        cli.arg(command);
        if command == "run" {
            cli.args(["codex", "--intent", "test", "--dry-run"]);
        }
        let (stdout, stderr, code) = output_of(cli.arg("--root").arg(&repo).current_dir(&repo));
        assert_eq!(code, Some(0), "{stderr}");
        let text = format!("{stdout}\n{stderr}");
        assert!(!text.contains("obsolete session"), "{text}");
        assert!(text.contains("change the parser"), "{text}");
        if command == "run" {
            assert!(stderr.contains("1 sibling session(s)"), "{stderr}");
        }
    }
    fixture.cleanup();
}

#[test]
fn fleet_reports_no_non_closed_sessions_when_every_scan_saw_clearly() {
    let fixture = Fixture::new("quiet");
    fixture.make_repo("alpha");
    fixture.set_sessions(&sessions_json("", "available"));

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["fleet", "--root"])
            .arg(fixture.dir.clone()),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("no possibly running sessions reported"),
        "{stdout}"
    );
    assert!(stderr.contains("coverage: scanned 1/1 repo(s)"), "{stderr}");
    assert!(!stderr.contains("incomplete"), "{stderr}");
}

#[test]
fn run_refuses_to_launch_when_the_scan_failed() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("refuse-failed");
    let repo = fixture.make_git_repo("only");

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .env("OHJAA_STUB_SESSIONS_EXIT", "1")
            .args(["run", "claude", "--intent", "x", "--dry-run"])
            .current_dir(&repo),
    );
    fixture.cleanup();

    assert_eq!(code, Some(3), "a blind launch is refused with 3");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("refusing to launch blind"), "{stderr}");
    assert!(stderr.contains("stub failure on demand"), "{stderr}");
}

#[test]
fn run_refuses_to_launch_on_degraded_coverage() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("refuse-degraded");
    let repo = fixture.make_git_repo("only");
    fixture.set_sessions(&sessions_json("", "degraded"));

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "x", "--dry-run"])
            .current_dir(&repo),
    );
    fixture.cleanup();

    // Degraded saw less than everything; its silence is not evidence either.
    assert_eq!(code, Some(3), "degraded coverage must also refuse");
    assert!(stderr.contains("refusing to launch blind"), "{stderr}");
    assert!(stderr.contains("degraded coverage"), "{stderr}");
}

#[test]
fn run_dry_run_prints_the_composed_toimija_command() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("dry-run");
    let repo = fixture.make_git_repo("only");

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "claude",
                "--intent",
                "refresh the guide",
                "--task",
                "update the quick start",
                "--scope",
                "docs/",
                "--dry-run",
                "--no-briefing",
                "--",
                "--model",
                "opus",
            ])
            .current_dir(&repo),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0));
    assert!(stdout.is_empty(), "dry-run must not launch: {stdout}");
    assert!(
        stderr.contains("0 sibling session(s) in this checkout at launch"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "would run: toimija run --intent 'refresh the guide' --task 'update the quick start' --scope docs/ -- claude --model opus"
        ),
        "{stderr}"
    );
}

#[test]
fn run_launches_toimija_with_the_full_forwarded_argv() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("exec");
    let repo = fixture.make_git_repo("only");

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "codex",
                "--intent",
                "work on ahti",
                "--task",
                "fix the lexer",
                "--scope",
                "src/parser/",
                "--no-briefing",
                "--",
                "--model",
                "o3",
            ])
            .env("OHJAA_STUB_EXIT", "7")
            .current_dir(&repo),
    );
    let argv = fixture.argv();
    fixture.cleanup();

    // The session's exit status becomes ohjaa's.
    assert_eq!(code, Some(7), "{stderr}");
    assert_eq!(
        argv,
        vec![
            "run",
            "--intent",
            "work on ahti",
            "--task",
            "fix the lexer",
            "--scope",
            "src/parser/",
            "--",
            "codex",
            "--model",
            "o3",
        ],
        "the launched argv is the truth"
    );
}

#[test]
fn run_warns_when_an_ohjaa_flag_hides_after_the_separator() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("misplaced");
    let repo = fixture.make_git_repo("only");

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "codex",
                "--intent",
                "x",
                "--no-briefing",
                "--",
                "--dry-run",
            ])
            .env("OHJAA_STUB_EXIT", "9")
            .current_dir(&repo),
    );
    fixture.cleanup();

    // The launch still happens (the flag belongs to the agent now) — but it
    // cannot happen silently.
    assert_eq!(code, Some(9), "exec semantics: {stderr}");
    assert!(
        stderr.contains("`--dry-run` after `--` was passed to the agent, not ohjaa"),
        "{stderr}"
    );
}

#[test]
fn system_derives_the_fleet_and_exits_incomplete_when_a_role_is_missing() {
    let fixture = Fixture::new("system");
    fixture.make_repo("teko");
    fixture.make_repo("kelo");
    fixture.set_context(
        r#"{"repo":{"name":"teko","role":"durable task contract","not_roles":["inbox"]}}"#,
    );
    // One canned context for every repo: teko parses, the "kelo" copy does
    // not (its name mismatches nothing — the stub serves the same file, so
    // kelo derives with the wrong name and no role of its own). To keep the
    // test honest, give kelo an empty repo object instead.
    fs::write(
        fixture.dir.join("context.json"),
        r#"{"repo":{"name":"teko","role":"durable task contract","not_roles":["inbox"]}}"#,
    )
    .unwrap();

    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["system", "--root"])
            .arg(fixture.dir.clone()),
    );
    fixture.cleanup();

    assert!(stdout.contains("teko"), "{stdout}");
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
    fixture.make_repo("teko");
    fixture
        .set_context(r#"{"repo":{"name":"teko","role":"durable task contract","not_roles":[]}}"#);

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
    assert_eq!(tool["name"], "teko");
    assert_eq!(tool["role"], "durable task contract");
}

#[test]
fn run_prepends_the_system_perimeter_to_the_task() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("briefing");
    let repo = fixture.make_git_repo("toimija");
    fixture.set_context(
        r#"{"repo":{"name":"toimija","role":"compiles Git context for agents","not_roles":[]}}"#,
    );

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "codex",
                "--intent",
                "x",
                "--task",
                "fix the lexer",
                "--dry-run",
            ])
            .current_dir(&repo),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0));
    // The task is user task + perimeter: the agent starts knowing the fleet.
    assert!(stderr.contains("fix the lexer"), "{stderr}");
    assert!(stderr.contains("System perimeter"), "{stderr}");
    assert!(
        stderr.contains("toimija — compiles Git context for agents"),
        "{stderr}"
    );
    assert!(stderr.contains("you are here"), "{stderr}");
}

#[test]
fn run_no_briefing_leaves_the_task_alone() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("no-briefing");
    let repo = fixture.make_git_repo("toimija");
    fixture.set_context(r#"{"repo":{"name":"toimija","role":"r","not_roles":[]}}"#);

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "codex",
                "--intent",
                "x",
                "--task",
                "fix the lexer",
                "--no-briefing",
                "--dry-run",
            ])
            .current_dir(&repo),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0));
    assert!(stderr.contains("fix the lexer"), "{stderr}");
    assert!(!stderr.contains("System perimeter"), "{stderr}");
}

#[test]
fn a_missing_root_is_an_error_not_an_empty_fleet() {
    let fixture = Fixture::new("noroot");
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["fleet", "--root"])
            .arg(fixture.dir.join("does-not-exist")),
    );
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("is not a directory"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn run_refuses_when_toimija_does_not_report_coverage() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("nocoverage");
    let repo = fixture.make_git_repo("work");
    fixture.set_sessions(r#"{"error":"store locked"}"#);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "codex", "--intent", "x", "--dry-run"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert!(
        stderr.contains("did not report its session coverage"),
        "{stderr}"
    );
    fixture.cleanup();
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
        "Dima owns the uploader.",
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
        "Dima owns",
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

#[test]
fn run_briefs_knowledge_before_the_perimeter() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("runbrief");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "rust",
        "decision",
        "Rust only.",
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "codex",
                "--intent",
                "x",
                "--task",
                "fix the lexer",
                "--dry-run",
            ])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let task = stderr.find("fix the lexer").expect("task");
    let knowledge = stderr.find("Rust only.").expect("knowledge in the task");
    let perimeter = stderr.find("System perimeter").expect("perimeter");
    assert!(task < knowledge && knowledge < perimeter, "{stderr}");
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
    let project = fixture.dir.join("home/.claude/projects/-work");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("session.jsonl"),
        [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Linux tests are blocked on macOS."}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-20T10:00:00Z","cwd":"/p/work","message":{"content":"we have lima baba"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();
    let drafter = fixture.stub_dir.join("drafter");
    fs::write(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\ncat <<'JSON'\nSure:\n[{{\"scope\":\"machine\",\"repo\":null,\"id\":\"lima-for-linux\",\
             \"kind\":\"fact\",\"body\":\"Linux tests run in Lima.\",\"origin\":[\"2026-09-20 work: 'we have lima baba'\"],\
             \"refines\":null,\"supersedes\":[]}},{{\"scope\":\"nowhere\",\"id\":\"bad\",\"kind\":\"fact\",\"body\":\"x\"}}]\nJSON\n",
            fixture.dir.join("prompt.txt").display()
        ),
    )
    .unwrap();
    make_executable(&drafter);

    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "intake",
                "--source",
                "transcripts",
                "--since",
                "2026-09-01",
                "--drafter",
            ])
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
            .args(["intake", "--source", "transcripts", "--drafter"])
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
    let project = fixture.dir.join("home/.claude/projects/-work");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("s.jsonl"),
        r#"{"type":"user","timestamp":"2026-09-20T10:00:00Z","message":{"content":"never python"}}"#,
    )
    .unwrap();
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
    assert!(stderr.contains("intake stopped"), "{stderr}");
    assert!(!fixture.dir.join("state/ohjaa/intake-since").exists());
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
fn a_deleted_configured_root_is_skipped_not_fatal() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("staleroot");
    let repo = fixture.make_git_repo("work");
    let config = fixture.dir.join("config/ohjaa");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("roots"),
        format!(
            "{}\n{}\n",
            fixture.dir.join("gone").display(),
            fixture.dir.display()
        ),
    )
    .unwrap();
    let (stdout, stderr, code) = output_of(fixture.ohjaa().arg("fleet").current_dir(&repo));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains("skipping root") && stderr.contains("gone"),
        "{stderr}"
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

// ---- the session lifecycle ---------------------------------------------------

fn ohjaa_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ohjaa")
}

/// `ohjaa run` whose stub toimija really runs `sh -c <script>` as the agent.
fn run_session(fixture: &Fixture, repo: &Path, script: &str) -> (String, String, Option<i32>) {
    output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "retry uploads",
                "--no-briefing",
                "--",
                "-c",
                script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(repo),
    )
}

#[test]
fn leaving_always_writes_a_record_even_when_the_agent_says_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("silent");
    let repo = fixture.make_git_repo("work");
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 3");
    assert_eq!(code, Some(3), "the session's exit is ohjaa's: {stderr}");
    for said in [
        "ended: exit 3",
        "─ observed",
        "no read-back",
        "phases        no commits",
        "─ handover",
        "none written",
        "queued for `ohjaa intake`",
    ] {
        assert!(stderr.contains(said), "missing `{said}`: {stderr}");
    }
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("session").current_dir(&repo));
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("ended: exit 3") && stdout.contains("none written"),
        "{stdout}"
    );
    fixture.cleanup();
}

#[test]
fn the_agent_reads_back_and_hands_over_from_inside_its_session() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("testimony");
    let repo = fixture.make_git_repo("work");
    let script = format!(
        "{o} oriented 'goal: retry uploads; rules: rust only; nobody else here' && \
         {o} handover --unfinished 'windows paths' --surprised 'the fixture skips without git'",
        o = ohjaa_bin()
    );
    let (stdout, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("recorded the read-back"), "{stdout}");
    assert!(stderr.contains("read back at"), "{stderr}");
    assert!(stderr.contains("unfinished    windows paths"), "{stderr}");
    assert!(
        stderr.contains("surprised by  the fixture skips without git"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn session_names_the_open_session_instead_of_saying_none() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("open-session");
    let repo = fixture.make_git_repo("work");
    let (stdout, _, code) = output_of(fixture.ohjaa().arg("session").current_dir(&repo));
    assert_eq!(code, Some(1));
    assert_eq!(stdout, "no session recorded here\n");

    // From inside a running session, nothing here has ended yet, but this
    // session is recorded, and is named with its id.
    let script = format!("{} session; echo \"id=$OHJAA_SESSION\"", ohjaa_bin());
    let (stdout, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    let id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("id="))
        .expect(&stdout);
    assert!(stdout.contains("no session here has ended yet"), "{stdout}");
    assert!(
        stdout.contains(&format!("  {id} · sh · started ")) && stdout.contains(" · retry uploads"),
        "{stdout}"
    );
    assert!(!stdout.contains("no session recorded"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn testimony_outside_a_session_is_refused() {
    let fixture = Fixture::new("nosession");
    for args in [vec!["oriented", "I know"], vec!["handover", "--next", "x"]] {
        let (_, stderr, code) = output_of(fixture.ohjaa().args(&args).env_remove("OHJAA_SESSION"));
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains("not inside an ohjaa session"), "{stderr}");
    }
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["oriented", "x"])
            .env("OHJAA_SESSION", "../../x"),
    );
    assert_eq!(code, Some(1));
    assert!(stderr.contains("is not a session id"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn phases_are_observed_from_commits_and_their_sykli_receipts() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("phases");
    let repo = fixture.make_git_repo("work");
    // Two phases; sykli ran on the first only. The receipt is written the way
    // sykli writes it: keyed by the tree HEAD had when it ran.
    let script = r#"
        set -e
        echo one > a.txt && git add a.txt && git commit -q -m 'first phase'
        tree=$(git rev-parse HEAD^{tree})
        mkdir -p .sykli/receipts
        printf '{"schema":"sykli-receipt.v1","contract_hash":"c","started_at_ms":1,"subject":{"repository":"r","tree_oid":"%s","inputs_digest":"d","head_tree_oid":"%s","dirty":false},"tasks":[],"outcome":"passed"}' "$tree" "$tree" > .sykli/receipts/rcpt_1.json
        echo two > b.txt && git add b.txt && git commit -q -m 'second phase'
    "#;
    let (_, stderr, code) = run_session(&fixture, &repo, script);
    assert_eq!(code, Some(0), "{stderr}");
    let phases = stderr
        .lines()
        .find(|l| l.starts_with("phases"))
        .expect(&stderr);
    assert!(
        phases.contains(" passed · ") && phases.ends_with("no sykli run"),
        "{phases}"
    );
    fixture.cleanup();
}

#[test]
fn the_next_briefing_says_what_the_last_session_left() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("next");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let script = format!("{} handover --unfinished 'windows paths'", ohjaa_bin());
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");

    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(stdout.contains("Last session here (oh-"), "{stdout}");
    assert!(stdout.contains("- unfinished: windows paths"), "{stdout}");
    // And the next agent is told it, and told how its own session works.
    let (_, stderr, _) = output_of(
        fixture
            .ohjaa()
            .args(["run", "codex", "--intent", "next", "--dry-run"])
            .current_dir(&repo),
    );
    assert!(stderr.contains("Last session here"), "{stderr}");
    assert!(stderr.contains("ohjaa oriented"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn the_briefing_asks_for_sykli_run_only_where_there_is_a_contract() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sykli-step");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let briefing = || {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["run", "codex", "--intent", "x", "--dry-run"])
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        stderr
    };
    let without = briefing();
    assert!(
        without.contains("There is no sykli contract")
            && !without.contains("with a commit and `sykli run`"),
        "{without}"
    );
    fs::write(repo.join("sykli.rs"), "fn main() {}\n").unwrap();
    let with = briefing();
    assert!(with.contains("with a commit and `sykli run`."), "{with}");
    assert!(!with.contains("There is no sykli contract"), "{with}");
    fixture.cleanup();
}

#[test]
fn a_broken_session_log_never_blocks_a_launch_and_is_left_untouched() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("badlog");
    let repo = fixture.make_git_repo("work");
    let log = fixture.dir.join("state/ohjaa/sessions.redb");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(&log, b"").unwrap();
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 0");
    assert_eq!(code, Some(0), "the agent still ran: {stderr}");
    assert!(
        stderr.contains("is unreadable") && stderr.contains("carrying on"),
        "{stderr}"
    );
    assert!(
        stderr.contains("could not be saved; it is shown above only"),
        "{stderr}"
    );
    assert_eq!(fs::metadata(&log).unwrap().len(), 0, "never replaced");
    fixture.cleanup();
}

/// Spawn `ohjaa run` whose agent runs `script`, and return once the agent is
/// running (it touches a mark first), so a signal lands while ohjaa waits.
fn spawn_session(fixture: &Fixture, repo: &Path, script: &str) -> std::process::Child {
    let mark = fixture.dir.join(format!(
        "agent-started-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let child = fixture
        .ohjaa()
        .args(["run", "sh", "--intent", "long", "--no-briefing", "--", "-c"])
        .arg(format!("touch '{}'; {script}", mark.display()))
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .current_dir(repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !mark.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the agent never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child
}

fn signal(pid: u32, name: &str) {
    assert!(Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .status()
        .unwrap()
        .success());
}

#[test]
fn sigterm_to_ohjaa_reaches_the_session_and_leaving_still_runs() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sigterm");
    let repo = fixture.make_git_repo("work");
    let started = std::time::Instant::now();
    let child = spawn_session(&fixture, &repo, "exec sleep 30");
    std::thread::sleep(std::time::Duration::from_millis(800));
    signal(child.id(), "TERM");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the session was not stopped"
    );
    assert_eq!(output.status.code(), Some(143), "{stderr}");
    assert!(stderr.contains("ended: killed by signal 15"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn sigint_aimed_at_ohjaa_alone_does_not_cut_the_session_short() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sigint");
    let repo = fixture.make_git_repo("work");
    let child = spawn_session(&fixture, &repo, "sleep 1; exit 4");
    std::thread::sleep(std::time::Duration::from_millis(300));
    signal(child.id(), "INT");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(4),
        "ohjaa waited for the session: {stderr}"
    );
    assert!(stderr.contains("ended: exit 4"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn graph_shows_a_nodes_neighbours_and_the_sessions_briefed_with_it() {
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
    // A briefed session, so the graph can say who read it.
    let (_, stderr, _) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "x", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    let (stdout, _, code) = output_of(fixture.ohjaa().args(["graph", "rust"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("builds on     lean"), "{stdout}");
    assert!(stdout.contains("replaced      go-ok"), "{stdout}");
    assert!(stdout.contains("briefed in    1 session(s)"), "{stdout}");
    let (stdout, _, _) = output_of(fixture.ohjaa().args(["graph", "lean"]).current_dir(&repo));
    assert!(stdout.contains("built on by   rust"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn intake_drafts_from_a_sessions_handover_once() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sessionintake");
    let repo = fixture.make_git_repo("work");
    let script = format!(
        "{} handover --surprised 'the fixture helper skips without git'",
        ohjaa_bin()
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");

    let drafter = fixture.stub_dir.join("drafter");
    fs::write(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    )
    .unwrap();
    make_executable(&drafter);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--source", "transcripts", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let prompt = fs::read_to_string(fixture.dir.join("prompt.txt")).unwrap();
    assert!(
        prompt.contains("surprised by: the fixture helper skips without git"),
        "{prompt}"
    );
    assert!(prompt.contains("its handover"), "{prompt}");

    // Drafted once: the next intake does not bring it back.
    fs::remove_file(fixture.dir.join("prompt.txt")).unwrap();
    let (stdout, stderr, _) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--source", "transcripts", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert!(!fixture.dir.join("prompt.txt").exists(), "{stdout}{stderr}");
    fixture.cleanup();
}

// ---- review fixes: each test failed before its fix -----------------------------

/// A drafter that, while intake is drafting, checks the session log can be
/// opened by another ohjaa (as `oriented`, `handover` and leaving must).
fn drafter_that_opens_the_log(fixture: &Fixture) -> PathBuf {
    let drafter = fixture.stub_dir.join("drafter-opens-log");
    fs::write(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {prompt}\n{ohjaa} session > {during} 2>&1\necho \"exit $?\" >> {during}\necho '[]'\n",
            prompt = fixture.dir.join("prompt.txt").display(),
            ohjaa = ohjaa_bin(),
            during = fixture.dir.join("during.txt").display(),
        ),
    )
    .unwrap();
    make_executable(&drafter);
    drafter
}

#[test]
fn intake_never_holds_the_session_log_while_the_drafter_runs() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("heldlog");
    let repo = fixture.make_git_repo("work");
    let script = format!("{} handover --next 'x'", ohjaa_bin());
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    let drafter = drafter_that_opens_the_log(&fixture);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["intake", "--source", "transcripts", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let during = fs::read_to_string(fixture.dir.join("during.txt")).unwrap();
    assert!(
        during.contains("ended:") && during.ends_with("exit 0\n"),
        "{during}"
    );
    fixture.cleanup();
}

fn wait_with_deadline(
    child: std::process::Child,
    limit: std::time::Duration,
) -> (String, Option<i32>, std::time::Duration) {
    let started = std::time::Instant::now();
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < limit,
        "took {elapsed:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
        elapsed,
    )
}

#[test]
fn sigquit_and_sighup_to_ohjaa_reach_the_session_and_leaving_still_runs() {
    if !git_available() {
        return;
    }
    for (name, number) in [("QUIT", 3), ("HUP", 1)] {
        let fixture = Fixture::new(&format!("sig{name}"));
        let repo = fixture.make_git_repo("work");
        let child = spawn_session(&fixture, &repo, "exec sleep 10");
        std::thread::sleep(std::time::Duration::from_millis(800));
        signal(child.id(), name);
        let (stderr, code, _) = wait_with_deadline(child, std::time::Duration::from_secs(7));
        assert_eq!(code, Some(128 + number), "{name}: {stderr}");
        assert!(
            stderr.contains(&format!("ended: killed by signal {number}")),
            "{name}: {stderr}"
        );
        fixture.cleanup();
    }
}

#[test]
fn a_signal_during_leaving_cuts_it_short_and_the_record_is_still_written() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("leavingint");
    let repo = fixture.make_git_repo("work");
    let mark = fixture.dir.join("agent-done");
    let child = fixture
        .ohjaa()
        .args([
            "run",
            "sh",
            "--intent",
            "slow leaving",
            "--no-briefing",
            "--",
            "-c",
        ])
        .arg(format!("touch '{}'; exit 0", mark.display()))
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .env("OHJAA_STUB_GH_SLEEP", "30")
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !mark.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(1200));
    signal(child.id(), "INT");
    let (stderr, code, _) = wait_with_deadline(child, std::time::Duration::from_secs(8));
    assert_eq!(code, Some(0), "the session itself ended cleanly: {stderr}");
    assert!(
        stderr.contains("finished      unknown (interrupted)"),
        "{stderr}"
    );
    assert!(stderr.contains("recorded; queued"), "{stderr}");
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("session").current_dir(&repo));
    assert!(
        stdout.contains("unknown (interrupted)"),
        "the record was saved: {stdout}"
    );
    fixture.cleanup();
}

#[test]
fn sessions_are_drafted_only_once_their_transcript_window_was_read() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("draftedwhen");
    let repo = fixture.make_git_repo("work");
    let script = format!("{} handover --surprised 'the handover text'", ohjaa_bin());
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    let drafter = fixture.stub_dir.join("drafter");
    fs::write(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    )
    .unwrap();
    make_executable(&drafter);
    let intake = |source: &str| {
        let _ = fs::remove_file(fixture.dir.join("prompt.txt"));
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", source, "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        );
        fs::read_to_string(fixture.dir.join("prompt.txt")).unwrap_or_default()
    };
    // A run that never reads transcripts leaves the session alone and queued...
    let prompt = intake("reviews");
    assert!(
        !prompt.contains("the handover text"),
        "a session is drafted whole, with its transcript window: {prompt}"
    );
    // ...and so does one whose transcripts could not all be read.
    let projects = fixture.dir.join("home/.claude/projects/-work");
    fs::create_dir_all(&projects).unwrap();
    let unreadable = projects.join("locked.jsonl");
    fs::write(&unreadable, "{}").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let prompt = intake("transcripts");
    assert!(prompt.contains("the handover text"), "{prompt}");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    // Read cleanly: drafted, and only now marked so.
    let prompt = intake("transcripts");
    assert!(
        prompt.contains("the handover text"),
        "still queued after a warning: {prompt}"
    );
    let prompt = intake("transcripts");
    assert!(
        !prompt.contains("the handover text"),
        "drafted once: {prompt}"
    );
    fixture.cleanup();
}

#[test]
fn last_session_here_is_the_latest_ended_one_never_a_live_session() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("lastended");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let first = format!(
        "{} handover --unfinished 'from the first session'",
        ohjaa_bin()
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &first);
    assert_eq!(code, Some(0), "{stderr}");
    let seen = fixture.dir.join("brief-inside.txt");
    let second = format!("{} brief > '{}' 2>&1", ohjaa_bin(), seen.display());
    let (_, stderr, code) = run_session(&fixture, &repo, &second);
    assert_eq!(code, Some(0), "{stderr}");
    let inside = fs::read_to_string(&seen).unwrap();
    assert!(
        inside.contains("- unfinished: from the first session"),
        "{inside}"
    );
    assert!(
        !inside.contains("did not record its leaving"),
        "a live session is not a crash: {inside}"
    );
    fixture.cleanup();
}

#[test]
fn a_second_handover_changes_only_the_fields_it_names() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("mergehandover");
    let repo = fixture.make_git_repo("work");
    let script = format!(
        "{o} handover --unfinished 'windows paths' --surprised 'old surprise' && \
         {o} handover --next 'ask Dima' --surprised ''",
        o = ohjaa_bin()
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("unfinished    windows paths"),
        "kept: {stderr}"
    );
    assert!(stderr.contains("next agent    ask Dima"), "added: {stderr}");
    assert!(
        !stderr.contains("old surprise"),
        "cleared with '': {stderr}"
    );
    fixture.cleanup();
}
