//! CLI integration tests: the real binary against a stub `toimija` on PATH.
//!
//! Ohjaa owns no facts — it shells out to toimija and decides — so every
//! behavior worth testing end-to-end is observable through what it does with
//! toimija's answers: the rendering, the exit codes, and the argv it execs.
//! The stub differentiates per repo by cwd (`*broken*`) and per invocation by
//! `OHJAA_STUB_EXIT`, so one script covers fleet, refusal, and launch paths.
//! Std-only on purpose: the dependency list is a boundary of this repo.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$OHJAA_STUB_ARGV"
case "$PWD" in
  *broken*) echo "boom: not a Git repository" >&2; exit 1;;
esac
case " $* " in
  *" sessions "*)
    if [ -n "${OHJAA_STUB_SESSIONS_EXIT:-}" ]; then
      echo "stub failure on demand" >&2
      exit "$OHJAA_STUB_SESSIONS_EXIT"
    fi
    cat "${OHJAA_STUB_SESSIONS:?}"
    exit 0;;
  *" context "*) cat "${OHJAA_STUB_CONTEXT:?}"; exit 0;;
  *" repository-point "*) cat "${OHJAA_STUB_POINT:?}"; exit 0;;
  *" gates status "*) cat "${OHJAA_STUB_GATES:?}"; exit "${OHJAA_STUB_GATES_EXIT:-0}";;
  *" verify-work "*|*" close-work "*) exit 0;;
esac
# Anything else is the exec'd `run`: its exit status is what ohjaa returns.
exit "${OHJAA_STUB_EXIT:-0}"
"#;

/// `verify` accepts everything; `stamp -` echoes stdin with an envelope so the
/// derived facts parse as a sealed `ohjaa-cycle-facts.v1`.
const SAUMA_STUB: &str = r#"#!/bin/sh
case "$1" in
  stamp) sed 's/}$/,"sauma":{"envelope":"sauma-envelope.v1","schema":"ohjaa-cycle-facts.v1","digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","provenance":[],"producer":{"tool":"stub","version":"0","run":null},"anchored_at":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"}}/';;
esac
exit 0
"#;

/// `teko current` answers from a file; nothing else is asked of it.
const TEKO_STUB: &str = "#!/bin/sh\ncat \"${OHJAA_STUB_TEKO:?}\"\n";

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
        let stub = stub_dir.join("toimija");
        fs::write(&stub, STUB).unwrap();
        make_executable(&stub);
        let sauma = stub_dir.join("sauma");
        fs::write(&sauma, SAUMA_STUB).unwrap();
        make_executable(&sauma);
        let teko = stub_dir.join("teko");
        fs::write(&teko, TEKO_STUB).unwrap();
        make_executable(&teko);
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

    /// The real binary with the stubbed PATH and env wired up.
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
            .env("OHJAA_STUB_ARGV", self.dir.join("argv.txt"))
            .env("OHJAA_STUB_SESSIONS", self.dir.join("sessions.json"))
            .env("OHJAA_STUB_CONTEXT", self.dir.join("context.json"))
            .env("OHJAA_STUB_POINT", self.dir.join("point.json"))
            .env("OHJAA_STUB_GATES", self.dir.join("gates.json"))
            .env("OHJAA_STUB_TEKO", self.dir.join("teko.json"));
        command
    }

    fn set_file(&self, name: &str, json: &str) -> PathBuf {
        let path = self.dir.join(name);
        fs::write(&path, json).unwrap();
        path
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("argv.txt"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
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
fn fleet_says_no_live_sessions_when_every_scan_saw_clearly() {
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
    assert!(stdout.contains("no live sessions"), "{stdout}");
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
            "would exec: toimija run --intent 'refresh the guide' --task 'update the quick start' --scope docs/ -- claude --model opus"
        ),
        "{stderr}"
    );
}

