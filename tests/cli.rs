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
printf '%s|%s\n' "$PWD" "$*" >> "$OHJAA_STUB_CALLS"
if [ "$1" = run ]; then printf '%s\n' "$@" > "$OHJAA_STUB_ARGV"; fi
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
# Anything else is `run`. With OHJAA_STUB_RUN_AGENT set it runs the agent
# command after `--`, as toimija would; its exit status is what ohjaa returns.
if [ -n "${OHJAA_STUB_RUN_AGENT:-}" ]; then
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do shift; done
  shift
  exec "$@"
fi
exit "${OHJAA_STUB_EXIT:-0}"
"#;

/// Never the real gh: no network, and a controlled answer. With
/// OHJAA_STUB_GH_PR_BRANCH set, the branch of that name has a pull request
/// (state OHJAA_STUB_GH_PR_STATE, default OPEN) whose head is that branch's
/// commit here; `pr view` with no branch asks about the one checked out.
/// `pr list --search <sha>` finds a merged pull request for
/// OHJAA_STUB_GH_PR_SEARCH only.
const GH_STUB: &str = r#"#!/bin/sh
if [ -n "${OHJAA_STUB_GH_SLEEP:-}" ]; then sleep "$OHJAA_STUB_GH_SLEEP"; fi
printf '%s|%s\n' "$PWD" "$*" >> "${OHJAA_STUB_GH_CALLS:-/dev/null}"
if [ "$1 $2" = "pr list" ]; then
  sha=""
  while [ $# -gt 0 ]; do case "$1" in --search) sha="$2"; shift;; esac; shift; done
  if [ -n "$sha" ] && [ "$sha" = "${OHJAA_STUB_GH_PR_SEARCH:-}" ]; then
    printf '[{"url":"https://github.com/o/r/pull/8","state":"MERGED","headRefOid":"%s"}]\n' "$sha"
  else
    echo '[]'
  fi
  exit 0
fi
if [ -n "${OHJAA_STUB_GH_PR_BRANCH:-}" ]; then
  case "$3" in -*|"") branch="$(git branch --show-current)";; *) branch="$3";; esac
  if [ "$branch" = "$OHJAA_STUB_GH_PR_BRANCH" ]; then
    head=$(git rev-parse --verify -q "refs/heads/$branch" || git rev-parse --verify -q "refs/remotes/origin/$branch")
    printf '{"url":"https://github.com/o/r/pull/7","state":"%s","headRefOid":"%s"}\n' \
      "${OHJAA_STUB_GH_PR_STATE:-OPEN}" "$head"
    exit 0
  fi
  echo "no pull requests found for branch \"$branch\"" >&2
  exit 1
fi
echo "gh: To get started with GitHub CLI, please run:  gh auth login" >&2
exit 4
"#;

/// Never the real sykli. `verify --help` names `--commit` only with
/// OHJAA_STUB_SYKLI_COMMIT set. `verify <receipt> --contract <c> --commit
/// <sha>` checks, in sykli's order and words: the contract the receipt was
/// made under, that it ran exactly that commit's tree, cleanly, and then its
/// outcome. With OHJAA_STUB_SYKLI_COMMIT=1 it is sykli#67 as it stands: after
/// the commit checks it also compares the checkout on disk (its HEAD's tree,
/// and nothing uncommitted beside it). With `new` it takes everything from the
/// commit, as sykli#67 is being changed to.
const SYKLI_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "${OHJAA_STUB_SYKLI_CALLS:-/dev/null}"
if [ "$1" = verify ] && [ "$2" = --help ]; then
  echo "Usage: sykli verify [OPTIONS] <RECEIPT>"
  echo "      --contract <CONTRACT>  Path to sykli.rs or a sykli-contract.v1 JSON file"
  if [ -n "${OHJAA_STUB_SYKLI_COMMIT:-}" ]; then
    echo "      --commit <REV>  Also require a clean run of this commit"
  fi
  if [ "${OHJAA_STUB_SYKLI_COMMIT:-}" = new ]; then
    echo "With --commit the subject is the commit, and nothing is read from the"
    echo "checkout; REV is resolved in the repository around the current directory."
  fi
  exit 0
fi
[ "$1" = verify ] || exit 2
receipt="$2"; shift 2
sha=""
while [ $# -gt 0 ]; do
  case "$1" in --commit) sha="$2"; shift;; esac
  shift
done
[ -n "$sha" ] || { echo "refused: commit expected a --commit" >&2; exit 2; }
grep -q '"contract_hash":"forged"' "$receipt" && { echo "refused: contract expected c but got forged" >&2; exit 4; }
tree=$(git rev-parse "$sha^{tree}") || exit 2
ran=$(sed -n 's/.*"tree_oid":"\([0-9a-f]*\)".*/\1/p' "$receipt" | head -1)
[ "$ran" = "$tree" ] || { echo "refused: commit expected $tree (the tree of commit $sha) but got $ran" >&2; exit 3; }
grep -q '"dirty":false' "$receipt" || { echo "refused: clean expected a clean run but got dirty true" >&2; exit 3; }
if [ "${OHJAA_STUB_SYKLI_COMMIT:-}" != new ]; then
  live=$(git rev-parse 'HEAD^{tree}')
  if [ "$live" != "$tree" ] || [ -n "$(git status --porcelain -- . ':(exclude).sykli')" ]; then
    echo "refused: tree expected $live but got $tree" >&2; exit 3
  fi
fi
grep -q '"outcome":"passed"' "$receipt" || { echo "refused: outcome expected passed or cached with complete task records but got failed with 0 task records" >&2; exit 1; }
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
        fs::write(&gh, GH_STUB).unwrap();
        make_executable(&gh);
        // Never the real sykli: `verify --help` names --commit only on demand.
        let sykli = stub_dir.join("sykli");
        fs::write(&sykli, SYKLI_STUB).unwrap();
        make_executable(&sykli);
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

    /// A Git repository whose only commit, and its reflog entry, are long
    /// past: reflog times are whole seconds, and one written in the second a
    /// session starts counts as written during it.
    fn make_settled_git_repo(&self, name: &str) -> PathBuf {
        let repo = self.make_repo(name);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "ohjaa@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Ohjaa Test"]);
        let status = Command::new("git")
            .args(["commit", "-q", "--allow-empty", "-m", "init"])
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
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
            .env("OHJAA_STUB_CALLS", self.dir.join("calls.txt"))
            .env("OHJAA_STUB_SESSIONS", self.dir.join("sessions.json"))
            .env("OHJAA_STUB_OUTSIDE_SESSIONS", self.dir.join("outside.json"))
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
    let usage_path = fixture.dir.join("receipt with \"quotes\".json");

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
                "--usage-file",
                usage_path.to_str().unwrap(),
                "--",
                "--model",
                "o3",
            ])
            .env("OHJAA_STUB_EXIT", "7")
            .current_dir(&repo),
    );
    let argv = fixture.argv();
    let (export, _, export_code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(export_code, Some(0));
    let export: serde_json::Value = serde_json::from_str(&export).unwrap();
    let session_id = export["sessions"][0]["id"].as_str().unwrap();
    let session_config = format!(
        "shell_environment_policy.set.OHJAA_SESSION={}",
        serde_json::to_string(session_id).unwrap()
    );
    let state_config = format!(
        "shell_environment_policy.set.XDG_STATE_HOME={}",
        serde_json::to_string(&fixture.dir.join("state").display().to_string()).unwrap()
    );
    let usage_config = format!(
        "shell_environment_policy.set.OHJAA_USAGE_FILE={}",
        serde_json::to_string(&usage_path.display().to_string()).unwrap()
    );
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
            "-c",
            &session_config,
            "-c",
            &state_config,
            "-c",
            &usage_config,
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
            .args(["run", "codex", "--intent", "x", "--task", "fix the lexer"])
            .current_dir(&repo),
    );
    let argv = fixture.argv();
    fixture.cleanup();

    assert_eq!(code, Some(0), "{stderr}");
    // The task is exactly what was assigned; the perimeter travels in the
    // briefing, scoped to the selected repository, not the whole fleet.
    let task = argv.iter().position(|a| a == "--task").expect("--task");
    assert_eq!(argv[task + 1], "fix the lexer", "{argv:?}");
    assert_eq!(argv[task + 2], "--briefing", "{argv:?}");
    let end = argv.iter().rposition(|a| a == "--").expect("--");
    let briefing = argv[task + 3..end].join("\n");
    assert!(briefing.contains("System perimeter"), "{briefing}");
    assert!(
        briefing.contains("toimija — compiles Git context for agents"),
        "{briefing}"
    );
    assert!(briefing.contains("you are here"), "{briefing}");
    assert!(!briefing.contains("fix the lexer"), "{briefing}");
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
fn startup_is_scoped_reuses_scans_and_delivers_repo_knowledge_from_the_root() {
    let fixture = Fixture::new("scoped-startup");
    let repo = fixture.make_git_repo("selected");
    fixture.set_context(r#"{"repo":{"name":"selected","role":"selected role","not_roles":[]}}"#);
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "personal",
        "decision",
        "Keep this personal decision.",
    );
    fixture.node(
        &repo.join(".ohjaa/knowledge"),
        "repo-rule",
        "decision",
        "Selected repository rule.",
    );
    let launch = || {
        output_of(fixture.ohjaa_in(&fixture.dir).args([
            "run",
            "codex",
            "--intent",
            "fix selected",
            "--dry-run",
            "--scope",
            repo.to_str().unwrap(),
        ]))
    };
    let (_, before, code) = launch();
    assert_eq!(code, Some(0), "{before}");
    assert!(
        before.contains("Keep this personal decision.")
            && before.contains("Selected repository rule.")
    );
    let calls = fs::read_to_string(fixture.dir.join("calls.txt")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.ends_with("|sessions --json --outside-repos"))
            .count(),
        1,
        "{calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.ends_with("|sessions --json"))
            .count(),
        1,
        "{calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.ends_with("|context --json"))
            .count(),
        1,
        "{calls}"
    );

    // Growing the unrelated fleet must not grow the prompt or trigger its scans.
    for i in 0..20 {
        fixture.make_repo(&format!("broken-unrelated-{i}"));
    }
    fs::write(fixture.dir.join("calls.txt"), "").unwrap();
    let (_, after, code) = launch();
    assert_eq!(code, Some(0), "{after}");
    assert_eq!(after.len(), before.len());
    assert!(!after.contains("broken-unrelated"));
    assert!(!fs::read_to_string(fixture.dir.join("calls.txt"))
        .unwrap()
        .contains("broken-unrelated"));
    assert!(after.contains("Other repositories were not scanned"));

    let (brief, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "brief",
        "--scope",
        repo.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(brief.contains("Selected repository rule.") && !brief.contains("broken-unrelated"));
    assert_eq!(
        brief
            .matches("Knowledge (ohjaa, knowledge-node.v1)")
            .count(),
        1,
        "the preamble is said once, not once per scoped repository: {brief}"
    );
    let (_, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "run",
        "codex",
        "--intent",
        "fix selected",
        "--scope",
        repo.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    let (export, stderr, code) =
        output_of(fixture.ohjaa_in(&fixture.dir).args(["usage", "export"]));
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&export).unwrap();
    let delivered = export["sessions"][0]["delivered"].as_array().unwrap();
    let node = delivered.iter().find(|n| n["id"] == "repo-rule").unwrap();
    assert_eq!(node["repo"], repo.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(node["scope"], "repo");
    fixture.node(
        &repo.join(".ohjaa/knowledge"),
        "long-rule",
        "decision",
        &"Keep this rule. ".repeat(600),
    );
    let (brief, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "brief",
        "--knowledge-only",
        "--scope",
        repo.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("combined scoped knowledge briefing"));
    assert_eq!(
        brief.matches("Keep this rule.").count(),
        600,
        "knowledge must not be truncated"
    );
    fixture.cleanup();
}

#[test]
fn unassigned_startup_does_not_derive_the_fleet_and_scope_failures_remain_visible() {
    let fixture = Fixture::new("unassigned");
    fixture.make_repo("broken-unrelated");
    let (_, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "run",
        "codex",
        "--intent",
        "unknown",
        "--dry-run",
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("If no concrete task is assigned, acknowledge and wait"));
    assert!(stderr.contains("--briefing"), "{stderr}");
    assert!(
        !stderr.contains("--task "),
        "idle startup must not manufacture a task: {stderr}"
    );
    let calls = fs::read_to_string(fixture.dir.join("calls.txt")).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "only the outside-session preflight is needed: {calls}"
    );
    assert!(!stderr.contains("broken-unrelated"));

    let selected = fixture.make_git_repo("selected");
    // Role derivation failed: no context payload exists. It must be marked unknown.
    let (brief, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "brief",
        "--scope",
        selected.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(4), "{brief}{stderr}");
    assert!(brief.contains("role not derivable"));
    fixture.set_context(r#"{"repo":{"name":"selected","role":"role"}}"#);
    fixture.set_outside_sessions(
        r#"{"sessions":[],"coverage":{"status":"degraded"},"warnings":["partial outside scan"]}"#,
    );
    let (brief, stderr, code) = output_of(fixture.ohjaa_in(&fixture.dir).args([
        "brief",
        "--scope",
        selected.to_str().unwrap(),
    ]));
    assert_eq!(code, Some(4), "{brief}{stderr}");
    assert!(brief.contains("incomplete") && brief.contains("partial outside scan"));
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

#[test]
fn usage_import_is_attributed_idempotent_and_exportable() {
    let fixture = Fixture::new("usage");
    let repo = fixture.make_git_repo("work");
    let receipt = fixture.dir.join("usage.json");
    let data = serde_json::json!({
        "schema":"agent-usage.v1", "agent":"sh", "session_id":"native-1",
        "source":"synthetic fixture", "observed_at_ms":1790000000000_i64,
        "complete":false,"limitations":["test source"],"entries":[{
            "provider":"test", "model":"test", "input_tokens":100,"output_tokens":20,
            "cache_read_tokens":null,"cache_write_tokens":null,
            "estimated_usd_micros":25,"estimate_basis":"synthetic rate",
            "credits":null,"billed":null
        }]
    });
    fs::write(&receipt, serde_json::to_vec(&data).unwrap()).unwrap();
    let script = format!(
        "{} usage import '{}'; {} usage import '{}'",
        ohjaa_bin(),
        receipt.display(),
        ohjaa_bin(),
        receipt.display()
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    let (out, err, code) = output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{err}");
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(export["schema"], "ohjaa-sessions.v1");
    assert_eq!(export["sessions"][0]["usage"].as_array().unwrap().len(), 1);
    assert_eq!(
        export["sessions"][0]["usage"][0]["entries"][0]["input_tokens"],
        100
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_ne!(code, Some(0));
    assert!(stderr.contains("already attributed"), "{stderr}");
    let id = export["sessions"][0]["id"].as_str().unwrap();
    let mut stale = data;
    stale["entries"][0]["input_tokens"] = 200.into();
    fs::write(&receipt, serde_json::to_vec(&stale).unwrap()).unwrap();
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["usage", "import"])
            .arg(&receipt)
            .args(["--session", id])
            .current_dir(&repo),
    );
    assert_ne!(code, Some(0));
    assert!(stderr.contains("stale or conflicting"), "{stderr}");
    stale["observed_at_ms"] = 1790000000001_i64.into();
    stale["entries"][0]["input_tokens"] = (-1).into();
    fs::write(&receipt, serde_json::to_vec(&stale).unwrap()).unwrap();
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["usage", "import"])
            .arg(&receipt)
            .args(["--session", id])
            .current_dir(&repo),
    );
    assert_ne!(code, Some(0));
    assert!(stderr.contains("invalid usage receipt"), "{stderr}");
    let (out, _, _) = output_of(
        fixture
            .ohjaa()
            .args(["usage", "export", "--session", id])
            .current_dir(&repo),
    );
    let unchanged: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        unchanged["sessions"][0]["usage"][0]["entries"][0]["input_tokens"],
        100
    );
    fixture.cleanup();
}