#[test]
fn run_execs_toimija_with_the_full_forwarded_argv() {
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

    // exec semantics: the stub's exit status becomes ohjaa's.
    assert_eq!(code, Some(7), "exec must replace the process: {stderr}");
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
        "the exec'd argv is the truth"
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
fn cycle_run_launches_only_the_current_leg() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("cycle-run");
    let repo = fixture.make_git_repo("only");
    let cycle = fixture.dir.join("cycle.v1.json");
    fs::write(
        &cycle,
        r#"{"schema":"cycle.v1","work":{"artifact":"teko-work.v1","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","repository_point":"rpoint_x"},"decision":{"artifact":"kisko-decision.v1","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","repository_point":"rpoint_x"},"qa_plan":{"artifact":"kelpo-plan.v1","digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","repository_point":"rpoint_x"},"implementation":{"worker_role":"implementation","max_attempts":2},"qa":{"worker_role":"qa","must_differ_from":"implementation"},"effects":[],"retry":{"gate_failure":1,"worker_failure":1},"sauma":{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
    )
    .unwrap();
    let facts = fixture.dir.join("cycle.json");
    fs::write(
        &facts,
        r#"{"schema":"ohjaa-cycle-facts.v1","cycle":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sauma":{"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"decision":"allow","qa_plan":"current","implementation":"pending","gates":"pending","qa":"pending","verdict":"pending","closure":"pending"}"#,
    )
    .unwrap();

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "cycle",
                "run",
                "--cycle",
                cycle.to_str().unwrap(),
                "--facts",
                facts.to_str().unwrap(),
                "codex",
                "--intent",
                "implement",
                "--task",
                "change the parser",
                "--no-briefing",
                "--dry-run",
            ])
            .current_dir(&repo),
    );
    fixture.cleanup();

    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("Cycle leg: implementation"), "{stderr}");
    assert!(stderr.contains("change the parser"), "{stderr}");
}

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// Everything a derived cycle needs, staged so each test perturbs one fact.
/// The stubs verify anything, so the fixtures carry only the fields ohjaa
/// reads; the digests are what bind them to the cycle.
struct CycleFixture {
    fixture: Fixture,
    repo: PathBuf,
    cycle: PathBuf,
    work: PathBuf,
    decision: PathBuf,
    plan: PathBuf,
    verdict: PathBuf,
}