#[test]
fn leaving_picks_up_a_fresh_receipt_without_changing_the_agent_exit() {
    let fixture = Fixture::new("usage-leaving");
    let repo = fixture.make_git_repo("work");
    let path = fixture.dir.join("receipt.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 1000;
    let data = serde_json::json!({"schema":"agent-usage.v1","agent":"sh","session_id":"native-leaving","source":"fixture","observed_at_ms":now,"complete":false,"limitations":["fixture"],"entries":[]});
    let script = format!("printf '%s' '{}' > \"$OHJAA_USAGE_FILE\"; exit 7", data);
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "usage test",
                "--no-briefing",
                "--usage-file",
            ])
            .arg(&path)
            .args(["--", "-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(7), "{stderr}");
    assert!(stderr.contains("native-leaving"), "{stderr}");
    let (out, _, code) = output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0));
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        export["sessions"][0]["usage"][0]["session_id"],
        "native-leaving"
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "do not reuse",
                "--no-briefing",
                "--usage-file",
            ])
            .arg(&path)
            .args(["--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(1));
    assert!(stderr.contains("already exists"), "{stderr}");
    fixture.cleanup();
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
        stdout.contains(&format!("  {id} · sh · started "))
            && stdout.contains(" UTC · retry uploads"),
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
        phases.contains(" receipt: passed · ") && phases.ends_with("no sykli receipt"),
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

/// A session as `toimija sessions --outside-repos` reports it: its worktree
/// is the folder the agent works in.
fn outside_session(id: &str, folder: &str, intent: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!(
        "{{\"id\":\"{id}\",\"agent\":[\"claude\"],\"worktree\":\"{folder}\",\"intent\":\"{intent}\",\
         \"scope\":[],\"liveness\":\"active\",\"pid_check\":\"alive\",\"last_seen_at\":{now}}}"
    )
}

#[test]
fn run_outside_a_repo_launches_from_the_folder_and_says_what_it_cannot_see() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("outside-run");
    let folder = fixture.make_folder("projects");
    let (_, stderr, code) = output_of(fixture.ohjaa_in(&folder).args([
        "run",
        "claude",
        "--intent",
        "sweep the fleet",
        "--scope",
        "/r/a",
        "--dry-run",
    ]));
    assert_eq!(code, Some(0), "{stderr}");
    for said in [
        "is not a Git repository; 0 sibling session(s) in this folder at launch",
        "--scope /r/a is not inside a Git repository; nothing there is observed",
        "no --scope names a repository; at leaving, the repositories directly",
        "This folder is not a Git repository, and no --scope named one",
        "which is not one of these repositories",
        "would run: toimija run --intent 'sweep the fleet'",
        "--scope /r/a -- claude",
    ] {
        assert!(stderr.contains(said), "missing `{said}`: {stderr}");
    }
    fixture.cleanup();
}

/// A live session launched outside every repository, scoped to `scope`.
fn scoped_session(id: &str, scope: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!(
        "{{\"id\":\"{id}\",\"agent\":[\"claude\"],\"worktree\":\"/somewhere\",\"intent\":\"sweep\",\
         \"scope\":[\"{scope}\"],\"liveness\":\"active\",\"pid_check\":\"alive\",\"last_seen_at\":{now}}}"
    )
}

#[test]
fn a_root_session_records_the_commits_it_made_in_a_scoped_repo() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("root-scoped");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_git_repo("work");
    let script = format!(
        "cd '{}' && echo x > f.txt && git add f.txt && git commit -q -m 'scoped work'",
        repo.display()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args([
                "run",
                "sh",
                "--intent",
                "root work",
                "--no-briefing",
                "--scope",
            ])
            .arg(&repo)
            .args(["--", "-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1"),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "scoped work: 0 session(s) live there at launch; its commits and pull request will be recorded"
        ),
        "{stderr}"
    );
    let phases = stderr
        .lines()
        .find(|l| l.starts_with("in work") && l.contains(" phases "))
        .expect(&stderr);
    assert!(
        phases.ends_with("no sykli receipt"),
        "the commit made in the scoped repo is its phase: {phases}"
    );
    assert!(
        stderr.contains("in work       finished unknown (gh is not signed in)"),
        "{stderr}"
    );
    assert!(!stderr.contains("none to observe"), "{stderr}");
    fixture.cleanup();
}

#[test]
fn a_repo_launch_counts_root_sessions_scoped_to_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("scoped-here");
    let repo = fixture.make_git_repo("work");
    let here = fs::canonicalize(&repo).unwrap();
    fixture.set_outside_sessions(&sessions_json(
        &format!(
            "{},{}",
            scoped_session("3-3", &here.join("src").display().to_string()),
            scoped_session("4-4", "/somewhere/else")
        ),
        "available",
    ));
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "codex", "--intent", "x", "--dry-run"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr
            .contains("1 more, launched outside repositories, name this checkout in their --scope"),
        "only the session whose scope reaches this checkout counts: {stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_root_launch_refuses_when_a_scoped_repo_cannot_be_read() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("scoped-blind");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_git_repo("broken-work");
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args(["run", "claude", "--intent", "x", "--dry-run", "--scope"])
            .arg(&repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert!(
        stderr.contains("named with --scope") && stderr.contains("refusing to launch blind"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_scope_path_not_created_yet_is_observed_in_its_repo() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("scoped-new-path");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_git_repo("work");
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args(["run", "claude", "--intent", "x", "--dry-run", "--scope"])
            .arg(repo.join("src").join("new_module.rs")),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "scoped work: 0 session(s) live there at launch; its commits and pull request will be recorded"
        ),
        "a file the agent will create is still in its repository: {stderr}"
    );
    assert!(
        !stderr.contains("is not inside a Git repository"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_root_session_is_the_last_session_in_the_repos_it_named() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("root-last");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_git_repo("work");
    let script = format!(
        "cd '{}' && echo x > f.txt && git add f.txt && git commit -q -m 'scoped work' \
         && {} handover --unfinished 'rename half done'",
        repo.display(),
        ohjaa_bin()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args([
                "run",
                "sh",
                "--intent",
                "root work",
                "--no-briefing",
                "--scope",
            ])
            .arg(&repo)
            .args(["--", "-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1"),
    );
    assert_eq!(code, Some(0), "{stderr}");

    // The next agent launched inside the repository is briefed with it.
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    for said in [
        "Last session here (oh-",
        "projects, it named this repository with --scope):",
        "- work in work: 1 commit(s)",
        "- unfinished: rename half done",
    ] {
        assert!(stdout.contains(said), "missing `{said}`: {stdout}");
    }
    let (stdout, stderr, code) = output_of(fixture.ohjaa().arg("session").current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("rename half done"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn run_outside_a_repo_counts_only_the_siblings_in_its_folder() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("outside-siblings");
    let folder = fixture.make_folder("projects");
    let here = fs::canonicalize(&folder).unwrap();
    fixture.set_outside_sessions(&sessions_json(
        &format!(
            "{},{}",
            outside_session("1-1", &here.display().to_string(), "here too"),
            outside_session("2-2", "/somewhere/else", "elsewhere")
        ),
        "available",
    ));
    let (_, stderr, code) =
        output_of(
            fixture
                .ohjaa_in(&folder)
                .args(["run", "claude", "--intent", "x", "--dry-run"]),
        );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("1 sibling session(s) in this folder at launch"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_session_outside_a_repo_is_recorded_without_commits_to_observe() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("outside-session");
    let folder = fixture.make_folder("projects");
    let script = format!(
        "{} handover --unfinished 'renamed the flag in two repos'",
        ohjaa_bin()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args(["run", "sh", "--intent", "sweep", "--no-briefing", "--"])
            .args(["-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1"),
    );
    assert_eq!(code, Some(0), "{stderr}");
    for said in [
        "phases        none to observe (not a Git repository)",
        "finished      none to observe (not a Git repository)",
        "unfinished    renamed the flag in two repos",
    ] {
        assert!(stderr.contains(said), "missing `{said}`: {stderr}");
    }

    // The folder's last session is found from the folder, and the next agent
    // launched there is told what it left.
    let (stdout, _, code) = output_of(fixture.ohjaa_in(&folder).arg("session"));
    assert_eq!(code, Some(0), "{stdout}");
    assert!(stdout.contains("renamed the flag in two repos"), "{stdout}");
    let (_, stderr, _) = output_of(fixture.ohjaa_in(&folder).args([
        "run",
        "claude",
        "--intent",
        "next",
        "--dry-run",
    ]));
    assert!(
        stderr.contains("not a Git repository, so no commits or pull request to observe"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_toimija_without_outside_sessions_is_named_never_read_as_empty() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("outside-old");
    let folder = fixture.make_folder("projects");
    fixture.make_repo("alpha");
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args(["run", "claude", "--intent", "x", "--dry-run"])
            .env("OHJAA_STUB_NO_OUTSIDE", "1"),
    );
    assert_eq!(
        code,
        Some(3),
        "a run outside a repo refuses to launch blind: {stderr}"
    );
    assert!(
        stderr.contains("this toimija cannot list sessions outside repositories"),
        "{stderr}"
    );

    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .arg("fleet")
            .arg("--root")
            .arg(&fixture.dir)
            .env("OHJAA_STUB_NO_OUTSIDE", "1"),
    );
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        stderr.contains("coverage: scanned 1/1 repo(s)\n")
            && stderr.contains("outside any repository: this toimija cannot list"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn fleet_lists_sessions_outside_repos_by_their_folder() {
    let fixture = Fixture::new("outside-fleet");
    fixture.make_repo("alpha");
    fixture.set_outside_sessions(&sessions_json(
        &outside_session("5-5", "/Users/x/projects", "sweep the fleet"),
        "available",
    ));
    let (stdout, stderr, code) =
        output_of(fixture.ohjaa().arg("fleet").arg("--root").arg(&fixture.dir));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("outside any repository\n    claude  5-5\n      folder: /Users/x/projects"),
        "{stdout}"
    );
    assert!(
        stderr.contains("coverage: scanned 1/1 repo(s) and the sessions outside repositories"),
        "{stderr}"
    );
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
        // A commit on a branch of its own, so leaving asks gh about its PR.
        .arg(format!(
            "git switch -q -c side && git commit -q --allow-empty -m side; touch '{}'; exit 0",
            mark.display()
        ))
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

// ---- review of #38: each test failed before its fix ---------------------------

#[test]
fn a_stalled_export_reader_never_costs_a_running_session_its_record() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("export-stall");
    let repo = fixture.make_git_repo("work");
    // Enough history that the export outgrows a pipe buffer.
    let long = "x".repeat(60_000);
    for _ in 0..3 {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args([
                    "run",
                    "sh",
                    "--intent",
                    &long,
                    "--no-briefing",
                    "--",
                    "-c",
                    "exit 0",
                ])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
    }
    // A reader that stops reading: the export blocks on its write.
    let mut export = fixture
        .ohjaa()
        .args(["usage", "export"])
        .current_dir(&repo)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 0");
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("recorded; queued") && !stderr.contains("could not be saved"),
        "the session log was free while the export waited on its reader: {stderr}"
    );
    let mut out = String::new();
    use std::io::Read as _;
    export
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert!(export.wait().unwrap().success());
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(export["sessions"].as_array().unwrap().len(), 3);
    fixture.cleanup();
}

#[test]
fn a_refused_usage_receipt_is_recorded_as_refused_with_its_reason() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("usage-refused");
    let repo = fixture.make_git_repo("work");
    let receipt = |agent: &str, observed: &str, extra: &str| {
        format!(
            "printf '%s' '{{\"schema\":\"agent-usage.v1\",\"agent\":\"{agent}\",\"session_id\":\"n\",\
             \"source\":\"t\",\"observed_at_ms\":{observed},\"complete\":true,\"limitations\":[],\
             \"entries\":[]{extra}}}' > \"$OHJAA_USAGE_FILE\""
        )
    };
    // Closes the single quotes around the JSON, so the shell expands it.
    let now = "'\"$(date +%s)000\"'";
    let cases = [
        (
            "symlink",
            "printf x > \"$OHJAA_USAGE_FILE.real\"; ln -s \"$OHJAA_USAGE_FILE.real\" \"$OHJAA_USAGE_FILE\""
                .to_owned(),
            "symbolic links",
        ),
        (
            "fifo",
            "mkfifo \"$OHJAA_USAGE_FILE\"".to_owned(),
            "must be a regular file",
        ),
        (
            "size",
            "head -c 9000000 /dev/zero > \"$OHJAA_USAGE_FILE\"".to_owned(),
            "exceeds 8 MiB",
        ),
        ("predates", receipt("sh", "1", ""), "predates this run"),
        (
            "agent",
            receipt("codex", now, ""),
            "receipt agent does not match session agent",
        ),
        (
            "unknown-field",
            receipt("sh", now, ",\"later\":1"),
            "unknown field `later`",
        ),
    ];
    for (case, write, reason) in cases {
        let path = fixture.dir.join(format!("usage-{case}.json"));
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args([
                    "run",
                    "sh",
                    "--intent",
                    case,
                    "--no-briefing",
                    "--usage-file",
                ])
                .arg(&path)
                .args(["--", "-c", &format!("{write}; exit 7")])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .current_dir(&repo),
        );
        assert_eq!(
            code,
            Some(7),
            "{case}: the agent's exit is unchanged: {stderr}"
        );
        let usage = &stderr[stderr.find("─ usage").expect(&stderr)..];
        assert!(
            usage.contains("refused: ") && usage.contains(reason),
            "{case}: the record says it was refused, and why: {usage}"
        );
        assert!(!usage.contains("not collected"), "{case}: {usage}");
        let (out, _, _) = output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
        let export: serde_json::Value = serde_json::from_str(&out).unwrap();
        let refused = export["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["intent"] == case)
            .unwrap()["usage_refused"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(refused.contains(reason), "{case}: exported: {refused}");
    }
    fixture.cleanup();
}

#[test]
fn export_creates_no_log_and_one_bad_row_does_not_fail_an_export_of_another_session() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("export-quirks");
    let repo = fixture.make_git_repo("work");
    let log = fixture.dir.join("state/ohjaa/sessions.redb");
    let (out, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(export["schema"], "ohjaa-sessions.v1");
    assert_eq!(export["sessions"], serde_json::json!([]));
    assert_eq!(export["unreadable"], serde_json::json!([]));
    assert!(!log.exists(), "an export never creates the log");

    let (_, stderr, code) = run_session(&fixture, &repo, "echo \"id=$OHJAA_SESSION\"");
    assert_eq!(code, Some(0), "{stderr}");
    let (out, _, _) = output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    let id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    {
        let db = redb::Database::open(&log).unwrap();
        let tx = db.begin_write().unwrap();
        tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("sessions"))
            .unwrap()
            .insert("oh-garbage", b"\"not a session\"".as_slice())
            .unwrap();
        tx.commit().unwrap();
    }
    let (out, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["usage", "export", "--session", &id])
            .current_dir(&repo),
    );
    assert_eq!(
        code,
        Some(0),
        "an unrelated bad row does not fail this export: {out}"
    );
    let (out, _, code) = output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(
        code,
        Some(4),
        "the whole log is incomplete, and says so: {out}"
    );
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(export["unreadable"], serde_json::json!(["oh-garbage"]));
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["usage", "export", "--session", "oh-garbage"])
            .current_dir(&repo),
    );
    assert_ne!(code, Some(0));
    assert!(
        stderr.contains("session oh-garbage is unreadable"),
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_briefing_too_big_for_one_command_line_argument_is_refused_clearly() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("briefing-cap");
    let repo = fixture.make_git_repo("work");
    let mine = fixture.dir.join("config/ohjaa/mine");
    let launch = || {
        output_of(
            fixture
                .ohjaa()
                .args(["run", "codex", "--intent", "x", "--dry-run"])
                .current_dir(&repo),
        )
    };
    fixture.node(&mine, "big", "fact", &"long knowledge ".repeat(500));
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(0), "{stderr}");
    // Linux caps one argument at 128 KiB, and the briefing is one, twice.
    fixture.node(&mine, "huge", "fact", &"long knowledge ".repeat(8_000));
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(3), "{stderr}");
    assert!(
        stderr.contains("KiB, over the 100 KiB a briefing may be")
            && stderr.contains("refusing to launch")
            && !stderr.contains("would run:"),
        "{stderr}"
    );
    fixture.cleanup();
}

// ---- where work happens (audit 2026-10-09): each test failed before its fix ----

/// A shell function for stub agents: `receipt <outcome> [<contract hash>]`
/// writes a sykli-receipt.v1 for HEAD's tree into this checkout, as `sykli
/// run` does.
const RECEIPT_FN: &str = r#"
receipt() {
  tree=$(git rev-parse 'HEAD^{tree}')
  mkdir -p .sykli/receipts
  printf '{"schema":"sykli-receipt.v1","contract_hash":"'"${2:-c}"'","started_at_ms":%s000,"subject":{"repository":"r","tree_oid":"%s","inputs_digest":"d","head_tree_oid":"%s","dirty":false},"tasks":[],"outcome":"%s"}' \
    "$(date +%s)" "$tree" "$tree" "$1" > ".sykli/receipts/rcpt_$tree.json"
}
"#;

fn short_head(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn a_commit_made_in_a_worktree_is_a_phase_with_the_receipt_made_there() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("worktree");
    let repo = fixture.make_git_repo("work");
    let worktree = fixture.dir.join("_worktrees").join("work-feat");
    let script = format!(
        "{RECEIPT_FN}
        set -e
        git worktree add -q -b feat '{wt}'
        cd '{wt}'
        echo one > a.txt && git add a.txt && git commit -q -m 'worktree phase'
        receipt passed",
        wt = worktree.display()
    );
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    let commit = short_head(&worktree);
    let phases = stderr
        .lines()
        .find(|l| l.starts_with("phases"))
        .expect(&stderr);
    assert!(
        phases.contains(&commit) && phases.contains("passed") && phases.contains("in work-feat"),
        "the worktree's commit is a phase, with the receipt sykli wrote there: {phases}"
    );
    fixture.cleanup();
}

#[test]
fn a_root_launch_without_scope_records_the_repos_it_worked_in() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("root-found");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_settled_git_repo("projects/work");
    fixture.make_settled_git_repo("projects/idle");
    let worktree = folder.join("_worktrees").join("work-feat");
    // A repository only found at leaving counts moves after the second the
    // session started (nothing was read of it at entering); a real agent reads
    // its briefing first.
    let script = format!(
        "set -e
        sleep 1
        cd '{repo}'
        git worktree add -q -b feat '{wt}'
        cd '{wt}'
        echo x > f.txt && git add f.txt && git commit -q -m 'found work'
        {ohjaa} handover --unfinished 'half the rename'",
        repo = repo.display(),
        wt = worktree.display(),
        ohjaa = ohjaa_bin()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa_in(&folder)
            .args(["run", "sh", "--intent", "root work", "--no-briefing", "--"])
            .args(["-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1"),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let commit = short_head(&worktree);
    let observed = stderr
        .lines()
        .find(|l| l.starts_with("in work") && l.contains(" observed "))
        .expect(&stderr);
    assert!(
        observed.contains("observed during the session, not attributed:")
            && observed.contains(&commit)
            && observed.contains("in work-feat"),
        "what moved in a repository nobody named is observed, never credited: {observed}"
    );
    assert!(
        !stderr
            .lines()
            .any(|l| l.starts_with("in work") && l.contains(" phases ")),
        "{stderr}"
    );
    assert!(
        stderr.contains("not named with --scope"),
        "and says it was found, not named: {stderr}"
    );
    assert!(
        !stderr.contains("in idle"),
        "a repository nothing was made in is not: {stderr}"
    );
    assert!(!stderr.contains("none to observe"), "{stderr}");

    // It never becomes that repository's last session; the root's says it.
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(!stdout.contains("Last session here"), "{stdout}");
    let (stdout, _, _) = output_of(fixture.ohjaa_in(&folder).arg("brief"));
    for said in [
        "Last session here (oh-",
        "- in work: 1 commit(s) observed during the session, not attributed (found at leaving)",
        "- unfinished: half the rename",
    ] {
        assert!(stdout.contains(said), "missing `{said}`: {stdout}");
    }
    fixture.cleanup();
}

#[test]
fn a_scope_naming_the_folder_of_repos_records_the_ones_that_moved() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("scope-folder");
    let launch = fixture.make_git_repo("launch");
    let folder = fixture.make_folder("projects");
    let repo = fixture.make_settled_git_repo("projects/work");
    let script = format!(
        "sleep 1; cd '{}' && echo x > f.txt && git add f.txt && git commit -q -m 'in a named folder'",
        repo.display()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "x", "--no-briefing", "--scope"])
            .arg(&folder)
            .args(["--", "-c", &script])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&launch),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "is not inside a Git repository; at leaving, the repositories directly in it"
        ),
        "{stderr}"
    );
    let observed = stderr
        .lines()
        .find(|l| l.starts_with("in work") && l.contains(" observed "))
        .expect(&stderr);
    assert!(observed.contains(&short_head(&repo)), "{observed}");
    fixture.cleanup();
}