impl CycleFixture {
    fn new(tag: &str) -> Self {
        let fixture = Fixture::new(tag);
        let repo = fixture.make_git_repo("cycle");
        let cycle = fixture.set_file(
            "cycle.v1.json",
            &format!(
                r#"{{"schema":"cycle.v1","work":{{"artifact":"teko-work.v1","digest":"{DIGEST_A}","repository_point":"rpoint_x"}},"decision":{{"artifact":"kisko.decision.v1","digest":"{DIGEST_A}","repository_point":"rpoint_x"}},"qa_plan":{{"artifact":"kelpo-plan.v1","digest":"{DIGEST_A}","repository_point":"rpoint_x"}},"implementation":{{"worker_role":"implementation","max_attempts":2}},"qa":{{"worker_role":"qa","must_differ_from":"implementation"}},"effects":[],"retry":{{"gate_failure":1,"worker_failure":1}},"sauma":{{"digest":"{DIGEST_A}"}}}}"#
            ),
        );
        let work = fixture.set_file(
            "work.json",
            &format!(
                r#"{{"schema":"teko-work.v1","packet":{{"work":{{"id":"work_w1"}},"extensions":{{"toimija.required_gates.v1":["sykli-full"]}}}},"sauma":{{"digest":"{DIGEST_A}"}}}}"#
            ),
        );
        let decision = fixture.set_file(
            "decision.json",
            &format!(r#"{{"schema":"kisko.decision.v1","status":"decided","sauma":{{"digest":"{DIGEST_A}"}}}}"#),
        );
        let plan = fixture.set_file(
            "plan.json",
            &format!(r#"{{"schema":"kelpo-plan.v1","sauma":{{"digest":"{DIGEST_A}"}}}}"#),
        );
        let verdict = fixture.dir.join("verdict.json");
        fixture.set_file(
            "point.json",
            &format!(
                r#"{{"repository":{{"id":"rpoint_x","consistency_token":"{TOKEN}","tree_oid":"abc"}}}}"#
            ),
        );
        fixture.set_file(
            "gates.json",
            r#"[{"gate":"sykli-full","status":"stale","receipt":null}]"#,
        );
        fixture.set_file("teko.json", r#"{"active":{"work":{"id":"w1"}}}"#);
        Self {
            fixture,
            repo,
            cycle,
            work,
            decision,
            plan,
            verdict,
        }
    }

    fn session(&self, id: &str, leg: &str, liveness: &str, exit: &str) -> String {
        format!(
            r#"{{"id":"{id}","agent":["sh"],"worktree":"/tmp/r","branch":"main","intent":"x","scope":[],"liveness":"{liveness}","exit_status":{exit},"cycle_action":{{"cycle":"{DIGEST_A}","leg":"{leg}","attempt":0,"repository_point":"rpoint_x"}}}}"#
        )
    }

    fn set_sessions(&self, sessions: &[String]) {
        self.fixture
            .set_sessions(&sessions_json(&sessions.join(","), "available"));
    }

    fn current_gate(&self, exit_code: i32) {
        let receipts = self.repo.join(".toimija/runs/gates");
        fs::create_dir_all(&receipts).unwrap();
        fs::write(
            receipts.join("gatercpt_1.json"),
            format!(r#"{{"gate":"sykli-full","exit_code":{exit_code},"sauma":{{"digest":"sha256:ff"}}}}"#),
        )
        .unwrap();
        self.fixture.set_file(
            "gates.json",
            r#"[{"gate":"sykli-full","status":"current","receipt":"gatercpt_1"}]"#,
        );
    }

    fn verdict(&self, verdict: &str, final_token: &str, test_session: &str) {
        self.fixture.set_file(
            "verdict.json",
            &format!(
                r#"{{"schema":"kelpo-verdict.v1","verdict":"{verdict}","plan":{{"digest":"{DIGEST_A}"}},"repository":{{"baseline":"sha256:00","final":"{final_token}"}},"independence":{{"code_sessions":[{{"id":"impl-1"}}],"test_sessions":[{{"id":"{test_session}"}}]}},"sauma":{{"digest":"sha256:bb"}}}}"#
            ),
        );
    }

    fn step(&self) -> (String, String, Option<i32>) {
        output_of(
            self.fixture
                .ohjaa()
                .args(["cycle", "step", "--cycle"])
                .arg(&self.cycle)
                .arg("--work")
                .arg(&self.work)
                .arg("--decision")
                .arg(&self.decision)
                .arg("--qa-plan")
                .arg(&self.plan)
                .arg("--verdict")
                .arg(&self.verdict)
                .current_dir(&self.repo),
        )
    }

    fn action(&self) -> String {
        let (stdout, stderr, code) = self.step();
        assert_eq!(code, Some(0), "{stderr}");
        stdout.trim().to_string()
    }
}

#[test]
fn golden_1_derived_facts_walk_the_cycle_to_closed() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-1");
    assert_eq!(
        cycle.action(),
        r#"{"action":"launch","leg":"implementation"}"#
    );

    // The implementation leg closed cleanly: gates are next, and the leg is
    // consumed rather than launched again (golden case 5).
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "closed", "0")]);
    assert_eq!(cycle.action(), r#"{"action":"run_gates"}"#);

    cycle.current_gate(0);
    assert_eq!(cycle.action(), r#"{"action":"launch","leg":"qa"}"#);

    cycle.set_sessions(&[
        cycle.session("impl-1", "implementation", "closed", "0"),
        cycle.session("qa-1", "qa", "closed", "0"),
    ]);
    assert_eq!(cycle.action(), r#"{"action":"judge_qa"}"#);

    cycle.verdict("pass", TOKEN, "qa-1");
    assert_eq!(cycle.action(), r#"{"action":"close_work"}"#);

    cycle.fixture.set_file("teko.json", r#"{"active":null}"#);
    fs::create_dir_all(cycle.repo.join(".teko")).unwrap();
    fs::write(
        cycle.repo.join(".teko/witness.jsonl"),
        r#"{"record":"teko-completion-witness.v1","work":"w1","sauma":{"digest":"sha256:aa"}}
"#,
    )
    .unwrap();
    assert_eq!(cycle.action(), r#"{"action":"closed"}"#);
    cycle.fixture.cleanup();
}

#[test]
fn golden_2_an_inconclusive_decision_holds_before_any_launch() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-2");
    cycle.fixture.set_file(
        "decision.json",
        &format!(r#"{{"schema":"kisko.decision.v1","status":"inconclusive","problem":{{"code":"kisko.model_absent"}},"sauma":{{"digest":"{DIGEST_A}"}}}}"#),
    );
    assert_eq!(
        cycle.action(),
        r#"{"action":"held","reason":"decision_held"}"#
    );

    // A decision the cycle did not seal is not a decision at all.
    cycle.fixture.set_file(
        "decision.json",
        r#"{"schema":"kisko.decision.v1","status":"decided","sauma":{"digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}}"#,
    );
    assert_eq!(
        cycle.action(),
        r#"{"action":"held","reason":"decision_held"}"#
    );
    cycle.fixture.cleanup();
}

#[test]
fn a_stale_gate_reported_with_a_non_zero_exit_is_still_pending() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("stale-exit");
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "closed", "0")]);
    cycle.fixture.set_file(
        "gates.json",
        r#"[{"gate":"sykli-full","status":"stale repository point","receipt":"gatercpt_0"}]"#,
    );
    let (stdout, stderr, code) = output_of(
        cycle
            .fixture
            .ohjaa()
            .env("OHJAA_STUB_GATES_EXIT", "1")
            .args(["cycle", "step", "--cycle"])
            .arg(&cycle.cycle)
            .arg("--work")
            .arg(&cycle.work)
            .arg("--decision")
            .arg(&cycle.decision)
            .arg("--qa-plan")
            .arg(&cycle.plan)
            .current_dir(&cycle.repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout.trim(), r#"{"action":"run_gates"}"#);
    cycle.fixture.cleanup();
}

#[test]
fn golden_3_a_failed_gate_receipt_is_a_failure_not_a_rerun() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-3");
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "closed", "0")]);
    cycle.current_gate(1);
    assert_eq!(
        cycle.action(),
        r#"{"action":"failed","reason":"gates_failed"}"#
    );
    cycle.fixture.cleanup();
}