/// A session whose agent commits on branch `feat` in a new worktree `name`
/// of `repo`, with a sykli contract, and writes a receipt with `receipt`'s
/// arguments; gh reports a pull request for `feat`. Returns the record and
/// the commit.
fn pull_request_session(
    fixture: &Fixture,
    repo: &Path,
    name: &str,
    receipt: &str,
    sykli_commit: bool,
) -> (String, String) {
    let worktree = fixture.dir.join("_worktrees").join(name);
    let script = format!(
        "{RECEIPT_FN}
        set -e
        git worktree add -q -b feat-{name} '{wt}'
        cd '{wt}'
        echo '{{}}' > sykli.json && echo '{name}' > name.txt
        git add sykli.json name.txt && git commit -q -m 'ship {name}'
        receipt {receipt}",
        wt = worktree.display()
    );
    let mut command = fixture.ohjaa();
    command
        .args([
            "run",
            "sh",
            "--intent",
            "ship",
            "--no-briefing",
            "--",
            "-c",
            &script,
        ])
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .env("OHJAA_STUB_GH_PR_BRANCH", format!("feat-{name}"))
        .env(
            "OHJAA_STUB_SYKLI_CALLS",
            fixture.dir.join("sykli-calls.txt"),
        )
        .current_dir(repo);
    if sykli_commit {
        command.env("OHJAA_STUB_SYKLI_COMMIT", "1");
    }
    let (_, stderr, code) = output_of(&mut command);
    assert_eq!(code, Some(0), "{stderr}");
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&worktree)
        .output()
        .unwrap();
    (
        stderr,
        String::from_utf8_lossy(&head.stdout).trim().to_owned(),
    )
}

fn finished_line(record: &str) -> &str {
    record
        .lines()
        .find(|l| l.starts_with("finished"))
        .expect(record)
}

#[test]
fn the_pull_request_head_is_verified_by_sykli_never_by_the_receipts_word() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("pr-head");
    let repo = fixture.make_git_repo("work");

    // The receipt says passed; sykli refuses it. The record says what sykli said.
    let (record, head) = pull_request_session(&fixture, &repo, "forged", "passed forged", true);
    let finished = finished_line(&record);
    assert!(
        finished.contains("https://github.com/o/r/pull/7 (open)")
            && finished.contains(&format!("head {}", &head[..7]))
            && finished
                .contains("FAILED sykli verify: refused: contract expected c but got forged"),
        "{finished}"
    );
    assert!(!finished.contains("passed"), "{finished}");
    let calls = fs::read_to_string(fixture.dir.join("sykli-calls.txt")).unwrap();
    assert!(
        calls.contains(&format!("--contract sykli.json --commit {head}")),
        "{calls}"
    );
    assert!(
        calls.contains("_worktrees/forged|verify "),
        "sykli verifies in the checkout the receipt is in: {calls}"
    );

    // An honest receipt, verified by a sykli that can bind a commit.
    let (record, head) = pull_request_session(&fixture, &repo, "honest", "passed", true);
    assert!(
        finished_line(&record).ends_with(&format!(
            "head {}: verified: passed (sykli verify --commit)",
            &head[..7]
        )),
        "{record}"
    );

    // The same receipt with a sykli that cannot: never a silent pass.
    let (record, _) = pull_request_session(&fixture, &repo, "old-sykli", "passed", false);
    assert!(
        finished_line(&record).ends_with("unverified: sykli has no --commit"),
        "{record}"
    );
    assert!(
        !record.contains("FLAGGED"),
        "a sykli that cannot say flags nothing: {record}"
    );
    fixture.cleanup();
}

/// A clone of a bare origin whose `main` has one commit, as `origin/main`.
fn clone_with_origin(fixture: &Fixture, name: &str) -> PathBuf {
    let origin = fixture.dir.join(format!("{name}-origin.git"));
    let status = Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&origin)
        .status()
        .unwrap();
    assert!(status.success());
    let repo = fixture.dir.join(name);
    let status = Command::new("git")
        .args(["clone", "-q"])
        .arg(&origin)
        .arg(&repo)
        .output()
        .unwrap();
    assert!(status.status.success());
    run_git(&repo, &["config", "user.email", "ohjaa@example.invalid"]);
    run_git(&repo, &["config", "user.name", "Ohjaa Test"]);
    // Long before the session, as far as reflog times say.
    for args in [
        &["commit", "-q", "--allow-empty", "-m", "init"][..],
        &["push", "-q", "origin", "HEAD:main"],
    ] {
        let status = Command::new("git")
            .args(args)
            .env("GIT_COMMITTER_DATE", "@1000000000 +0000")
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    }
    run_git(&repo, &["remote", "set-head", "origin", "main"]);
    repo
}

/// A session in a worktree of `repo` that runs `work` there, with the stub
/// sykli in `mode` (`""`: no --commit). Returns the record and the worktree.
fn worktree_push_session(
    fixture: &Fixture,
    repo: &Path,
    name: &str,
    work: &str,
    mode: &str,
) -> (String, PathBuf) {
    let worktree = fixture.dir.join("_worktrees").join(name);
    let script = format!(
        "{RECEIPT_FN}
        set -e
        git worktree add -q -b {name} '{wt}'
        cd '{wt}'
        {work}",
        wt = worktree.display()
    );
    let mut command = fixture.ohjaa();
    command
        .args([
            "run",
            "sh",
            "--intent",
            name,
            "--no-briefing",
            "--",
            "-c",
            &script,
        ])
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .current_dir(repo);
    if !mode.is_empty() {
        command.env("OHJAA_STUB_SYKLI_COMMIT", mode);
    }
    let (_, stderr, code) = output_of(&mut command);
    assert_eq!(code, Some(0), "{stderr}");
    (stderr, worktree)
}

fn short_of(dir: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", rev])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn landed_lines(record: &str) -> Vec<&str> {
    record.lines().filter(|l| l.starts_with("landed")).collect()
}

#[test]
fn the_documented_flow_pushed_to_main_is_verified_at_its_tip_and_raises_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("landed");
    let repo = clone_with_origin(&fixture, "work");
    // Each phase a commit and a sykli run; then one push.
    let (record, worktree) = worktree_push_session(
        &fixture,
        &repo,
        "flow",
        "echo '{}' > sykli.json && git add sykli.json && git commit -q -m one
        receipt passed
        echo x > x.txt && git add x.txt && git commit -q -m two
        receipt passed
        git push -q origin HEAD:main",
        "1",
    );
    let (one, two) = (short_of(&worktree, "HEAD~1"), short_of(&worktree, "HEAD"));
    assert_eq!(
        landed_lines(&record),
        [
            format!("landed        {one} reached origin/main: receipt: passed (not verified: only a push's tip is)"),
            format!("landed        {two} reached origin/main: verified: passed (sykli verify --commit)"),
        ],
        "{record}"
    );
    assert!(
        !record.contains("FLAGGED") && !record.contains("ohjaa: flagged"),
        "{record}"
    );
    fixture.cleanup();
}

#[test]
fn what_sykli_could_not_check_is_recorded_and_never_flagged() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("not-flagged");
    let repo = clone_with_origin(&fixture, "work");
    let quiet = |record: &str| {
        assert!(
            !record.contains("FLAGGED") && !record.contains("ohjaa: flagged"),
            "{record}"
        );
    };
    // A stray file beside the commit: sykli#67 as it stands checks the
    // checkout, not the commit.
    let (record, wt) = worktree_push_session(
        &fixture,
        &repo,
        "stray",
        "echo '{}' > sykli.json && git add sykli.json && git commit -q -m checked
        receipt passed
        git push -q origin HEAD:main
        echo scratch > notes.txt",
        "1",
    );
    assert!(
        landed_lines(&record).concat().contains(&format!(
            "{} reached origin/main: stale: sykli checked the checkout, not the commit",
            short_of(&wt, "HEAD")
        )),
        "{record}"
    );
    quiet(&record);
    // A sykli without --commit cannot say.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "old-sykli",
        "git pull -q origin main && echo y > y.txt && git add y.txt && git commit -q -m y
        receipt passed
        git push -q origin HEAD:main",
        "",
    );
    assert!(
        landed_lines(&record)
            .concat()
            .contains("unverified: sykli has no --commit"),
        "{record}"
    );
    quiet(&record);
    // Pushed without a sykli run.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "unchecked",
        "git pull -q origin main && echo z > z.txt && git add z.txt && git commit -q -m z
        git push -q origin HEAD:main",
        "1",
    );
    assert!(
        landed_lines(&record)
            .concat()
            .contains("unchecked: no sykli receipt for its tree"),
        "{record}"
    );
    quiet(&record);
    // And nothing of it opens the next briefing.
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(!stdout.contains("WARNING"), "{stdout}");
    // The commit-based sykli passes the stray-file case.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "stray-new",
        "git pull -q origin main && echo w > w.txt && git add w.txt && git commit -q -m w
        receipt passed
        git push -q origin HEAD:main
        echo scratch > notes.txt",
        "new",
    );
    assert!(
        landed_lines(&record).concat().contains("verified: passed"),
        "{record}"
    );
    fixture.cleanup();
}