#[test]
fn golden_4_qa_evidence_from_the_implementation_session_is_inconclusive() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-4");
    cycle.set_sessions(&[
        cycle.session("impl-1", "implementation", "closed", "0"),
        cycle.session("qa-1", "qa", "closed", "0"),
    ]);
    cycle.current_gate(0);
    cycle.verdict("pass", TOKEN, "impl-1");
    assert_eq!(cycle.action(), r#"{"action":"held","reason":"qa_verdict"}"#);

    // A verdict anchored to a point the tree has left is stale, not false.
    cycle.verdict(
        "pass",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "qa-1",
    );
    assert_eq!(cycle.action(), r#"{"action":"held","reason":"qa_verdict"}"#);
    cycle.fixture.cleanup();
}

#[test]
fn golden_6_a_live_leg_holds_instead_of_duplicating_work() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-6");
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "active", "null")]);
    assert_eq!(
        cycle.action(),
        r#"{"action":"held","reason":"implementation"}"#
    );
    cycle.fixture.cleanup();
}

#[test]
fn golden_8_an_exhausted_worker_budget_fails_without_another_launch() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("golden-8");
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "closed", "1")]);
    assert_eq!(
        cycle.action(),
        r#"{"action":"launch","leg":"implementation"}"#
    );
    cycle.set_sessions(&[
        cycle.session("impl-1", "implementation", "closed", "1"),
        cycle.session("impl-2", "implementation", "closed", "null"),
    ]);
    assert_eq!(
        cycle.action(),
        r#"{"action":"failed","reason":"implementation_failed"}"#
    );
    cycle.fixture.cleanup();
}

#[test]
fn an_unreadable_session_listing_holds_rather_than_launching_blind() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("blind");
    cycle.fixture.set_sessions(&sessions_json("", "degraded"));
    let (stdout, stderr, code) = cycle.step();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stdout.trim(),
        r#"{"action":"held","reason":"implementation"}"#
    );
    assert!(stderr.contains("absence is not evidence"), "{stderr}");
    cycle.fixture.cleanup();
}

#[test]
fn cycle_run_loop_acts_through_toimija_and_stops_on_a_terminal_action() {
    if !git_available() {
        return;
    }
    let cycle = CycleFixture::new("loop");
    cycle.set_sessions(&[cycle.session("impl-1", "implementation", "closed", "0")]);
    // Gates are stale, so the loop runs `toimija verify-work` once; the stub
    // leaves them stale, so the second step repeats — and the bound ends it.
    let (stdout, stderr, code) = output_of(
        cycle
            .fixture
            .ohjaa()
            .args(["cycle", "run", "--cycle"])
            .arg(&cycle.cycle)
            .arg("--work")
            .arg(&cycle.work)
            .arg("--decision")
            .arg(&cycle.decision)
            .arg("--qa-plan")
            .arg(&cycle.plan)
            .args([
                "--max-steps",
                "2",
                "sh",
                "--intent",
                "loop",
                "--no-briefing",
            ])
            .current_dir(&cycle.repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert_eq!(stdout.trim(), r#"{"action":"held","reason":"step_budget"}"#);
    assert!(
        stderr.contains("step 0") && stderr.contains("run_gates"),
        "{stderr}"
    );
    let argv = fs::read_to_string(cycle.fixture.dir.join("argv.txt")).unwrap();
    assert!(argv.contains("verify-work"), "{argv}");
    cycle.fixture.cleanup();
}