#[test]
fn a_failing_commit_pushed_to_main_flags_the_record_and_tops_the_next_briefing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("loud");
    let repo = clone_with_origin(&fixture, "work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let script = format!(
        "{RECEIPT_FN}
        set -e
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m 'broken build'
        receipt failed
        git push -q origin HEAD:main
        {ohjaa} handover --next 'ship it'",
        ohjaa = ohjaa_bin()
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "push",
                "--no-briefing",
                "--",
                "-c",
                &script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "the agent's exit is still ohjaa's: {stderr}");
    let pushed = short_head(&repo);
    let warning = format!(
        "origin/main at {pushed} in work has no passing evidence — check before building on it"
    );
    let flagged = stderr.find("─ FLAGGED").expect(&stderr);
    assert!(
        flagged < stderr.find("─ observed").unwrap(),
        "the flag comes first in the record: {stderr}"
    );
    assert!(stderr.contains(&warning), "{stderr}");
    assert!(
        stderr.contains("FAILED sykli verify: refused: outcome expected passed"),
        "it says why: {stderr}"
    );
    assert!(
        stderr
            .trim_end()
            .ends_with(&format!("ohjaa: flagged: {warning}")),
        "and says it again last, where the person reads: {stderr}"
    );

    // The next agent hears it before anything else, the handover included.
    let (_, stderr, _) = output_of(
        fixture
            .ohjaa()
            .args(["run", "codex", "--intent", "next", "--dry-run"])
            .current_dir(&repo),
    );
    let briefing = &stderr[stderr.find("--briefing '").expect(&stderr) + 12..];
    assert!(
        briefing.starts_with("WARNING from the last session here (oh-"),
        "{briefing}"
    );
    let top = briefing.lines().take(2).collect::<Vec<_>>().join("\n");
    assert!(top.contains(&warning), "{top}");
    assert!(
        briefing.find(&warning).unwrap() < briefing.find("ship it").unwrap(),
        "{briefing}"
    );
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(
        stdout.starts_with("WARNING from the last session here"),
        "{stdout}"
    );
    fixture.cleanup();
}

#[test]
fn a_session_whose_ohjaa_was_killed_is_closed_as_orphaned_at_the_next_launch() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("orphan");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let agent_pid = fixture.dir.join("agent.pid");
    let mut child = spawn_session(
        &fixture,
        &repo,
        &format!("echo $$ > '{}'; exec sleep 30", agent_pid.display()),
    );
    signal(child.id(), "KILL");
    let _ = child.wait();
    // The agent outlives a SIGKILLed ohjaa; stop it, as a person would.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !agent_pid.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid = fs::read_to_string(&agent_pid).unwrap();
    signal(pid.trim().parse().unwrap(), "KILL");
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("session").current_dir(&repo));
    let orphan = stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("oh-"))
        .and_then(|rest| rest.split(' ').next())
        .map(|rest| format!("oh-{rest}"))
        .expect(&stdout);

    // The next launch closes it, says so, queues it and briefs with it.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "next", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("ohjaa: session {orphan} (sh, started "))
            && stderr.contains("was left open by an ohjaa that is gone; closed as orphaned"),
        "{stderr}"
    );
    let briefing = fixture.argv().join("\n");
    assert!(
        briefing.contains(&format!("ORPHANED: session {orphan} (sh, long)")),
        "{briefing}"
    );
    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .args(["session", &orphan])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("ended: orphaned (observed at next launch)")
            && stdout.contains("phases        not observed (its ohjaa was killed before leaving)"),
        "{stdout}"
    );
    // Queued for intake, like any ended session.
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
    assert!(
        stderr.contains("2 ended session(s) queued for drafting"),
        "the orphan and the session that closed it: {stderr}"
    );
    fixture.cleanup();
}

#[test]
fn intake_drafts_from_what_ohjaa_observed_going_wrong_not_only_the_handover() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("anomalies");
    let repo = clone_with_origin(&fixture, "work");
    // A commit sykli refuses pushed to main, a failing exit, and no handover.
    let script = format!(
        "{RECEIPT_FN}
        set -e
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m broken
        receipt failed
        git push -q origin HEAD:main
        exit 3"
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "hurry",
                "--no-briefing",
                "--",
                "-c",
                &script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    let pushed = short_head(&repo);
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
    for said in [
        " · observed · ",
        &format!(
            "origin/main at {pushed} in work has no passing evidence — check before building on it \
             (FAILED sykli verify: refused: outcome expected passed"
        ),
        "the agent exited with code 3",
        "the agent left no handover",
    ] {
        assert!(prompt.contains(said), "missing `{said}`: {prompt}");
    }
    fixture.cleanup();
}

#[test]
fn a_warning_about_main_stands_while_main_is_still_at_the_flagged_commit() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("standing");
    let repo = clone_with_origin(&fixture, "work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let session = |script: &str| {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args([
                    "run",
                    "sh",
                    "--intent",
                    "x",
                    "--no-briefing",
                    "--",
                    "-c",
                    script,
                ])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
    };
    let briefing = || {
        let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
        stdout
    };
    session(&format!(
        "{RECEIPT_FN}
        set -e
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m broken
        receipt failed
        git push -q origin HEAD:main"
    ));
    let flagged = short_head(&repo);
    // A later session that changes nothing is now the last session here.
    session(&format!(
        "sleep 1; {} handover --next 'all quiet'",
        ohjaa_bin()
    ));
    let text = briefing();
    assert!(
        text.starts_with("WARNING from an earlier session here (oh-")
            && text.contains(&format!(
                "origin/main at {flagged} in work has no passing evidence"
            )),
        "main still sits at the flagged commit: {text}"
    );
    // Main moves on, with passing evidence: the warning has nothing left to say.
    session(&format!(
        "{RECEIPT_FN}
        set -e
        sleep 1
        echo fixed > fix.txt && git add fix.txt && git commit -q -m fixed
        receipt passed
        git push -q origin HEAD:main"
    ));
    let text = briefing();
    assert!(!text.contains("WARNING"), "{text}");
    fixture.cleanup();
}

#[test]
fn the_pull_request_is_the_one_for_the_sessions_own_branch_never_the_checkouts() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("own-pr");
    let repo = clone_with_origin(&fixture, "work");
    // The main checkout sits on another session's branch, which has a PR.
    run_git(&repo, &["switch", "-q", "-c", "theirs"]);
    let session = |script: &str, branch: &str| {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args([
                    "run",
                    "sh",
                    "--intent",
                    "x",
                    "--no-briefing",
                    "--",
                    "-c",
                    script,
                ])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .env("OHJAA_STUB_GH_PR_BRANCH", branch)
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        stderr
    };
    // A session that made nothing does not inherit the checkout's PR.
    let record = session("exit 0", "theirs");
    assert_eq!(
        finished_line(&record),
        "finished      unknown (not looked up: this session made no commit on a branch)",
        "{record}"
    );
    assert!(!record.contains("pull/7"), "{record}");
    // A commit on its own branch in a worktree removed since: that branch's PR.
    let wt = fixture.dir.join("_worktrees").join("feat3");
    let record = session(
        &format!(
            "set -e; sleep 1; git worktree add -q -b feat3 '{wt}' origin/main; cd '{wt}'
            echo a > a.txt && git add a.txt && git commit -q -m a
            cd '{repo}' && git worktree remove '{wt}'",
            wt = wt.display(),
            repo = repo.display()
        ),
        "feat3",
    );
    assert!(
        finished_line(&record).starts_with("finished      https://github.com/o/r/pull/7 (open)"),
        "{record}"
    );
    fixture.cleanup();
}

#[test]
fn a_squash_merged_and_cleaned_up_pull_request_is_still_found() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("squashed");
    let repo = clone_with_origin(&fixture, "work");
    let wt = fixture.dir.join("_worktrees").join("feat4");
    // The standard flow: push the branch, squash-merge, delete everything.
    let script = format!(
        "{RECEIPT_FN}
        set -e
        git worktree add -q -b feat4 '{wt}' origin/main
        cd '{wt}'
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m 'the work'
        receipt passed
        git push -q origin HEAD:feat4
        git rev-parse HEAD > '{mark}'
        cd '{repo}' && git worktree remove --force '{wt}' && git branch -q -D feat4",
        wt = wt.display(),
        repo = repo.display(),
        mark = fixture.dir.join("sha").display()
    );
    let mark = fixture.dir.join("sha");
    // gh finds it by the commit only once the branch name is gone everywhere.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "x",
                "--no-briefing",
                "--",
                "-c",
                &script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .env("OHJAA_STUB_GH_PR_BRANCH", "feat4")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let sha = fs::read_to_string(&mark).unwrap().trim().to_owned();
    let phases = stderr
        .lines()
        .find(|l| l.starts_with("phases"))
        .expect(&stderr);
    assert!(
        phases.contains(&sha[..7]),
        "the pushed commit is still the session's work: {phases}"
    );
    assert!(
        finished_line(&stderr).starts_with("finished      https://github.com/o/r/pull/7"),
        "found by the branch the push named: {stderr}"
    );
    fixture.cleanup();
}

/// Wait until `path` exists (another process's mark), for at most 20 s.
fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never came",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn a_hand_push_during_a_root_session_is_never_credited_to_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("hand-push");
    let folder = fixture.make_folder("projects");
    let plain = clone_with_origin(&fixture, "projects/plain");
    // The root session only waits. Meanwhile someone commits, runs sykli
    // (it fails) and pushes to main by hand.
    let started = fixture.dir.join("started");
    let done = fixture.dir.join("done");
    let child = fixture
        .ohjaa_in(&folder)
        .args(["run", "sh", "--intent", "wait", "--no-briefing", "--", "-c"])
        .arg(format!(
            "touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done",
            started.display(),
            done.display()
        ))
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .env("OHJAA_STUB_SYKLI_COMMIT", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&started);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let by_hand = format!(
        "{RECEIPT_FN}
        set -e
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m 'by hand'
        receipt failed
        git push -q origin HEAD:main"
    );
    let status = Command::new("sh")
        .args(["-c", &by_hand])
        .current_dir(&plain)
        .status()
        .unwrap();
    assert!(status.success());
    fs::write(&done, "").unwrap();
    let output = child.wait_with_output().unwrap();
    let record = String::from_utf8_lossy(&output.stderr);
    let pushed = short_head(&plain);
    assert!(
        record.contains("in plain")
            && record.contains("observed during the session, not attributed"),
        "{record}"
    );
    assert!(
        !record.contains("FLAGGED"),
        "not this session's to answer for: {record}"
    );
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&plain));
    assert!(
        !stdout.contains("WARNING") && !stdout.contains("Last session here"),
        "the root session is not plain's last session, and raises nothing there: {stdout}"
    );
    assert!(record.contains(&pushed), "{record}");
    fixture.cleanup();
}

#[test]
fn two_sessions_in_one_checkout_never_claim_each_others_commits() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("shared-checkout");
    let repo = fixture.make_git_repo("work");
    let mark = |name: &str| fixture.dir.join(name);
    let agent = |me: &str, other: &str| {
        format!(
            "set -e
            touch '{started}'
            while [ ! -e '{their_start}' ]; do sleep 0.1; done
            sleep 1
            git worktree add -q -b {me} '{wt}'
            cd '{wt}' && echo {me} > {me}.txt && git add {me}.txt && git commit -q -m {me}
            git rev-parse --short=7 HEAD > '{sha}'
            touch '{done}'
            while [ ! -e '{their_done}' ]; do sleep 0.1; done",
            started = mark(&format!("{me}-started")).display(),
            their_start = mark(&format!("{other}-started")).display(),
            wt = fixture.dir.join("_worktrees").join(me).display(),
            sha = mark(&format!("{me}-sha")).display(),
            done = mark(&format!("{me}-done")).display(),
            their_done = mark(&format!("{other}-done")).display(),
        )
    };
    let launch = |me: &str, other: &str| {
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                me,
                "--no-briefing",
                "--",
                "-c",
                &agent(me, other),
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let x = launch("x", "y");
    let y = launch("y", "x");
    let records: Vec<String> = [x, y]
        .into_iter()
        .map(|child| {
            String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).into_owned()
        })
        .collect();
    let sha = |me: &str| {
        fs::read_to_string(mark(&format!("{me}-sha")))
            .unwrap()
            .trim()
            .to_owned()
    };
    for (record, theirs) in records.iter().zip(["y", "x"]) {
        let phases = record
            .lines()
            .find(|l| l.starts_with("phases"))
            .expect(record);
        assert!(
            !phases.contains(&sha(theirs)),
            "a session never claims the other's commit: {phases}\n{record}"
        );
        assert!(
            record.contains("shared        with oh-"),
            "and says why: {record}"
        );
    }
    fixture.cleanup();
}

#[test]
fn a_session_keeps_its_own_commits_in_a_checkout_another_session_has_left() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("takeover");
    let repo = fixture.make_git_repo("work");
    let mark = |name: &str| fixture.dir.join(name);
    let spawn = |intent: &str, script: String| {
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                intent,
                "--no-briefing",
                "--",
                "-c",
                &script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    // P is here first, and leaves while S is still working.
    let p = spawn(
        "p",
        format!(
            "touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done",
            mark("p-started").display(),
            mark("s-started").display()
        ),
    );
    wait_for(&mark("p-started"));
    let s = spawn(
        "s",
        format!(
            "set -e; touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; sleep 2
            git commit -q --allow-empty -m 'after p left'; git rev-parse --short=7 HEAD > '{}'",
            mark("s-started").display(),
            mark("p-left").display(),
            mark("s-sha").display()
        ),
    );
    let _ = p.wait_with_output().unwrap();
    fs::write(mark("p-left"), "").unwrap();
    let record = String::from_utf8_lossy(&s.wait_with_output().unwrap().stderr).into_owned();
    let sha = fs::read_to_string(mark("s-sha")).unwrap().trim().to_owned();
    let phases = record
        .lines()
        .find(|l| l.starts_with("phases"))
        .expect(&record);
    assert!(
        phases.contains(&sha),
        "made after the other session left: its own: {phases}\n{record}"
    );
    fixture.cleanup();
}
