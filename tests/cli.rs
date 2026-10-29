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
# The window between the spawn and the record: ohjaa killed at once.
if [ -n "${OHJAA_STUB_KILL_PARENT:-}" ] && [ "$1" = run ]; then kill -9 "$PPID"; fi
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
/// OHJAA_STUB_GH_PR_SEARCH only (or the sha in the file
/// OHJAA_STUB_GH_PR_SEARCH_FILE names). `api graphql` answers with the file
/// OHJAA_STUB_GH_GRAPHQL names, each call counted in `$HOME/gh-graphql-calls`;
/// without it, gh is not logged in.
const GH_STUB: &str = r#"#!/bin/sh
if [ -n "${OHJAA_STUB_GH_SLEEP:-}" ]; then sleep "$OHJAA_STUB_GH_SLEEP"; fi
printf '%s|%s\n' "$PWD" "$*" >> "${OHJAA_STUB_GH_CALLS:-/dev/null}"
if [ "$1 $2" = "api graphql" ] && [ -n "${OHJAA_STUB_GH_GRAPHQL:-}" ]; then
  echo "api graphql" >> "$HOME/gh-graphql-calls"
  cat "$OHJAA_STUB_GH_GRAPHQL"
  exit 0
fi
if [ "$1 $2" = "pr list" ]; then
  sha=""
  while [ $# -gt 0 ]; do case "$1" in --search) sha="$2"; shift;; esac; shift; done
  if [ -n "${OHJAA_STUB_GH_PR_SEARCH_FILE:-}" ] && [ -f "$OHJAA_STUB_GH_PR_SEARCH_FILE" ]; then
    OHJAA_STUB_GH_PR_SEARCH=$(cat "$OHJAA_STUB_GH_PR_SEARCH_FILE")
  fi
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
/// OHJAA_STUB_SYKLI_COMMIT set, and no sentence ohjaa could lean on.
/// `verify <receipt> --contract <c> --commit <sha>` judges the commit alone,
/// as every sykli with `--commit` does (sykli#67 as merged), checking in
/// sykli's order and words: the contract the receipt was made under, that it
/// ran exactly that commit's tree, cleanly, and then its outcome.
const SYKLI_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "${OHJAA_STUB_SYKLI_CALLS:-/dev/null}"
if [ "$1" = verify ] && [ "$2" = --help ]; then
  echo "Usage: sykli verify [OPTIONS] <RECEIPT>"
  echo "      --contract <CONTRACT>  Path to sykli.rs or a sykli-contract.v1 JSON file"
  if [ -n "${OHJAA_STUB_SYKLI_COMMIT:-}" ]; then
    echo "      --commit <REV>  Also require a clean run of this commit"
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
grep -q '"outcome":"passed"' "$receipt" || { echo "refused: outcome expected passed or cached with complete task records but got failed with 0 task records" >&2; exit 1; }
exit 0
"#;

/// Never the real vessa. Its header listing, `sessions --json --headers`,
/// prints the file OHJAA_STUB_VESSA_JSON names, else no sessions; it lists
/// no other sessions, so a report that asked for the full listing would
/// fail. `messages` lists, as
/// `vessa-messages.v1`, every line of `$HOME/vessa-messages.jsonl` (one
/// message each, as vessa writes them), whatever its `--since`: intake
/// windows what it is given too. Each call's arguments go to
/// `$HOME/vessa-calls`. With `$HOME/vessa-gap` or `$HOME/vessa-warning` (each
/// a JSON string) the list is partial, with that gap (a store vessa does not
/// read) or source warning (one it could not read), and it exits 4. With
/// `$HOME/vessa-old` it is a vessa older than `messages`. `usage` prints
/// `$HOME/vessa-usage.json`, else no sessions, its arguments logged too.
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
if [ "$1" = usage ]; then
  printf '%s\n' "$*" >> "$HOME/vessa-calls"
  if [ -f "$HOME/vessa-usage.json" ]; then cat "$HOME/vessa-usage.json"
  else echo '{"schema":"vessa-usage.v2","sessions":[],"warnings":[]}'; fi
  exit 0
fi
[ "$*" = "sessions --json --headers" ] || { echo "vessa stub: not the header listing: $*" >&2; exit 64; }
if [ -n "${OHJAA_STUB_VESSA_JSON:-}" ]; then
  cat "$OHJAA_STUB_VESSA_JSON"
else
  echo '{"schema":"vessa-session-headers.v1","sessions":[],"warnings":[]}'
fi
"#;

/// A source warning from vessa (`$HOME/vessa-warning`): a transcript it could
/// not read.
const UNREADABLE: &str = r#""/h/.codex/sessions/r.jsonl: Permission denied (os error 13)""#;

/// A gap from vessa (`$HOME/vessa-gap`): a store it knows of and does not
/// read, written in the window, as on a machine where opencode is in use.
const UNREAD_STORE: &str = r#""opencode: /h/.local/share/opencode/opencode.db not read; opencode keeps its sessions in SQLite, which Vessa does not read""#;

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
        write_executable(&stub, STUB);
        // Never the real gh: no network, and a controlled answer.
        let gh = stub_dir.join("gh");
        write_executable(&gh, GH_STUB);
        // Never the real sykli: `verify --help` names --commit only on demand.
        let sykli = stub_dir.join("sykli");
        write_executable(&sykli, SYKLI_STUB);
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
         block -->\nKnowledge (ohjaa, mine and machine): what the person you work for has \
         decided and knows. Follow it; if an entry looks wrong or stale, say so instead of \
         quietly working around it. Propose changes with `ohjaa propose`; only a person makes \
         them current.\n{}\n<!-- ohjaa:knowledge end -->\n",
        lines.join("\n")
    )
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
    // Its first line is the fixed sentinel vessa cuts a launch's task at
    // (byte for byte: vessa matches it, so a change is a new version).
    assert_eq!(argv[task + 3], "<!-- ohjaa-briefing v1 -->", "{argv:?}");
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
    let current = applied_block(&["- decision: Rust only.", "- fact: Lima is here."]);
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
            "{KNOWLEDGE_PREAMBLE}{ours_part}\nMine and machine (2 nodes) are not repeated here: \
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
            "{KNOWLEDGE_PREAMBLE}{ours_part}\nThis machine:\n- [fact] Lima is here.\n  \
             (lima · as of 2026-09-20)\n  ! may no longer hold: \
             command:no-such-command-ohjaa-test is false\n\nMine and machine (2 nodes) are not \
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
            "{full}\nMine and machine are carried here, not left to {agent}'s own \
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
    let ours_and_line = |file: &Path| {
        format!(
            "{KNOWLEDGE_PREAMBLE}{ours_part}\nMine and machine (2 nodes) are not repeated here: \
             Codex has them from its own instructions, {}, whose ohjaa block is current.\n",
            file.display()
        )
    };
    assert_eq!(brief("codex"), ours_and_line(&codex));
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

/// `ohjaa run` decides it for the agent it launches, and the session log
/// says which nodes the agent's own file carried.
#[test]
fn run_leaves_out_what_the_agents_own_file_holds_and_records_where_it_was() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("run-own-file");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "team",
        "decision",
        "Team rule.",
    );
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust",
        "decision",
        "Rust only.",
    );
    fixture.node(
        &fixture.dir.join("config/ohjaa/machine"),
        "lima",
        "fact",
        "Lima is here.",
    );
    let file = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        applied_block(&["- decision: Rust only.", "- fact: Lima is here."]),
    )
    .unwrap();
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "x"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let briefing = fixture.argv().join("\n");
    assert!(
        briefing.contains(&format!(
            "\nMine and machine (2 nodes) are not repeated here: Claude Code has them from its \
             own instructions, {}, whose ohjaa block is current.\n",
            file.display()
        )) && briefing.contains("- [decision] Team rule.")
            && !briefing.contains("Rust only.")
            && !briefing.contains("Lima is here."),
        "{briefing}"
    );
    let (export, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&export).unwrap();
    assert_eq!(
        export["sessions"][0]["delivered"],
        serde_json::json!([
            {"id": "team", "scope": "ours", "as_of": "2026-09-20"},
            {"id": "rust", "scope": "mine", "as_of": "2026-09-20", "via": file.to_str().unwrap()},
            {"id": "lima", "scope": "machine", "as_of": "2026-09-20", "via": file.to_str().unwrap()},
        ])
    );
    fixture.cleanup();
}

/// The agent's block is current but over its budget: 41 of 42 nodes and
/// `1 more`. The one it cuts is carried by the briefing, never by nothing,
/// and the one line says how many the file holds and how many follow.
#[test]
fn a_node_the_agents_block_cuts_is_carried_by_the_briefing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("block-cut");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
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
    let lima = "Lima is here. ".repeat(25).trim_end().to_owned();
    fixture.node_as_of(&machine, "lima", "fact", "2026-09-10", &lima);
    // What apply writes: every decision, every taste, and the oldest fact
    // by count, over the 8,000 characters.
    lines.push("1 more: `ohjaa list`".to_owned());
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let file = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, applied_block(&lines)).unwrap();
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
            "{KNOWLEDGE_PREAMBLE}\nThis machine:\n- [fact] {lima}\n  (lima · as of 2026-09-10)\n\
             \nMine and machine: Claude Code has 41 of their 42 nodes from its own instructions, \
             {}, whose ohjaa block is current; the other 1 is above.\n",
            file.display()
        )
    );
    // The session log: the 41 by the file, the one by the briefing.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "x"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let (export, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&export).unwrap();
    let delivered = export["sessions"][0]["delivered"].as_array().unwrap();
    let via: Vec<(&str, Option<&str>)> = delivered
        .iter()
        .map(|node| (node["id"].as_str().unwrap(), node["via"].as_str()))
        .collect();
    assert_eq!(via.len(), 42, "{via:?}");
    assert_eq!(via[41], ("lima", None));
    assert!(
        via[..41]
            .iter()
            .all(|(_, by)| *by == Some(file.to_str().unwrap())),
        "{via:?}"
    );
    fixture.cleanup();
}

/// The file holds the block, current, and still the agent will not read it:
/// its own arguments or environment say so. Then the briefing carries mine
/// and machine, and its one line says why. `CLAUDE_CONFIG_DIR` and
/// `CODEX_HOME` move the file looked at, as they move the one the agent reads.
#[test]
fn an_agent_that_will_not_read_its_own_file_is_briefed_in_full() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("not-read");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust",
        "decision",
        "Rust only.",
    );
    let current = applied_block(&["- decision: Rust only."]);
    let file = fixture.dir.join("home/.claude/CLAUDE.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, &current).unwrap();
    let left_out = |agent: &str, file: &Path| {
        format!(
            "Mine and machine (1 node) are not repeated here: {agent} has them from its own \
             instructions, {}, whose ohjaa block is current.",
            file.display()
        )
    };
    let carried = |why: &str| {
        format!(
            "Mine and machine are carried here, not left to Claude Code's own instructions: \
             {why}."
        )
    };
    // What `ohjaa run claude -- <args>` briefs: its one line about mine and
    // machine, and whether they are in it.
    let run = |args: &[&str]| {
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["run", "claude", "--intent", "x", "--"])
                .args(args)
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        let briefing = fixture.argv().join("\n");
        let line = briefing
            .lines()
            .find(|line| line.starts_with("Mine and machine"))
            .unwrap_or_else(|| panic!("no line about mine and machine: {briefing}"))
            .to_owned();
        (line, briefing.contains("- [decision] Rust only."))
    };
    assert_eq!(run(&[]), (left_out("Claude Code", &file), false));
    for (args, why) in [
        (
            &["--bare"][..],
            "`--bare` is passed to Claude Code, which then reads no CLAUDE.md".to_owned(),
        ),
        (
            &["--model", "opus", "--safe-mode"][..],
            "`--safe-mode` is passed to Claude Code, which then reads no CLAUDE.md".to_owned(),
        ),
        (
            &["--setting-sources", "project,local"][..],
            format!(
                "`--setting-sources project,local` leaves out `user`, so Claude Code does not \
                 read {}",
                file.display()
            ),
        ),
        (
            &["--setting-sources=project"][..],
            format!(
                "`--setting-sources=project` leaves out `user`, so Claude Code does not read {}",
                file.display()
            ),
        ),
    ] {
        assert_eq!(run(args), (carried(&why), true), "{args:?}");
    }
    // What the agent reads as its own: `user` among the sources, and
    // anything after its own `--`.
    for args in [
        &["--setting-sources", "user,project"][..],
        &["--print", "--", "--bare"][..],
    ] {
        assert_eq!(
            run(args),
            (left_out("Claude Code", &file), false),
            "{args:?}"
        );
    }
    // The environment, as Claude Code reads it, for `brief` as for `run`.
    let brief_with = |key: &str, value: &str| {
        let (stdout, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["brief", "--knowledge-only", "--agent", "claude"])
                .env(key, value)
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        stdout
            .lines()
            .find(|line| line.starts_with("Mine and machine"))
            .unwrap_or_else(|| panic!("{stdout}"))
            .to_owned()
    };
    assert_eq!(
        brief_with("CLAUDE_CODE_SIMPLE", "1"),
        carried("CLAUDE_CODE_SIMPLE=1 is in the environment, so Claude Code reads no CLAUDE.md")
    );
    assert_eq!(
        brief_with("CLAUDE_CODE_SAFE_MODE", "TRUE"),
        carried(
            "CLAUDE_CODE_SAFE_MODE=TRUE is in the environment, so Claude Code reads no CLAUDE.md"
        )
    );
    assert_eq!(
        brief_with("CLAUDE_CODE_SIMPLE", "0"),
        left_out("Claude Code", &file)
    );
    assert_eq!(
        brief_with("CLAUDE_CONFIG_DIR", ""),
        carried(&format!(
            "CLAUDE_CONFIG_DIR is set but empty, so Claude Code does not read {}",
            file.display()
        ))
    );
    let moved = fixture.dir.join("claude-config/CLAUDE.md");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::write(&moved, &current).unwrap();
    assert_eq!(
        brief_with(
            "CLAUDE_CONFIG_DIR",
            moved.parent().unwrap().to_str().unwrap()
        ),
        left_out("Claude Code", &moved)
    );
    fs::write(&moved, "# Moved\n").unwrap();
    assert_eq!(
        brief_with(
            "CLAUDE_CONFIG_DIR",
            moved.parent().unwrap().to_str().unwrap()
        ),
        carried(&format!(
            "{} holds no ohjaa block (`ohjaa apply` writes it)",
            moved.display()
        ))
    );
    let codex = fixture.dir.join("codex-home/AGENTS.md");
    fs::create_dir_all(codex.parent().unwrap()).unwrap();
    fs::write(&codex, &current).unwrap();
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["brief", "--knowledge-only", "--agent", "codex"])
            .env("CODEX_HOME", codex.parent().unwrap())
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(stdout, format!("{}\n", left_out("Codex", &codex)));
    fixture.cleanup();
}

/// pi and opencode as Claude Code: their own switches, before their own
/// `--` (pi 1.2.0: `--no-context-files`, `-nc`; opencode 2.1.4: `--server`,
/// whose server reads the instructions), and the folder opencode's
/// `OPENCODE_CONFIG_DIR` names. A folder that is not an absolute path is
/// resolved by the agent where it starts, so ohjaa cannot tell the file:
/// mine and machine are carried.
#[test]
fn every_agents_own_switch_and_a_relative_folder_carry_mine_and_machine() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("own-switches");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fixture.node(
        &fixture.dir.join("config/ohjaa/mine"),
        "rust",
        "decision",
        "Rust only.",
    );
    let current = applied_block(&["- decision: Rust only."]);
    let pi = fixture.dir.join("home/.pi/agent/AGENTS.md");
    let opencode = fixture.dir.join("config/opencode/AGENTS.md");
    for file in [&pi, &opencode] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, &current).unwrap();
    }
    let left_out = |agent: &str, file: &Path| {
        format!(
            "Mine and machine (1 node) are not repeated here: {agent} has them from its own \
             instructions, {}, whose ohjaa block is current.",
            file.display()
        )
    };
    let carried = |agent: &str, why: &str| {
        format!("Mine and machine are carried here, not left to {agent}'s own instructions: {why}.")
    };
    let run = |agent: &str, args: &[&str], env: &[(&str, &str)]| {
        let mut command = fixture.ohjaa();
        command
            .args(["run", agent, "--intent", "x", "--"])
            .args(args)
            .current_dir(&repo);
        for (key, value) in env {
            command.env(key, value);
        }
        let (_, stderr, code) = output_of(&mut command);
        assert_eq!(code, Some(0), "{stderr}");
        let briefing = fixture.argv().join("\n");
        let line = briefing
            .lines()
            .find(|line| line.starts_with("Mine and machine"))
            .unwrap_or_else(|| panic!("no line about mine and machine: {briefing}"))
            .to_owned();
        (line, briefing.contains("- [decision] Rust only."))
    };
    assert_eq!(run("pi", &[], &[]), (left_out("pi", &pi), false));
    for switch in ["--no-context-files", "-nc"] {
        assert_eq!(
            run("pi", &["--model", "x", switch], &[]),
            (
                carried(
                    "pi",
                    &format!(
                        "`{switch}` is passed to pi, which then reads no AGENTS.md or CLAUDE.md"
                    )
                ),
                true
            )
        );
    }
    assert_eq!(run("pi", &["--", "-nc"], &[]), (left_out("pi", &pi), false));
    // Without `--standalone`, opencode's background service reads its own
    // instructions, with the environment it was started with: here
    // XDG_CONFIG_HOME (the fixture's) moves the file, so it is carried.
    let service = |vars: &str| {
        carried(
            "opencode",
            &format!(
                "opencode hands the session to its background service unless `--standalone` is \
                 passed, and the service reads its own instructions with the environment it was \
                 started with, which may not set {vars} as this one does"
            ),
        )
    };
    assert_eq!(
        run("opencode", &[], &[]),
        (service("XDG_CONFIG_HOME"), true)
    );
    assert_eq!(
        run("opencode", &["--standalone"], &[]),
        (left_out("opencode", &opencode), false)
    );
    for (args, said) in [
        (
            &["--server", "http://127.0.0.1:4096"][..],
            "--server http://127.0.0.1:4096",
        ),
        (
            &["--server=http://127.0.0.1:4096"][..],
            "--server=http://127.0.0.1:4096",
        ),
    ] {
        assert_eq!(
            run("opencode", args, &[]),
            (
                carried(
                    "opencode",
                    &format!(
                        "`{said}` is passed to opencode, so a server ohjaa cannot see reads its \
                         own instructions"
                    )
                ),
                true
            )
        );
    }
    // OPENCODE_CONFIG_DIR moves opencode's own file, as it moves the one
    // opencode reads.
    let moved = fixture.dir.join("opencode-config/AGENTS.md");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::write(&moved, &current).unwrap();
    let moved_dir = [(
        "OPENCODE_CONFIG_DIR",
        moved.parent().unwrap().to_str().unwrap(),
    )];
    assert_eq!(
        run("opencode", &["--standalone"], &moved_dir),
        (left_out("opencode", &moved), false)
    );
    assert_eq!(
        run("opencode", &[], &moved_dir),
        (service("OPENCODE_CONFIG_DIR and XDG_CONFIG_HOME"), true)
    );
    // A folder that is not an absolute path: carried.
    for (agent, name, key, value, vars) in [
        (
            "claude",
            "Claude Code",
            "CLAUDE_CONFIG_DIR",
            "rel/claude",
            "CLAUDE_CONFIG_DIR",
        ),
        ("codex", "Codex", "CODEX_HOME", "rel/codex", "CODEX_HOME"),
        (
            "pi",
            "pi",
            "PI_CODING_AGENT_DIR",
            "~/pi",
            "PI_CODING_AGENT_DIR",
        ),
        (
            "opencode",
            "opencode",
            "OPENCODE_CONFIG_DIR",
            "rel/opencode",
            "OPENCODE_CONFIG_DIR, else XDG_CONFIG_HOME",
        ),
    ] {
        assert_eq!(
            run(agent, &[], &[(key, value)]),
            (
                carried(
                    name,
                    &format!(
                        "its folder, {value}, is not an absolute path ({vars}), so ohjaa cannot \
                         tell which file it reads"
                    )
                ),
                true
            ),
            "{agent}"
        );
    }
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
    let (ours, mine, machine) = (
        fixture.dir.join("ours/knowledge"),
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
        "Mine and machine: Claude Code has 41 of their 43 nodes from its own instructions, {}, \
         whose ohjaa block is current; the other 2 are above, 1 of them only by count.",
        file.display()
    );
    // Sized so that the briefing holds the team's taste and the newer fact,
    // and the older only by count.
    let team = "y".repeat(7000 - line.len());
    fixture.node(&ours, "team", "taste", &team);
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
            "{KNOWLEDGE_PREAMBLE}\nOurs: decided by the team, binding everywhere:\n\
             - [taste] {team}\n  (team · as of 2026-09-20)\n\nThis machine:\n- [fact] {vm}\n  \
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
        "Mine and machine are carried here, not left to pi's own instructions: {} does not \
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
            .filter(|l| l.ends_with("|context --identity --json"))
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
    // Its size, but not its numbers: a session id carries a pid, whose digits
    // can grow by one between two launches.
    let size = |text: &str| text.chars().filter(|c| !c.is_ascii_digit()).count();
    assert_eq!(size(&after), size(&before));
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
    // Intake's first real run, 2026-10-27: `git log` refuses a repository
    // with no commits yet, so the reverts of two such repositories counted
    // as unread, and held the cursor 336 hours back and the sessions queued
    // behind it.
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
    // The review of #61: git says "does not have any commits yet" for a
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
    // Intake's first real run, 2026-10-27, wrote 108 proposals, four of them
    // one lesson ("read back the briefing and leave a handover"). The review
    // of #61: refusing what looks repeated drops real lessons for good
    // (`tapio-linux-only` as `rauha-linux-only`), and a person is the only
    // one who decides. So each is written, and said to that person.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakedup");
    let repo = fixture.make_git_repo("work");
    let (rauha, tapio) = (fixture.make_folder("rauha"), fixture.make_folder("tapio"));
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
        &proposals.join("repo").join(encoded(&rauha)),
        "rauha-linux-only",
        "Rauha runs on Linux only.",
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
            &rauha.display().to_string(),
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
            drafted("repo", Some(&tapio), "tapio-linux-only", None),
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
    assert!(!first.contains("rauha-linux-only"), "{first}");
    assert!(
        second.contains(&format!(
            "- rauha-linux-only [repo/lesson/proposed] in {}: Rauha runs on Linux only.",
            rauha.display()
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
        "proposed tapio-linux-only\n",
        "proposed lima-for-linux\n",
        "9 proposal(s) waiting for review (`ohjaa review`), 4 of them possible duplicates, 0 \
         draft(s) refused",
        "possible duplicates: of read-back-briefing-and-leave-handover ×2, of rust-elixir-only \
         ×1, of start-sessions-through-ohjaa-run ×1",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }
    // Nothing drafted is lost: every draft waits for a person, and nothing
    // is stored beside it (the re-review of #61: a possible duplicate is
    // derived when a person reviews).
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
    let tapio_dir = proposals.join("repo").join(encoded(&tapio));
    assert!(tapio_dir.join("tapio-linux-only.md").is_file());
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
    // Stamped by the agent while it runs, as a real agent stamps one: after
    // the session started, however long ohjaa took to start it. (A stamp
    // taken here, before ohjaa runs, is fresh only while ohjaa starts within
    // its margin, and a loaded machine misses it.) The quotes close around
    // the stamp, so the shell expands it.
    let data = r#"{"schema":"agent-usage.v1","agent":"sh","session_id":"native-leaving","source":"fixture","observed_at_ms":OBSERVED,"complete":false,"limitations":["fixture"],"entries":[]}"#
        .replace("OBSERVED", "'\"$(date +%s)000\"'");
    let script = format!("printf '%s' '{data}' > \"$OHJAA_USAGE_FILE\"; exit 7");
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
    let session = &export["sessions"][0];
    assert_eq!(session["usage"][0]["session_id"], "native-leaving");
    // Fresh: observed within the session, never before it started.
    let observed = session["usage"][0]["observed_at_ms"].as_i64().unwrap();
    assert!(
        observed >= session["started_ms"].as_i64().unwrap()
            && observed <= session["left"]["ended_ms"].as_i64().unwrap(),
        "{session:#}"
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

/// A stand-in `claude`: it keeps its launch prompt in its transcript, as
/// Claude Code does, in the sandbox HOME. The real launch prompt is
/// toimija's, with ohjaa's briefing in it; the stub toimija passes none, so
/// this one says the briefing's handshake from `OHJAA_SESSION` itself
/// (with OHJAA_FAKE_NATIVE empty, it keeps no transcript). With
/// OHJAA_FAKE_ORIENTED or OHJAA_FAKE_HANDED set it then reads back or hands
/// over with `$OHJAA_FAKE_OHJAA`, from a shell whose CLAUDE_CODE_SESSION_ID is
/// that, as Claude Code's tool shells have it (`-`: none); two ids are a
/// `/clear` between them. With OHJAA_FAKE_KILL_OHJAA set it then kills the
/// ohjaa that launched it. Its records carry the time to the fraction of a
/// second (`%N`), as Claude Code's do: leaving compares them with the
/// launch's own instant.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
dir="$HOME/.claude/projects/-work"
mkdir -p "$dir"
now=$(date -u +%Y-%m-%dT%H:%M:%S.%NZ)
if [ -n "${OHJAA_FAKE_NATIVE:-}" ]; then
printf '{"type":"user","sessionId":"%s","cwd":"%s","timestamp":"%s","isSidechain":false,"message":{"role":"user","content":"How this session works (ohjaa):\\nThis session is %s; its intent is in-1, new."}}\n' \
  "$OHJAA_FAKE_NATIVE" "$(pwd -P)" "$now" "$OHJAA_SESSION" >> "$dir/$OHJAA_FAKE_NATIVE.jsonl"
fi
if [ "${OHJAA_FAKE_ORIENTED:-}" = "-" ]; then
  env -u CLAUDE_CODE_SESSION_ID "$OHJAA_FAKE_OHJAA" oriented 'goal: x; rules: y; nobody' || exit 9
elif [ -n "${OHJAA_FAKE_ORIENTED:-}" ]; then
  CLAUDE_CODE_SESSION_ID="$OHJAA_FAKE_ORIENTED" "$OHJAA_FAKE_OHJAA" oriented 'goal: x; rules: y; nobody' || exit 9
fi
if [ -n "${OHJAA_FAKE_HANDED:-}" ]; then
  CLAUDE_CODE_SESSION_ID="$OHJAA_FAKE_HANDED" "$OHJAA_FAKE_OHJAA" handover --next 'z' || exit 9
fi
if [ -n "${OHJAA_FAKE_KILL_OHJAA:-}" ]; then kill -9 "$PPID"; fi
exit 0
"#;

fn fake_claude(fixture: &Fixture) {
    let path = fixture.stub_dir.join("claude");
    write_executable(&path, FAKE_CLAUDE);
}

/// D7: leaving names the agent's own session, found by the launch prompt in
/// its transcript, in the record and in the export, as the receipts' are.
#[test]
fn leaving_names_the_agents_own_session_from_its_launch_prompt() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("native");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fake_claude(&fixture);
    let native = "5e1976e4-71c3-407c-9533-0be05b7e1b70";
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "native"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_FAKE_NATIVE", native)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("native        claude {native}\n")),
        "{stderr}"
    );
    let session = &export_of(&fixture, &repo)["sessions"][0];
    let found = &session["left"]["native"]["sessions"];
    assert_eq!(found.as_array().map(Vec::len), Some(1), "{session:#}");
    assert_eq!(found[0]["agent"], "claude");
    assert_eq!(found[0]["session_id"], native);
    assert!(
        found[0]["source"]
            .as_str()
            .unwrap()
            .ends_with(&format!(".claude/projects/-work/{native}.jsonl")),
        "{session:#}"
    );

    // An agent ohjaa reads no transcripts of is said not looked for.
    let (_, stderr, _) = run_session(&fixture, &repo, "exit 0");
    assert!(
        stderr.contains(
            "native        not looked for: ohjaa reads the transcripts of claude, codex and \
             pi, not sh"
        ),
        "{stderr}"
    );
    fixture.cleanup();
}

/// D7: a session closed as orphaned has its agent's own session found too,
/// by the launch that closes it.
#[test]
fn an_orphan_is_closed_with_its_agents_own_session_named() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("native-orphan");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fake_claude(&fixture);
    let native = "b4e4122a-4154-41f2-b642-c1a5632a7f0d";
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "killed"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_FAKE_NATIVE", native)
            .env("OHJAA_FAKE_KILL_OHJAA", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, None, "ohjaa was killed before leaving: {stderr}");
    let orphan = export_of(&fixture, &repo)["sessions"][0].clone();
    assert!(orphan["left"].is_null(), "never left: {orphan:#}");
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 0");
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("closed as orphaned"), "{stderr}");
    let sessions = export_of(&fixture, &repo)["sessions"].clone();
    let closed = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == orphan["id"])
        .unwrap();
    assert_eq!(closed["left"]["orphaned"], true, "{closed:#}");
    assert_eq!(
        closed["left"]["native"]["sessions"][0]["session_id"], native,
        "{closed:#}"
    );
    let (stdout, _, _) = output_of(
        fixture
            .ohjaa()
            .args(["session", orphan["id"].as_str().unwrap()])
            .current_dir(&repo),
    );
    assert!(
        stdout.contains(&format!("native        claude {native}\n")),
        "{stdout}"
    );
    fixture.cleanup();
}

/// `ohjaa run claude` with the stand-in claude, given these env settings:
/// the export record of the session this launch made, and leaving's stderr.
/// That session is the one the log did not hold before it, never the latest
/// by `started_ms`: a launch's time is a whole second, and launches a moment
/// apart share one.
fn run_fake_claude(
    fixture: &Fixture,
    repo: &Path,
    vars: &[(&str, &str)],
) -> (serde_json::Value, String) {
    let ids = |export: &serde_json::Value| -> Vec<serde_json::Value> {
        let sessions = export["sessions"].as_array().unwrap();
        sessions.iter().map(|s| s["id"].clone()).collect()
    };
    let before = ids(&export_of(fixture, repo));
    let mut command = fixture.ohjaa();
    command
        .args(["run", "claude", "--intent", "native env"])
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .env("OHJAA_FAKE_OHJAA", ohjaa_bin())
        .current_dir(repo);
    for (key, value) in vars {
        command.env(key, value);
    }
    let (_, stderr, code) = output_of(&mut command);
    assert_eq!(code, Some(0), "{stderr}");
    let export = export_of(fixture, repo);
    let made: Vec<&serde_json::Value> = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| !before.contains(&s["id"]))
        .collect();
    assert_eq!(made.len(), 1, "one session per launch: {export:#}");
    (made[0].clone(), stderr)
}

/// D7 by the environment: `oriented` and `handover`, run from the agent's
/// own shell, record the session its environment names. With the launch
/// prompt's it is one session named both ways; after a `/clear` (another id
/// at the handover) both are kept; with none in the environment nothing
/// changes; and a launch prompt and an environment naming different ones are
/// both kept, the disagreement said.
///
/// Each launch is a new Claude session, with a transcript of its own. A
/// launch given an earlier one's transcript would resume it, and ohjaa tells
/// a resume by whether the transcript began before the launch's second:
/// launches a moment apart share one, and the resume goes unseen.
#[test]
fn oriented_and_handover_record_the_agents_own_session_from_its_environment() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("native-env");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fake_claude(&fixture);
    // The session each launch's prompt is kept in (the fourth keeps none),
    // and `cleared`, the one a `/clear` starts: the environment's alone.
    let first = "0a4b8c3e-0000-4000-8000-000000000001";
    let cleared = "0a4b8c3e-0000-4000-8000-000000000002";
    let second = "0a4b8c3e-0000-4000-8000-000000000003";
    let third = "0a4b8c3e-0000-4000-8000-000000000004";
    let fifth = "0a4b8c3e-0000-4000-8000-000000000005";

    // Agreeing: one session, named by both.
    let (session, stderr) = run_fake_claude(
        &fixture,
        &repo,
        &[
            ("OHJAA_FAKE_NATIVE", first),
            ("OHJAA_FAKE_ORIENTED", first),
            ("OHJAA_FAKE_HANDED", first),
        ],
    );
    let found = session["left"]["native"]["sessions"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{session:#}");
    assert_eq!(found[0]["session_id"], first);
    assert_eq!(found[0]["env"], true);
    assert!(found[0]["source"].as_str().unwrap().ends_with(".jsonl"));
    assert!(session["left"]["native"]["disagreement"].is_null());
    assert_eq!(session["native_env"].as_array().map(Vec::len), Some(1));
    assert!(
        stderr.contains(&format!(
            "native        claude {first} (observed by its launch prompt; its environment claimed it too)\n"
        )),
        "{stderr}"
    );

    // A `/clear` between read-back and handover: the second is named by the
    // environment alone, and nothing disagrees.
    let (session, stderr) = run_fake_claude(
        &fixture,
        &repo,
        &[
            ("OHJAA_FAKE_NATIVE", second),
            ("OHJAA_FAKE_ORIENTED", second),
            ("OHJAA_FAKE_HANDED", cleared),
        ],
    );
    let ids: Vec<(&str, &str)> = session["left"]["native"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["session_id"].as_str().unwrap(),
                s["source"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(ids.len(), 2, "{session:#}");
    assert_eq!(ids[0].0, second);
    assert!(
        ids[0].1.ends_with(&format!("/{second}.jsonl")),
        "the first is observed by its launch prompt: {session:#}"
    );
    assert_eq!(ids[1], (cleared, "env"));
    assert!(session["left"]["native"]["disagreement"].is_null());
    assert!(
        stderr.contains(&format!(
            "native        claude {cleared} (claimed by the agent's environment, not observed)\n"
        )),
        "{stderr}"
    );

    // No id in the environment: as before, by the launch prompt alone.
    let (session, _) = run_fake_claude(
        &fixture,
        &repo,
        &[("OHJAA_FAKE_NATIVE", third), ("OHJAA_FAKE_ORIENTED", "-")],
    );
    assert!(session["readback"].is_object(), "it read back: {session:#}");
    assert!(session["native_env"].is_null(), "{session:#}");
    let found = &session["left"]["native"]["sessions"][0];
    assert_eq!(found["session_id"], third, "{session:#}");
    assert!(found["env"].is_null(), "{session:#}");

    // Only the environment: a launch prompt never kept (late, or cleared
    // before a transcript was written) still names the session.
    let (session, _) = run_fake_claude(
        &fixture,
        &repo,
        &[("OHJAA_FAKE_NATIVE", ""), ("OHJAA_FAKE_ORIENTED", cleared)],
    );
    let found = &session["left"]["native"]["sessions"];
    assert_eq!(found.as_array().map(Vec::len), Some(1), "{session:#}");
    assert_eq!(found[0]["source"], "env");
    assert_eq!(
        found[0]["testimony"], true,
        "the agent's claim: {session:#}"
    );
    assert!(session["left"]["native"]["why"]
        .as_str()
        .unwrap()
        .starts_with("none found"));

    // Review of #57: an environment claiming another session than the
    // launch prompt was observed in (forged, or a `/clear` before the first
    // read-back). The observation is kept, the claim is said and not taken.
    let (session, stderr) = run_fake_claude(
        &fixture,
        &repo,
        &[("OHJAA_FAKE_NATIVE", fifth), ("OHJAA_FAKE_HANDED", cleared)],
    );
    let found = session["left"]["native"]["sessions"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{session:#}");
    assert_eq!(found[0]["session_id"], fifth);
    assert_eq!(
        session["native_env"][0]["session_id"], cleared,
        "the claim itself is kept as said: {session:#}"
    );
    let said = format!(
        "native        disagreement: its launch prompt named claude {fifth}, and the agent's \
         environment claimed claude {cleared} instead (a /clear before its first read-back, or \
         a mistake): the launch prompt's is kept, the claim is not\n"
    );
    assert!(stderr.contains(&said), "{stderr}");
    fixture.cleanup();
}

/// Review of #57: a shell that kept a session's OHJAA_SESSION after it left
/// (a tmux server started inside it) runs `ohjaa handover` in a session a
/// person started by hand. Its environment's id is not recorded on the
/// session that left, and the report still finds the bypass.
#[test]
fn an_id_named_after_leaving_is_not_recorded_and_the_bypass_is_still_found() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("native-env-stale");
    let (repo, _) = report_fixture(&fixture);
    let through = export_of(&fixture, &repo)["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["intent"] == "through ohjaa")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["handover", "--next", "from a shell that outlived it"])
            .env("OHJAA_SESSION", &through)
            .env("CLAUDE_CODE_SESSION_ID", "by-hand")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stdout.contains("recorded its claude session"), "{stdout}");
    assert!(
        stderr.contains("has left: the id its environment names is not recorded"),
        "{stderr}"
    );
    let record = export_of(&fixture, &repo)["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == through.as_str())
        .unwrap()
        .clone();
    assert!(record["native_env"].is_null(), "{record:#}");
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .arg("--root")
            .arg(&fixture.dir)
            .args(["report", "--json"])
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let bypassed: Vec<&str> = json["report"]["bypasses"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(bypassed, ["by-hand", "pi-by-hand"], "{stdout}");
    fixture.cleanup();
}

/// An orphan's leaving, written by a later launch, has what its agent's
/// environment named from inside it too.
#[test]
fn an_orphan_is_closed_with_what_its_agents_environment_named() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("native-env-orphan");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fake_claude(&fixture);
    let native = "0a4b8c3e-0000-4000-8000-0000000000aa";
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "killed"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_FAKE_OHJAA", ohjaa_bin())
            .env("OHJAA_FAKE_NATIVE", "")
            .env("OHJAA_FAKE_ORIENTED", native)
            .env("OHJAA_FAKE_KILL_OHJAA", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, None, "ohjaa was killed before leaving: {stderr}");
    assert!(
        stdout.contains(&format!(
            "recorded its claude session {native}, as the agent's environment claims it"
        )),
        "{stdout}"
    );
    let orphan = export_of(&fixture, &repo)["sessions"][0].clone();
    assert_eq!(orphan["native_env"][0]["session_id"], native, "{orphan:#}");
    let (stdout, _, _) = output_of(
        fixture
            .ohjaa()
            .args(["session", orphan["id"].as_str().unwrap()])
            .current_dir(&repo),
    );
    assert!(
        stdout.contains(&format!(
            "native        claude {native} (claimed by the agent's environment, not observed)\n"
        )),
        "still open, it says what its environment named: {stdout}"
    );
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 0");
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("closed as orphaned"), "{stderr}");
    let sessions = export_of(&fixture, &repo)["sessions"].clone();
    let closed = sessions
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == orphan["id"])
        .unwrap();
    let found = &closed["left"]["native"]["sessions"];
    assert_eq!(found[0]["session_id"], native, "{closed:#}");
    assert_eq!(found[0]["source"], "env", "{closed:#}");
    assert_eq!(found[0]["testimony"], true, "{closed:#}");
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

/// Without a terminal nothing but ohjaa can bring the session an interrupt
/// (it runs in a process group of its own): SIGINT sent to ohjaa reaches the
/// session's group once, never twice, and the session decides what it means.
/// With the terminal, Ctrl-C reaches the session directly (tests/terminal.rs).
#[test]
fn sigint_to_ohjaa_without_a_terminal_reaches_the_session_once() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("sigint");
    let repo = fixture.make_git_repo("work");
    let interrupts = fixture.dir.join("interrupts");
    let child = spawn_session(
        &fixture,
        &repo,
        &format!(
            "trap 'echo int >> \"{}\"' INT; sleep 2; exit 4",
            interrupts.display()
        ),
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    signal(child.id(), "INT");
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(4),
        "the session handled it, and ohjaa waited for its end: {stderr}"
    );
    assert!(stderr.contains("ended: exit 4"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&interrupts).unwrap_or_default(),
        "int\n",
        "the session got the interrupt, once"
    );
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
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    );
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

/// An ended session that said `count` corrections in its own window and
/// repository (all in one second), read back and handed over: queued for
/// intake, with `count + 1` candidates (and nothing for the report). Each is
/// `correction <n>. you forgot it`, which the pre-filter keeps.
fn session_that_said(fixture: &Fixture, repo: &Path, count: usize) {
    let script = format!(
        r#"sleep 1; here=$(pwd -P); at=$(date -u +%Y-%m-%dT%H:%M:%SZ); i=1
while [ $i -le {count} ]; do
  printf '{{"agent":"claude","session":"q","parent":null,"kind":"message","id":null,"at":"%s","cwd":"%s","text":"correction %s. you forgot it","question":null,"reply":null,"source":"/t/q.jsonl","line":%s}}\n' "$at" "$here" "$i" "$i" >> '{said}'
  i=$((i+1))
done
{ohjaa} oriented 'said corrections, then handed over' > /dev/null
{ohjaa} handover --surprised 'the queue never drained'"#,
        said = fixture.dir.join("home/vessa-messages.jsonl").display(),
        ohjaa = ohjaa_bin(),
    );
    let (_, stderr, code) = run_session(fixture, repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
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
fn intake_drains_a_queue_larger_than_one_run_and_never_moves_the_cursor_back() {
    // The 2026-10-26 audit: queued sessions' old candidates outnumbered a
    // run's cap, so every run drafted the same first ones, put the cursor
    // before where the run began, never marked a session drafted and exited
    // 4, forever.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakedrain");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 5);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    // The window starts after the session: all six candidates are older.
    let since = utc_now();
    let sent = fixture.dir.join("sent.txt");
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ngrep '^    said: ' >> '{}'\necho '[]'\n",
            sent.display()
        ),
    );
    let cursor_path = fixture.dir.join("state/ohjaa/intake-since");
    let mut cursors: Vec<String> = Vec::new();
    let mut runs = 0;
    loop {
        runs += 1;
        assert!(runs <= 5, "intake never finished: {:?}", said_lines(&sent));
        let before = said_lines(&sent).len();
        let mut command = fixture.ohjaa();
        command.arg("intake");
        if runs == 1 {
            command.args(["--since", &since]);
        }
        command
            .args(["--batch", "1", "--max-batches", "2", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo);
        let (stdout, stderr, code) = output_of(&mut command);
        let sent_now = said_lines(&sent).len() - before;
        assert!(sent_now <= 2, "a run takes at most its cap: {stderr}");
        let cursor = fs::read_to_string(&cursor_path).unwrap_or_default();
        let cursor = cursor.trim().to_owned();
        assert!(
            cursor.is_empty() || cursor >= since,
            "run {runs}: the cursor {cursor} is before where the run began, {since}"
        );
        if let Some(last) = cursors.last() {
            assert!(cursor >= *last, "run {runs}: the cursor moved back");
        }
        cursors.push(cursor);
        if stdout.contains("nothing new") {
            assert_eq!(code, Some(0), "{stderr}");
            assert_eq!(sent_now, 0, "{stderr}");
            assert!(
                !stderr.contains("queued for drafting"),
                "the session is drafted: {stderr}"
            );
            break;
        }
        assert!(sent_now >= 1, "run {runs} made no progress: {stderr}");
        let done = said_lines(&sent).len() == 6;
        assert_eq!(
            code,
            Some(if done { 0 } else { 4 }),
            "4 while work is left for the next run: {stderr}"
        );
    }
    let mut lines = said_lines(&sent);
    assert_eq!(lines.len(), 6, "each candidate once: {lines:?}");
    lines.sort();
    lines.dedup();
    assert_eq!(lines.len(), 6, "no candidate twice: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("the queue never drained")));
    assert_eq!(runs, 4, "three runs of two, then nothing left");
    fixture.cleanup();
}

#[test]
fn a_drafter_failing_mid_run_keeps_what_was_drafted_and_the_next_run_retries_the_rest() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakeresume");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 5);
    let (sent, failed, count) = (
        fixture.dir.join("sent.txt"),
        fixture.dir.join("failed.txt"),
        fixture.dir.join("count"),
    );
    // Its second and third calls fail (two candidates failing alone in a row
    // stop the run); every other one answers that nothing is there.
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\nn=$(($(cat '{count}' 2>/dev/null || echo 0) + 1)); echo $n > '{count}'\n\
             if [ $n -eq 2 ] || [ $n -eq 3 ]; then grep '^    said: ' >> '{failed}'; echo boom >&2; exit 1; fi\n\
             grep '^    said: ' >> '{sent}'\necho '[]'\n",
            count = count.display(),
            failed = failed.display(),
            sent = sent.display(),
        ),
    );
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", "transcripts", "--batch", "1"])
                .args(["--max-batches", "3", "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("intake stopped"), "{stderr}");
    assert_eq!(said_lines(&sent).len(), 1, "the first batch was drafted");
    let failed = said_lines(&failed);
    assert_eq!(failed.len(), 2);

    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    let lines = said_lines(&sent);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[1..3],
        failed[..],
        "the next run starts from what failed, not from the beginning"
    );
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    let mut lines = said_lines(&sent);
    assert_eq!(lines.len(), 6, "{lines:?}");
    lines.sort();
    lines.dedup();
    assert_eq!(lines.len(), 6, "nothing drafted twice: {lines:?}");
    fixture.cleanup();
}

/// Wait for `path` to appear, at most ten seconds.
fn wait_for_file(path: &Path) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !path.exists() {
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    true
}

#[test]
fn a_second_intake_while_one_runs_says_so_and_sends_nothing() {
    // Two at once sent the same batch twice, and one of them failed on the
    // other's half-written progress.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakebusy");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 3);
    let (calls, started, release, sent) = (
        fixture.dir.join("drafter-calls.txt"),
        fixture.dir.join("started"),
        fixture.dir.join("release"),
        fixture.dir.join("sent.txt"),
    );
    // Holds its first answer until released (ten seconds at most).
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\necho call >> '{calls}'\ntouch '{started}'\ni=0\n\
             while [ ! -e '{release}' ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done\n\
             grep '^    said: ' >> '{sent}'\necho '[]'\n",
            calls = calls.display(),
            started = started.display(),
            release = release.display(),
            sent = sent.display(),
        ),
    );
    let intake = || {
        let mut command = fixture.ohjaa();
        command
            .args(["intake", "--source", "transcripts", "--drafter"])
            .arg(&drafter)
            .current_dir(&repo);
        command
    };
    let first = intake()
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wait_for_file(&started), "the first intake never asked");
    let (_, stderr, code) = output_of(&mut intake());
    fs::write(&release, "").unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(code, Some(3), "refused: {stderr}");
    assert!(stderr.contains("intake already running"), "{stderr}");
    assert_eq!(said_lines(&calls).len(), 1, "the second asked nothing");
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let mut lines = said_lines(&sent);
    lines.sort();
    lines.dedup();
    assert_eq!((said_lines(&sent).len(), lines.len()), (4, 4));
    fixture.cleanup();
}

#[test]
fn a_candidate_the_drafter_always_fails_on_is_isolated_and_set_aside_out_loud() {
    // One poison candidate failed its whole batch on every run: exit 4, no
    // progress, forever.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakepoison");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 5);
    let sent = fixture.dir.join("sent.txt");
    // Refuses, in prose, any batch that holds `correction 3`.
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\nprompt=$(cat)\n\
             case \"$prompt\" in *'said: correction 3'*) echo 'I cannot help with that.'; exit 0;; esac\n\
             printf '%s\\n' \"$prompt\" | grep '^    said: ' >> '{}'\necho '[]'\n",
            sent.display()
        ),
    );
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", "transcripts", "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    // The first run drafts from everything around it.
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    assert_eq!(said_lines(&sent).len(), 5, "all but the one: {stderr}");
    assert!(stderr.contains("attempt 1 of 3"), "{stderr}");
    // Each run after it tries the one alone again, until it is set aside,
    // said with its time and source, never its words.
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("attempt 2 of 3"), "{stderr}");
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(4), "{stderr}");
    assert!(stderr.contains("set aside"), "{stderr}");
    assert!(stderr.contains("claude"), "its source: {stderr}");
    assert!(
        !stderr.contains("correction 3"),
        "never its words: {stderr}"
    );
    // Then nothing is left, and the set-aside one is still said, by its time
    // and source; its session stays queued, so a retry can find it.
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert!(stderr.contains("1 candidate(s) set aside"), "{stderr}");
    assert!(stderr.contains("Z claude"), "{stderr}");
    assert!(stderr.contains("queued for drafting"), "{stderr}");
    let mut lines = said_lines(&sent);
    lines.sort();
    lines.dedup();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("correction 3")));
    fixture.cleanup();
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
fn a_store_vessa_does_not_read_is_said_every_run_and_holds_neither_cursor_nor_queue() {
    // The review of #56: an opencode store written in the window held the
    // cursor, and every queued session, until vessa could read SQLite; the
    // hold kept the window before each day's write, so it never ended.
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakestore");
    let repo = fixture.make_git_repo("work");
    let script = format!("{} handover --surprised 'the store text'", ohjaa_bin());
    let (_, stderr, code) = run_session(&fixture, &repo, &script);
    assert_eq!(code, Some(0), "{stderr}");
    fixture.said(&[vessa_line(
        &utc_now(),
        "/p/elsewhere",
        "i told you, never python",
        None,
    )]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(fixture.dir.join("home/vessa-gap"), UNREAD_STORE).unwrap();
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
    let said = "ohjaa: intake: vessa: opencode: /h/.local/share/opencode/opencode.db not read";
    let (stdout, stderr, code) = intake();
    assert_eq!(
        code,
        Some(0),
        "a store vessa does not read is no failure: {stderr}"
    );
    assert!(stderr.contains(said), "said: {stderr}");
    assert!(!stderr.contains("cursor stays"), "{stderr}");
    let sent_once = said_lines(&sent);
    assert!(
        sent_once.contains(&"    said: i told you, never python".to_owned())
            && sent_once.iter().any(|l| l.contains("the store text")),
        "the message, and the session's handover: {stderr}"
    );
    assert!(stdout.contains("0 proposal(s)"), "{stdout}");
    let first = fs::read_to_string(&cursor_path).unwrap();
    // Said again on the next run, which finds the session drafted and the
    // cursor moved past what was sent.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains(said), "said on every run: {stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert!(
        !stderr.contains("queued"),
        "the session is drafted: {stderr}"
    );
    assert_eq!(said_lines(&sent), sent_once, "nothing sent twice: {stderr}");
    let second = fs::read_to_string(&cursor_path).unwrap();
    assert!(second > first, "the cursor moves: {first} -> {second}");
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
fn an_intake_killed_mid_run_keeps_every_batch_answered_before_the_kill() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakekilled");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 5);
    let (sent, count, hung, pids) = (
        fixture.dir.join("sent.txt"),
        fixture.dir.join("count"),
        fixture.dir.join("hung"),
        fixture.dir.join("pids"),
    );
    // Its second call hangs, until ohjaa is killed under it.
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\nn=$(($(cat '{count}' 2>/dev/null || echo 0) + 1)); echo $n > '{count}'\n\
             if [ $n -eq 2 ]; then sleep 30 & echo \"$$ $!\" > '{pids}'; touch '{hung}'; wait; fi\n\
             grep '^    said: ' >> '{sent}'\necho '[]'\n",
            count = count.display(),
            pids = pids.display(),
            hung = hung.display(),
            sent = sent.display(),
        ),
    );
    let intake = || {
        let mut command = fixture.ohjaa();
        command
            .args([
                "intake",
                "--source",
                "transcripts",
                "--batch",
                "2",
                "--drafter",
            ])
            .arg(&drafter)
            .current_dir(&repo);
        command
    };
    let mut first = intake()
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_for_file(&hung), "the second call never came");
    first.kill().unwrap();
    first.wait().unwrap();
    for pid in fs::read_to_string(&pids).unwrap().split_whitespace() {
        let _ = Command::new("kill").args(["-9", pid]).status();
    }
    assert_eq!(said_lines(&sent).len(), 2, "the first batch was answered");
    let (_, stderr, code) = output_of(&mut intake());
    assert_eq!(code, Some(0), "{stderr}");
    let mut lines = said_lines(&sent);
    assert_eq!(lines.len(), 6, "{lines:?}");
    lines.sort();
    lines.dedup();
    assert_eq!(
        lines.len(),
        6,
        "nothing answered before the kill was sent again"
    );
    fixture.cleanup();
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
fn what_no_later_run_can_find_again_is_forgotten() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intakeforget");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 2);
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let handled = fixture.dir.join("state/ohjaa/intake-handled");
    let intake = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    // Every source, so the cursor moves and the session is drafted from.
    let (_, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(said_lines(&sent).len(), 3, "{stderr}");
    let kept = fs::read_to_string(&handled).unwrap();
    assert!(
        kept.ends_with("end 3\n"),
        "kept while this run found them: {kept}"
    );
    // The next run finds none of them again, so nothing is kept. (A second
    // later: a run starts on the second, and one started on the cursor's
    // second cannot move it, nor forget anything, until the next.)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (stdout, stderr, code) = intake();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert!(!handled.exists(), "{:?}", fs::read_to_string(&handled).ok());
    fixture.cleanup();
}

/// Intake's progress is replaced whole, written aside and renamed into
/// place, never truncated and written where it is, so a run killed while it
/// writes leaves the last whole file. The file the first run left is held
/// open across the second: a file held open keeps its inode, so the one the
/// second run writes cannot be handed that number again (ext4 hands a freed
/// one straight back, and a run writes its progress twice). The path then
/// naming another file, and the held one still saying what the first run
/// wrote, show that it was replaced, not written in place.
#[test]
fn progress_is_replaced_whole_never_written_in_place() {
    if !git_available() {
        return;
    }
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new("intakewhole");
    let repo = fixture.make_git_repo("work");
    session_that_said(&fixture, &repo, 2);
    let sent = fixture.dir.join("sent.txt");
    let drafter = recording_drafter(&fixture, &sent);
    let handled = fixture.dir.join("state/ohjaa/intake-handled");
    let one_batch = || {
        output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", "transcripts", "--batch", "1"])
                .args(["--max-batches", "1", "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        )
    };
    one_batch();
    let mut held = fs::File::open(&handled).unwrap();
    let first = fs::read_to_string(&handled).unwrap();
    assert!(first.ends_with("end 1\n"), "{first}");
    one_batch();
    assert_ne!(
        fs::metadata(&handled).unwrap().ino(),
        held.metadata().unwrap().ino(),
        "written aside and renamed into place"
    );
    let mut kept = String::new();
    held.read_to_string(&mut kept).unwrap();
    assert_eq!(kept, first, "the first file was never written in place");
    assert!(!handled.with_extension("partial").exists());
    assert!(fs::read_to_string(&handled).unwrap().ends_with("end 2\n"));
    fixture.cleanup();
}

/// The second review's setup: an ended session that said three corrections,
/// and a correction said outside any session, after it. A drafter refuses, in
/// prose, any batch holding `correction 2` or the outside one while `refuse`
/// exists; it records every `said:` line it is sent in `tried`, and those it
/// answers for in `sent`. Three runs of every source set both aside, and the
/// cursor moves past them.
struct Poisoned {
    fixture: Fixture,
    repo: PathBuf,
    drafter: PathBuf,
    refuse: PathBuf,
    tried: PathBuf,
    sent: PathBuf,
}

impl Poisoned {
    fn new(tag: &str) -> Poisoned {
        let fixture = Fixture::new(tag);
        let repo = fixture.make_git_repo("work");
        session_that_said(&fixture, &repo, 3);
        fixture.said(&[vessa_line(
            &utc_now(),
            "/p/elsewhere",
            "the stubborn one. you forgot it",
            None,
        )]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let (refuse, tried, sent) = (
            fixture.dir.join("refuse"),
            fixture.dir.join("tried.txt"),
            fixture.dir.join("sent.txt"),
        );
        fs::write(&refuse, "").unwrap();
        let drafter = fixture.stub_dir.join("drafter");
        write_executable(
            &drafter,
            format!(
                "#!/bin/sh\nprompt=$(cat)\nprintf '%s\\n' \"$prompt\" | grep '^    said: ' >> '{tried}'\n\
                 if [ -e '{refuse}' ]; then case \"$prompt\" in\n\
                 *'said: correction 2'*|*'said: the stubborn one'*) echo 'I cannot help with that.'; exit 0;;\n\
                 esac; fi\n\
                 printf '%s\\n' \"$prompt\" | grep '^    said: ' >> '{sent}'\necho '[]'\n",
                tried = tried.display(),
                refuse = refuse.display(),
                sent = sent.display(),
            ),
        );
        let poisoned = Poisoned {
            fixture,
            repo,
            drafter,
            refuse,
            tried,
            sent,
        };
        for run in 1..=3 {
            let (_, stderr, code) = poisoned.intake(&[]);
            assert_eq!(code, Some(4), "run {run}: {stderr}");
        }
        let (_, stderr, _) = poisoned.intake(&[]);
        assert!(stderr.contains("2 candidate(s) set aside"), "{stderr}");
        poisoned
    }

    fn intake(&self, extra: &[&str]) -> (String, String, Option<i32>) {
        // A second apart: the cursor is kept in whole seconds of a run's start.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        output_of(
            self.fixture
                .ohjaa()
                .arg("intake")
                .args(extra)
                .arg("--drafter")
                .arg(&self.drafter)
                .current_dir(&self.repo),
        )
    }

    fn count(path: &Path, words: &str) -> usize {
        said_lines(path)
            .iter()
            .filter(|l| l.contains(words))
            .count()
    }
}

#[test]
fn retry_failed_sends_set_aside_candidates_again_after_the_cursor_passed_them() {
    if !git_available() {
        return;
    }
    let poisoned = Poisoned::new("intakeretry");
    // Set aside, each is still said by its time and source in every run.
    let (_, stderr, _) = poisoned.intake(&[]);
    assert_eq!(stderr.matches(" claude").count(), 2, "{stderr}");
    fs::remove_file(&poisoned.refuse).unwrap();
    let (_, stderr, code) = poisoned.intake(&["--retry-failed"]);
    assert_eq!(code, Some(0), "{stderr}");
    for words in ["said: correction 2", "said: the stubborn one"] {
        assert_eq!(
            Poisoned::count(&poisoned.sent, words),
            1,
            "{words}, answered once retried: {stderr}"
        );
    }
    // Answered: no longer set aside, and the session is drafted from.
    let (stdout, stderr, code) = poisoned.intake(&[]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing new"), "{stdout}");
    assert!(!stderr.contains("set aside"), "{stderr}");
    assert!(!stderr.contains("queued for drafting"), "{stderr}");
    poisoned.fixture.cleanup();
}

#[test]
fn a_retry_that_fails_again_keeps_them_set_aside_until_the_drafter_answers() {
    if !git_available() {
        return;
    }
    let poisoned = Poisoned::new("intakeretryfail");
    let before = Poisoned::count(&poisoned.tried, "said: the stubborn one");
    let (_, stderr, code) = poisoned.intake(&["--retry-failed"]);
    assert_eq!(code, Some(4), "{stderr}");
    assert!(
        Poisoned::count(&poisoned.tried, "said: the stubborn one") > before,
        "retried: {stderr}"
    );
    assert!(stderr.contains("2 candidate(s) set aside"), "{stderr}");
    assert!(
        !stderr.contains("wait for the next run"),
        "set aside again, not waiting: {stderr}"
    );
    // Still kept, still said, and retried again later with success.
    let (_, stderr, _) = poisoned.intake(&[]);
    assert!(stderr.contains("2 candidate(s) set aside"), "{stderr}");
    fs::remove_file(&poisoned.refuse).unwrap();
    let (_, stderr, code) = poisoned.intake(&["--retry-failed"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(Poisoned::count(&poisoned.sent, "said: correction 2"), 1);
    assert_eq!(Poisoned::count(&poisoned.sent, "said: the stubborn one"), 1);
    let (_, stderr, _) = poisoned.intake(&[]);
    assert!(!stderr.contains("set aside"), "{stderr}");
    poisoned.fixture.cleanup();
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

// ---- review fixes: each test failed before its fix -----------------------------

/// A drafter that, while intake is drafting, checks the session log can be
/// opened by another ohjaa (as `oriented`, `handover` and leaving must).
fn drafter_that_opens_the_log(fixture: &Fixture) -> PathBuf {
    let drafter = fixture.stub_dir.join("drafter-opens-log");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {prompt}\n{ohjaa} session > {during} 2>&1\necho \"exit $?\" >> {during}\necho '[]'\n",
            prompt = fixture.dir.join("prompt.txt").display(),
            ohjaa = ohjaa_bin(),
            during = fixture.dir.join("during.txt").display(),
        ),
    );
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
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    );
    let intake = |source: &str| {
        let _ = fs::remove_file(fixture.dir.join("prompt.txt"));
        let (_, stderr, _) = output_of(
            fixture
                .ohjaa()
                .args(["intake", "--source", source, "--drafter"])
                .arg(&drafter)
                .current_dir(&repo),
        );
        let prompt = fs::read_to_string(fixture.dir.join("prompt.txt")).unwrap_or_default();
        (prompt, stderr)
    };
    // A run that never reads transcripts leaves the session alone and queued...
    let (prompt, _) = intake("reviews");
    assert!(
        !prompt.contains("the handover text"),
        "a session is drafted with its transcript window: {prompt}"
    );
    // ...and one whose transcripts could not all be read drafts from what it
    // found, but leaves the session queued.
    let unreadable = fixture.dir.join("home/vessa-warning");
    fs::write(&unreadable, UNREADABLE).unwrap();
    let (prompt, _) = intake("transcripts");
    assert!(prompt.contains("the handover text"), "{prompt}");
    fs::remove_file(&unreadable).unwrap();
    // Read cleanly: still queued after the warning, its handover is not sent
    // again, and only now it is marked drafted.
    let (prompt, stderr) = intake("transcripts");
    assert!(stderr.contains("1 ended session(s) queued"), "{stderr}");
    assert!(
        !prompt.contains("the handover text"),
        "handled once, never sent again: {prompt}"
    );
    let (prompt, stderr) = intake("transcripts");
    assert!(!stderr.contains("queued"), "drafted: {stderr}");
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
    fixture.node(&mine, "big", "fact", &"long knowledge ".repeat(450));
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(0), "{stderr}");
    // A fact past the budget is named by count, not sent. (The budget holds the
    // line saying why Codex is briefed with mine and machine, too.)
    fixture.node(&mine, "huge", "fact", &"long knowledge ".repeat(8_000));
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("\n1 more: `ohjaa list`\n"), "{stderr}");
    assert_eq!(stderr.matches("long knowledge").count(), 450);
    // Every decision is sent in full, however long. Linux caps one argument
    // at 128 KiB, and the briefing is one, twice.
    fixture.node(&mine, "huge-rule", "decision", &"long rule ".repeat(12_000));
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

    // The receipt says passed; sykli refuses it (it ran under another
    // contract). The record says what sykli said.
    let (record, head) = pull_request_session(&fixture, &repo, "forged", "passed forged", true);
    let finished = finished_line(&record);
    assert!(
        finished.contains("https://github.com/o/r/pull/7 (open)")
            && finished.contains(&format!("head {}", &head[..7]))
            && finished.contains("unproven: ")
            && finished.contains("(refused: contract expected c but got forged)"),
        "{finished}"
    );
    assert!(!finished.contains("verified: passed"), "{finished}");
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
fn a_refusal_says_when_not_every_receipt_of_the_tree_was_asked() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("receipts-cap");
    let repo = clone_with_origin(&fixture, "work");
    // Six refused receipts of one tree: sykli is asked about five.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "six-runs",
        "echo '{}' > sykli.json && git add sykli.json && git commit -q -m checked
        for n in 1 2 3 4 5 6; do
          receipt passed forged && mv .sykli/receipts/rcpt_$(git rev-parse HEAD^{tree}).json .sykli/receipts/run_$n.json
        done
        git push -q origin HEAD:main",
        "1",
    );
    assert!(
        landed_lines(&record)
            .concat()
            .contains("(sykli was asked about the best 5 of 6 receipts of its tree)"),
        "{record}"
    );
    fixture.cleanup();
}

#[test]
fn a_commit_passes_when_any_receipt_of_its_tree_verifies() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("receipts-per-tree");
    let repo = clone_with_origin(&fixture, "work");
    // A passing run of the commit's contract, then a newer passing run of
    // the same tree under another contract (a demo, say), which sykli
    // refuses for that commit.
    let (record, wt) = worktree_push_session(
        &fixture,
        &repo,
        "two-runs",
        "echo '{}' > sykli.json && git add sykli.json && git commit -q -m checked
        receipt passed
        mv .sykli/receipts/rcpt_*.json .sykli/receipts/rcpt_the_contract.json
        sleep 1
        receipt passed forged
        git push -q origin HEAD:main",
        "1",
    );
    assert!(
        landed_lines(&record).concat().contains(&format!(
            "{} reached origin/main: verified: passed (sykli verify --commit)",
            short_of(&wt, "HEAD")
        )),
        "a newer receipt of the tree never shadows a pass: {record}"
    );
    assert!(!record.contains("FLAGGED"), "{record}");
    // Refused only when every receipt of its tree is.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "all-refused",
        "git pull -q origin main && echo x > x.txt && git add x.txt && git commit -q -m x
        receipt passed forged
        mv .sykli/receipts/rcpt_*.json .sykli/receipts/rcpt_one.json
        sleep 1
        receipt failed
        git push -q origin HEAD:main",
        "1",
    );
    assert!(
        landed_lines(&record)
            .concat()
            .contains("FAILED sykli verify: refused:"),
        "{record}"
    );
    assert!(record.contains("─ FLAGGED"), "{record}");
    fixture.cleanup();
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
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(!stdout.contains("WARNING"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn a_push_to_main_with_no_sykli_run_where_there_is_a_contract_warns() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("unchecked-push");
    let repo = clone_with_origin(&fixture, "work");
    let (record, wt) = worktree_push_session(
        &fixture,
        &repo,
        "unchecked",
        "echo '{}' > sykli.json && git add sykli.json && git commit -q -m 'no run'
        git push -q origin HEAD:main",
        "1",
    );
    let warning = format!(
        "origin/main at {} in work has no passing evidence — check before building on it",
        short_of(&wt, "HEAD")
    );
    assert!(
        record.contains("─ FLAGGED") && record.contains(&warning),
        "{record}"
    );
    assert!(
        record.contains("unchecked: no sykli receipt for its tree"),
        "{record}"
    );
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(
        stdout.starts_with("WARNING from the last session here") && stdout.contains(&warning),
        "{stdout}"
    );
    // A commit with no contract has nothing to run: recorded, never flagged,
    // and main has moved on from the unchecked commit.
    let (record, _) = worktree_push_session(
        &fixture,
        &repo,
        "no-contract",
        "git pull -q origin main && git rm -q sykli.json && git commit -q -m 'drop the contract'
        git push -q origin HEAD:main",
        "1",
    );
    assert!(
        landed_lines(&record)
            .concat()
            .contains("unverified: no sykli contract"),
        "{record}"
    );
    assert!(!record.contains("FLAGGED"), "{record}");
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(!stdout.contains("WARNING"), "{stdout}");
    fixture.cleanup();
}

#[test]
fn a_broken_main_pushed_from_a_shared_clone_warns_whoever_pushed_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("shared-failing");
    let repo = clone_with_origin(&fixture, "work");
    let mark = |name: &str| fixture.dir.join(name);
    // Y sits idle in the clone while X, in a worktree of its own, pushes a
    // failing commit to main past every hook: neither is shown to have done it.
    let idle = format!(
        "touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done",
        mark("y-started").display(),
        mark("x-done").display()
    );
    let broken = format!(
        "{RECEIPT_FN}
        set -e
        while [ ! -e '{y}' ]; do sleep 0.1; done
        sleep 1
        git worktree add -q -b x '{wt}' origin/main
        cd '{wt}'
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q --no-verify -m broken
        receipt failed
        git push -q --no-verify origin HEAD:main
        touch '{done}'",
        y = mark("y-started").display(),
        wt = fixture.dir.join("_worktrees").join("x").display(),
        done = mark("x-done").display()
    );
    let launch = |intent: &str, script: &str| {
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
                script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let y = launch("y", &idle);
    let x = launch("x", &broken);
    let records: Vec<String> = [x, y]
        .into_iter()
        .map(|child| {
            String::from_utf8_lossy(&child.wait_with_output().unwrap().stderr).into_owned()
        })
        .collect();
    let warning = format!(
        "origin/main at {} in work has no passing evidence (observed, not attributed) — check \
         before building on it",
        short_of(&repo, "origin/main")
    );
    for record in &records {
        assert!(
            record.contains("(not attributed)"),
            "neither session is shown to have pushed it: {record}"
        );
        assert!(
            record.contains("─ FLAGGED") && record.contains(&warning),
            "a broken main is said whoever pushed it: {record}"
        );
        assert!(
            record.contains("FAILED sykli verify: refused: outcome"),
            "{record}"
        );
    }
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(
        stdout.starts_with("WARNING from the last session here") && stdout.contains(&warning),
        "{stdout}"
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
    // A stray file beside the commit: sykli judges the commit alone.
    let (record, _) = worktree_push_session(
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
        landed_lines(&record).concat().contains("verified: passed"),
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
    // And nothing of it opens the next briefing.
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(!stdout.contains("WARNING"), "{stdout}");
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
    // After the sentinel line, which only says where ohjaa's words begin.
    let briefing = briefing
        .strip_prefix("<!-- ohjaa-briefing v1 -->\n")
        .expect(briefing);
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
        // Written whole, then moved into place: never read half written.
        &format!(
            "echo $$ > '{0}.new'; mv '{0}.new' '{0}'; exec sleep 30",
            agent_pid.display()
        ),
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
            && stdout.contains("phases        not observed (its ohjaa was gone before leaving)"),
        "{stdout}"
    );
    // Queued for intake, like any ended session.
    let drafter = fixture.stub_dir.join("drafter");
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    );
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
    write_executable(
        &drafter,
        format!(
            "#!/bin/sh\ncat > {}\necho '[]'\n",
            fixture.dir.join("prompt.txt").display()
        ),
    );
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

#[test]
fn a_push_from_a_worktree_removed_since_is_never_shouted_for_its_lost_receipt() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("gone-worktree");
    let repo = clone_with_origin(&fixture, "work");
    // The house flow: a worktree off origin/main, a commit and its run, a push
    // to main, then the worktree and its branch go, and its `.sykli/` with it.
    // With `keep`, sykli's pre-push hook keeps the receipt that verified the
    // push under the git common dir.
    let session = |name: &str, keep: bool| {
        let wt = fixture.dir.join("_worktrees").join(name);
        let script = format!(
            "{RECEIPT_FN}
            set -e
            git fetch -q origin
            git worktree add -q -b {name} '{wt}' origin/main
            cd '{wt}'
            echo {name} > {name}.txt && echo '{{}}' > sykli.json
            git add {name}.txt sykli.json && git commit -q -m {name}
            receipt passed
            if [ {keep} = true ]; then
              common=$(cd \"$(git rev-parse --git-common-dir)\" && pwd)
              mkdir -p \"$common/sykli/receipts\" && cp .sykli/receipts/*.json \"$common/sykli/receipts/\"
            fi
            git push -q origin HEAD:main
            cd '{repo}' && git worktree remove --force '{wt}' && git branch -q -D {name}",
            wt = wt.display(),
            repo = repo.display(),
        );
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
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
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        stderr
    };
    let brief = || output_of(fixture.ohjaa().arg("brief").current_dir(&repo)).0;
    // No receipt anywhere any more: it cannot be observed, so it is not said.
    let record = session("gone", false);
    let tip = short_of(&repo, "origin/main");
    assert!(
        landed_lines(&record).concat().contains(&format!(
            "{tip} reached origin/main: unchecked: no sykli receipt, and the checkout it was \
             made in is gone"
        )),
        "{record}"
    );
    assert!(!record.contains("FLAGGED"), "{record}");
    assert!(!brief().contains("WARNING"));
    // The receipt sykli kept under the common dir verifies it.
    let record = session("kept", true);
    assert!(
        landed_lines(&record)
            .concat()
            .contains("verified: passed (sykli verify --commit)"),
        "{record}"
    );
    assert!(!record.contains("FLAGGED"), "{record}");
    assert!(!brief().contains("WARNING"));
    fixture.cleanup();
}

#[test]
fn a_stash_is_never_taken_for_lost_work() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("stash");
    let repo = clone_with_origin(&fixture, "work");
    fs::write(repo.join("tracked.txt"), "one").unwrap();
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "tracked"]);
    let calls = fixture.dir.join("gh-calls.txt");
    // A stash and its pop write commits no reference names afterwards.
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
                "set -e; sleep 1; echo two > tracked.txt; git stash -q; git stash pop -q",
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_GH_CALLS", &calls)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("phases        no commits") && !stderr.contains("on no branch now"),
        "{stderr}"
    );
    assert_eq!(
        finished_line(&stderr),
        "finished      unknown (not looked up: this session made no commit on a branch)",
        "{stderr}"
    );
    fixture.cleanup();
}

#[test]
fn a_squash_merged_pull_request_is_found_by_its_commit_once_every_ref_is_pruned() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("pruned");
    let repo = clone_with_origin(&fixture, "work");
    let origin = fixture.dir.join("work-origin.git");
    let wt = fixture.dir.join("_worktrees").join("feat5");
    let mark = fixture.dir.join("sha");
    // Squash-merged on the forge, which deletes the branch; then everything
    // here goes too, the remote-tracking branch and its reflog by a prune.
    let script = format!(
        "{RECEIPT_FN}
        set -e
        git worktree add -q -b feat5 '{wt}' origin/main
        cd '{wt}'
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m 'the work'
        receipt passed
        git push -q origin HEAD:feat5
        git rev-parse HEAD > '{mark}'
        cd '{repo}' && git worktree remove --force '{wt}' && git branch -q -D feat5
        git -C '{origin}' branch -q -D feat5
        git fetch -q --prune
        ! git rev-parse -q --verify refs/remotes/origin/feat5",
        wt = wt.display(),
        repo = repo.display(),
        origin = origin.display(),
        mark = mark.display()
    );
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
            .env("OHJAA_STUB_GH_PR_SEARCH_FILE", &mark)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let sha = fs::read_to_string(&mark).unwrap().trim().to_owned();
    let phases = stderr
        .lines()
        .find(|l| l.starts_with("phases"))
        .expect(&stderr);
    assert!(
        phases.contains(&format!("{} ", &sha[..7])) && phases.contains("(on no branch now)"),
        "found in the object store, though no reference names it: {phases}"
    );
    assert!(
        finished_line(&stderr).starts_with("finished      https://github.com/o/r/pull/8 (merged)"),
        "found by its commit: {stderr}"
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
    // The broken main is the repository's state, whoever pushed it: said,
    // never as this session's.
    let warning = format!(
        "origin/main at {pushed} in plain has no passing evidence (observed, not attributed) — \
         check before building on it"
    );
    assert!(record.contains(&warning), "{record}");
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&plain));
    assert!(
        stdout.starts_with("WARNING from a session that observed this repository")
            && stdout.contains(&warning)
            && !stdout.contains("Last session here"),
        "the root session is not plain's last session, but plain's main is broken: {stdout}"
    );
    fixture.cleanup();
}

/// Two sessions launched at once in one checkout of a new repository, x
/// and y, each making a worktree and a commit there while the other is in
/// the checkout, one after the other (`after`: the mark it waits for).
/// Never at once: two `git worktree add`s in one repository race in git
/// itself, each reading the other's admin folder while it is written
/// ("fatal: failed to read .git/worktrees/y/commondir"). Neither agent
/// waits for the other for ever: one that stops early still says it is
/// done, and each gives up on a mark that is not there `deadline` seconds
/// after it started (the other's `ohjaa run` never started it). y waits
/// `y_late` seconds more before its git. The two runs' outputs, x's first.
fn shared_checkout(fixture: &Fixture, deadline: u64, y_late: u64) -> [std::process::Output; 2] {
    let repo = fixture.make_git_repo("work");
    let mark = |name: &str| fixture.dir.join(name);
    let agent = |me: &str, other: &str, after: Option<&str>, late: u64| {
        let turn = after.map_or_else(String::new, |after| {
            format!("wait_for '{}'", mark(after).display())
        });
        format!(
            "set -e
            trap \"touch '{done}'\" EXIT
            deadline=$(( $(date +%s) + {deadline} ))
            wait_for() {{
              while [ ! -e \"$1\" ]; do
                [ \"$(date +%s)\" -lt \"$deadline\" ] || {{ echo \"{me}: no $1 within {deadline} s\" >&2; exit 1; }}
                sleep 0.1
              done
            }}
            touch '{started}'
            wait_for '{their_start}'
            sleep 1
            {turn}
            sleep {late}
            git worktree add -q -b {me} '{wt}'
            cd '{wt}' && echo {me} > {me}.txt && git add {me}.txt && git commit -q -m {me}
            git rev-parse --short=7 HEAD > '{sha}'
            touch '{done}'
            wait_for '{their_done}'
            touch '{end}'",
            started = mark(&format!("{me}-started")).display(),
            their_start = mark(&format!("{other}-started")).display(),
            wt = fixture.dir.join("_worktrees").join(me).display(),
            sha = mark(&format!("{me}-sha")).display(),
            done = mark(&format!("{me}-done")).display(),
            their_done = mark(&format!("{other}-done")).display(),
            end = mark(&format!("{me}-end")).display(),
        )
    };
    let launch = |me: &str, other: &str, after: Option<&str>, late: u64| {
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
                &agent(me, other, after, late),
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let x = launch("x", "y", None, 0);
    let y = launch("y", "x", Some("x-done"), y_late);
    [x, y].map(|child| child.wait_with_output().unwrap())
}

/// What the two sessions of [`shared_checkout`] must have recorded: neither
/// claims the other's commit, and each says why. Only for two agents that
/// did all their work: both runs exited 0 (`ohjaa run` returns its agent's
/// exit) and both agents got to their last step, past every wait. One that
/// gave up waiting left before the other's commit, which then proves
/// nothing.
fn neither_claims_the_others_commit(fixture: &Fixture, outputs: &[std::process::Output; 2]) {
    let records: Vec<String> = outputs
        .iter()
        .map(|output| String::from_utf8_lossy(&output.stderr).into_owned())
        .collect();
    for (output, me) in outputs.iter().zip(["x", "y"]) {
        assert!(
            output.status.success() && fixture.dir.join(format!("{me}-end")).exists(),
            "{me} did not do all its work ({}):\n{}",
            output.status,
            records.join("\n")
        );
    }
    let sha = |me: &str| {
        fs::read_to_string(fixture.dir.join(format!("{me}-sha")))
            .unwrap_or_else(|error| {
                panic!("{me} made no commit ({error}):\n{}", records.join("\n"))
            })
            .trim()
            .to_owned()
    };
    for (record, theirs) in records.iter().zip(["y", "x"]) {
        let phases = record
            .lines()
            .find(|l| l.starts_with("phases"))
            .unwrap_or_else(|| panic!("a record without phases:\n{}", records.join("\n")));
        assert!(
            !phases.contains(&sha(theirs)),
            "a session never claims the other's commit: {phases}\n{record}"
        );
        assert!(
            record.contains("shared        with oh-"),
            "and says why: {record}"
        );
    }
}

#[test]
fn two_sessions_in_one_checkout_never_claim_each_others_commits() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("shared-checkout");
    let outputs = shared_checkout(&fixture, 120, 0);
    neither_claims_the_others_commit(&fixture, &outputs);
    fixture.cleanup();
}

#[test]
fn a_wait_that_ran_out_in_the_two_sessions_test_never_passes_it() {
    if !git_available() {
        return;
    }
    // y is later than x's deadline (the re-review of #59, 130 s against
    // 120): x gives up waiting for y to be done, after its own commit, and
    // leaves; y commits once x is gone. The commits were not made while
    // both sessions were there, so this must not pass.
    let fixture = Fixture::new("shared-late");
    let outputs = shared_checkout(&fixture, 3, 6);
    let judged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        neither_claims_the_others_commit(&fixture, &outputs)
    }));
    assert!(
        judged.is_err(),
        "x gave up and left before y's commit, and the test passed:\n{}\n{}",
        String::from_utf8_lossy(&outputs[0].stderr),
        String::from_utf8_lossy(&outputs[1].stderr)
    );
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

/// One launch through ohjaa each of a stub `claude` and a stub `pi` that
/// write when they started, and a session of `sh` that pushes a failing
/// commit to main: what the report is made of.
fn report_fixture(fixture: &Fixture) -> (PathBuf, String) {
    let repo = clone_with_origin(fixture, "work");
    let launch = |agent: &str, intent: &str| {
        let started = fixture.dir.join(format!("{agent}-started"));
        let stub = fixture.stub_dir.join(agent);
        write_executable(
            &stub,
            format!(
                "#!/bin/sh\ndate -u +%Y-%m-%dT%H:%M:%SZ > '{}'\nexit 0\n",
                started.display()
            ),
        );
        let (_, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(["run", agent, "--intent", intent, "--no-briefing"])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .current_dir(&repo),
        );
        assert_eq!(code, Some(0), "{stderr}");
        fs::read_to_string(&started).unwrap().trim().to_owned()
    };
    let launched = launch("claude", "through ohjaa");
    let pi_launched = launch("pi", "pi through ohjaa");
    let script = format!(
        "{RECEIPT_FN}
        set -e
        echo '{{}}' > sykli.json && git add sykli.json && git commit -q -m 'broken build'
        receipt failed
        git push -q origin HEAD:main"
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "push broken",
                "--no-briefing",
                "--",
                "-c",
                &script,
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    // vessa sees the launches above, and a claude and a pi session started
    // by hand: its header listing, each session's eight fields.
    let repo_path = std::fs::canonicalize(&repo).unwrap();
    fs::write(
        fixture.dir.join("vessa.json"),
        format!(
            r#"{{"schema":"vessa-session-headers.v1","warnings":[],"sessions":[
              {{"agent":"claude","id":"through","parent":null,"started_at":"{launched}","cwd":"{r}","source":"/nonexistent/1.jsonl","entrypoint":"cli","originator":null}},
              {{"agent":"claude","id":"by-hand","parent":null,"started_at":"{launched}","cwd":"{r}/elsewhere","source":"/nonexistent/2.jsonl","entrypoint":"cli","originator":null}},
              {{"agent":"claude","id":"by-hand","parent":"x","started_at":"{launched}","cwd":"{r}","source":"/nonexistent/3.jsonl","entrypoint":"cli","originator":null}},
              {{"agent":"claude","id":"drafter","parent":null,"started_at":"{launched}","cwd":"{r}/elsewhere","source":"/nonexistent/4.jsonl","entrypoint":"sdk-cli","originator":null}},
              {{"agent":"pi","id":"pi-through","parent":null,"started_at":"{pi_launched}","cwd":"{r}","source":"/nonexistent/5.jsonl","entrypoint":null,"originator":null}},
              {{"agent":"pi","id":"pi-by-hand","parent":null,"started_at":"{pi_launched}","cwd":"{r}/elsewhere","source":"/nonexistent/6.jsonl","entrypoint":null,"originator":null}}
            ]}}"#,
            r = repo_path.display()
        )
        .replace(&format!("{}/elsewhere", repo_path.display()), "/nonexistent/elsewhere"),
    )
    .unwrap();
    let pushed = short_head(&repo);
    (repo, pushed)
}

#[test]
fn the_report_lists_untold_sessions_unproven_commits_and_bypasses() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report");
    let (repo, pushed) = report_fixture(&fixture);
    let report = |json: bool| {
        let mut command = fixture.ohjaa();
        command
            .arg("--root")
            .arg(&fixture.dir)
            .arg("report")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
            .current_dir(&repo);
        if json {
            command.arg("--json");
        }
        output_of(&mut command)
    };
    let (stdout, stderr, code) = report(true);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let marks: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(marks["schema"], "ohjaa-marks.v1");
    // The window's report, as it was, under `report`.
    let json = &marks["report"];
    assert_eq!(json["schema"], "ohjaa-report.v1");
    let mut intents: Vec<&str> = json["sessions_without_testimony"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["intent"].as_str().unwrap())
        .collect();
    // Launched within a second of each other, they are logged in no
    // particular order.
    intents.sort_unstable();
    assert_eq!(
        intents,
        ["pi through ohjaa", "push broken", "through ohjaa"],
        "{stdout}"
    );
    let commits = json["unproven_commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1, "{stdout}");
    assert_eq!(commits[0]["check"], "failed");
    assert!(commits[0]["commit"].as_str().unwrap().starts_with(&pushed));
    assert_eq!(json["bypasses"]["judged"], true);
    assert_eq!(
        json["bypasses"]["headless"], 1,
        "claude -p bypasses nothing"
    );
    assert_eq!(
        json["default_branch_commits"]["repositories"], 1,
        "{stdout}"
    );
    let bypassed: Vec<&str> = json["bypasses"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        bypassed,
        ["by-hand", "pi-by-hand"],
        "the launches through ohjaa, claude's and pi's, and a subagent are not bypasses; \
         a pi session started by hand is: {stdout}"
    );
    // Launched with --no-briefing, they recorded no id of their own: each is
    // matched by time and folder, and said so.
    let mut matched: Vec<(&str, &str)> = json["bypasses"]["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["id"].as_str().unwrap(), m["by"].as_str().unwrap()))
        .collect();
    matched.sort_unstable();
    assert_eq!(
        matched,
        [
            ("pi-through", "time_and_folder"),
            ("through", "time_and_folder")
        ],
        "{stdout}"
    );
    let (text, _, code) = report(false);
    assert_eq!(code, Some(0));
    assert!(
        text.contains("Commits on a default branch without passing evidence (1), of ")
            && text.contains("  The rest: ")
            && text.contains(&format!("work origin/main {pushed} \"broken build\""))
            && text.contains("Agent sessions a person started without ohjaa (2):")
            && text.contains("Not counted: 1 headless run(s)")
            && text.contains("- claude by-hand in /nonexistent/elsewhere")
            && text.contains("- pi pi-by-hand in /nonexistent/elsewhere")
            && text.contains("  Launched through ohjaa (2): 2 by time and folder alone\n")
            && text.contains("  - claude through taken as oh-"),
        "{text}"
    );
    assert!(
        !fixture.dir.join("state/ohjaa/report-at").exists(),
        "only a complete report a person read at a terminal is the week's"
    );

    // Intake drafts from what the report found, never from what could not be
    // checked.
    let drafter = fixture.stub_dir.join("drafter");
    let prompt = fixture.dir.join("prompt.txt");
    write_executable(
        &drafter,
        format!("#!/bin/sh\ncat > '{}'\necho '[]'\n", prompt.display()),
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .arg("--root")
            .arg(&fixture.dir)
            .args(["intake", "--source", "report", "--drafter"])
            .arg(&drafter)
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let prompt = fs::read_to_string(&prompt).unwrap();
    assert!(
        prompt.contains("never read back its briefing")
            && prompt
                .contains("\"broken build\" is on origin/main in work without passing evidence")
            && prompt.contains("a claude session (by-hand)")
            && prompt.contains("a pi session (pi-by-hand)"),
        "{prompt}"
    );
    assert!(!prompt.contains("unverified"), "{prompt}");
    fixture.cleanup();
}

#[test]
fn a_report_without_vessa_says_what_it_could_not_look_for() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report-no-vessa");
    let repo = fixture.make_git_repo("work");
    fs::remove_file(fixture.stub_dir.join("vessa")).unwrap();
    let path = std::env::join_paths([
        fixture.stub_dir.clone(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .arg("--root")
            .arg(&fixture.dir)
            .arg("report")
            .env("PATH", path)
            .current_dir(&repo),
    );
    assert_eq!(
        code,
        Some(4),
        "incomplete, never quietly empty: {stdout}{stderr}"
    );
    assert!(
        stdout.contains(
            "Agent sessions a person started without ohjaa: not judged: vessa is not on PATH"
        ),
        "{stdout}"
    );
    fixture.cleanup();
}

#[test]
fn what_vessa_could_not_read_or_date_leaves_the_report_incomplete() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report-vessa-gaps");
    let repo = fixture.make_git_repo("work");
    let transcript = fixture.dir.join("undated.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    let report = |listed: &str| {
        fs::write(fixture.dir.join("vessa.json"), listed).unwrap();
        output_of(
            fixture
                .ohjaa()
                .arg("--root")
                .arg(&fixture.dir)
                .arg("report")
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
                .current_dir(&repo),
        )
    };
    // vessa read nothing, and said so: never a clean week.
    let (stdout, _, code) = report(
        r#"{"schema":"vessa-session-headers.v1","sessions":[],"warnings":[
            "/home/x/.claude/projects: symlink not followed",
            "No supported native sessions found; usage is unknown"]}"#,
    );
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("Agent sessions a person started without ohjaa (0):")
            && stdout.contains("- vessa: /home/x/.claude/projects: symlink not followed")
            && stdout.contains("- vessa: No supported native sessions found"),
        "{stdout}"
    );
    // A session vessa could not date is said apart, never dropped.
    let (stdout, _, code) = report(&format!(
        r#"{{"schema":"vessa-session-headers.v1","warnings":[],"sessions":[
            {{"agent":"claude","id":"no-date","parent":null,"started_at":null,"cwd":"",
              "source":"{}","entrypoint":"cli","originator":null}}]}}"#,
        transcript.display()
    ));
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("Not judged, vessa could not date or place them (1):")
            && stdout.contains("- claude no-date (transcript ")
            && stdout.contains("1 native session(s) vessa could not date or place: not judged"),
        "{stdout}"
    );
    // Only the header listing is read: the full one, given instead, is not.
    let (stdout, _, code) = report(
        r#"{"schema":"vessa-usage.v1","warnings":[],"sessions":[
            {"agent":"claude","id":"old","parent":null,"started_at":"2026-10-09T10:00:00Z","cwd":"/r"}]}"#,
    );
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains(
            "not judged: vessa listed sessions as `vessa-usage.v1`, not \
             `vessa-session-headers.v1`"
        ),
        "{stdout}"
    );
    fixture.cleanup();
}

/// A vessa from before vessa#5 refuses `--headers` as clap does (exit 2,
/// nothing on stdout). The report says to update it, and never asks for the
/// slow full listing instead.
#[test]
fn a_vessa_too_old_for_headers_is_said_and_never_read_the_slow_way() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report-old-vessa");
    let repo = fixture.make_git_repo("work");
    let calls = fixture.dir.join("vessa-calls");
    write_executable(
        &fixture.stub_dir.join("vessa"),
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n\
             case \" $* \" in *' --headers '*)\n\
               printf '%s\\n\\n%s\\n\\n%s\\n' \"error: unexpected argument '--headers' found\" \
                 'Usage: vessa sessions [OPTIONS]' \"For more information, try '--help'.\" >&2\n\
               exit 2;;\n\
             esac\n\
             echo '{{\"schema\":\"vessa-usage.v1\",\"sessions\":[],\"warnings\":[]}}'\n",
            calls.display()
        ),
    );
    let report = |json: bool| {
        let mut command = fixture.ohjaa();
        command
            .arg("--root")
            .arg(&fixture.dir)
            .arg("report")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo);
        if json {
            command.arg("--json");
        }
        output_of(&mut command)
    };
    // What vessa said is kept: its first line, not clap's closing hint.
    let why = "vessa is too old for `sessions --headers`; update it (vessa said: error: \
               unexpected argument '--headers' found)";
    let (stdout, stderr, code) = report(false);
    assert_eq!(code, Some(4), "never a clean week: {stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "Agent sessions a person started without ohjaa: not judged: {why}"
        )),
        "{stdout}"
    );
    let (stdout, _, code) = report(true);
    assert_eq!(code, Some(4), "{stdout}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        json["report"]["bypasses"],
        serde_json::json!({"judged": false, "why": why})
    );
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "sessions --json --headers\nsessions --json --headers\n",
        "asked once a report, for the headers alone: no fallback to the full listing"
    );
    fixture.cleanup();
}

/// Two pi sessions start in the repository while one `ohjaa run pi` runs:
/// its own, whose transcript was last written as it ended, and one a person
/// opened by hand and left at once. Whichever started first, the one by hand
/// is the bypass, never ohjaa's own.
#[test]
fn of_two_sessions_started_during_a_launch_its_own_is_not_the_bypass() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report-two-pis");
    let repo = fixture.make_git_repo("work");
    let repo_path = std::fs::canonicalize(&repo).unwrap();
    // The stub pi notes two moments a second apart while it runs, each as
    // epoch seconds and RFC 3339 from one `date`.
    let moments = fixture.dir.join("pi-moments");
    let pi = fixture.stub_dir.join("pi");
    write_executable(
        &pi,
        format!(
            "#!/bin/sh\ndate -u '+%s %Y-%m-%dT%H:%M:%SZ' > '{m}'\nsleep 1\n\
             date -u '+%s %Y-%m-%dT%H:%M:%SZ' >> '{m}'\nsleep 1\nexit 0\n",
            m = moments.display()
        ),
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "pi", "--intent", "pi through ohjaa", "--no-briefing"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let noted = fs::read_to_string(&moments).unwrap();
    let at: Vec<(u64, &str)> = noted
        .lines()
        .map(|line| {
            let (secs, rfc) = line.split_once(' ').unwrap();
            (secs.parse().unwrap(), rfc)
        })
        .collect();
    assert_eq!(at.len(), 2, "{noted}");
    let (out, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ended_ms = export["sessions"][0]["left"]["ended_ms"].as_i64().unwrap();
    let transcript = |name: &str, written_secs: u64| {
        let path = fixture.dir.join(format!("{name}.jsonl"));
        fs::write(&path, "{}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(written_secs))
            .unwrap();
        path
    };
    for (own_at, by_hand_at) in [(at[1], at[0]), (at[0], at[1])] {
        // Its own was written until the launch ended; the one by hand only
        // as it started.
        let own = transcript("own", (ended_ms / 1000) as u64);
        let by_hand = transcript("by-hand", by_hand_at.0);
        let line = |id: &str, started: &str, source: &Path| {
            format!(
                r#"{{"agent":"pi","id":"{id}","parent":null,"started_at":"{started}","cwd":"{}","source":"{}","entrypoint":null,"originator":null}}"#,
                repo_path.display(),
                source.display()
            )
        };
        fs::write(
            fixture.dir.join("vessa.json"),
            format!(
                r#"{{"schema":"vessa-session-headers.v1","warnings":[],"sessions":[{},{}]}}"#,
                line("by-hand", by_hand_at.1, &by_hand),
                line("own", own_at.1, &own)
            ),
        )
        .unwrap();
        let (stdout, stderr, code) = output_of(
            fixture
                .ohjaa()
                .arg("--root")
                .arg(&fixture.dir)
                .args(["report", "--json"])
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
                .current_dir(&repo),
        );
        assert!(matches!(code, Some(0 | 4)), "{stdout}{stderr}");
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let bypassed: Vec<&str> = json["report"]["bypasses"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("{stdout}"))
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            bypassed,
            ["by-hand"],
            "own started {}, by hand {}: {stdout}",
            own_at.1,
            by_hand_at.1
        );
    }
    fixture.cleanup();
}

#[test]
fn a_window_with_more_commits_than_a_report_judges_says_so() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("report-cap");
    let repo = fixture.make_git_repo("work");
    let status = Command::new("sh")
        .args([
            "-c",
            "i=0; while [ $i -lt 101 ]; do git commit -q --allow-empty -m c$i || exit 1; i=$((i+1)); done",
        ])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(status.success());
    let (stdout, _, code) = output_of(
        fixture
            .ohjaa()
            .arg("--root")
            .arg(&fixture.dir)
            .arg("report")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("of 100 first-parent commit(s) in 1 repository")
            && stdout.contains(
                "work: only the newest 100 default-branch commits in the window were judged"
            )
            && stdout.contains("The rest: 0 verified · 100 without a sykli contract"),
        "{stdout}"
    );
    fixture.cleanup();
}

/// `command` run under a pseudo-terminal, as a person's shell would run it,
/// with its output; `None` where `script` is not there to make one.
fn under_a_terminal(command: &Command) -> Option<String> {
    let mut line = vec![command.get_program().to_string_lossy().into_owned()];
    line.extend(command.get_args().map(|a| a.to_string_lossy().into_owned()));
    let quoted = line
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    let mut script = Command::new("script");
    if cfg!(target_os = "macos") {
        script.args(["-q", "/dev/null", "sh", "-c", &quoted]);
    } else {
        script.args(["-qec", &quoted, "/dev/null"]);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => script.env(key, value),
            None => script.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        script.current_dir(dir);
    }
    let output = script.stdin(std::process::Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).replace('\r', ""))
}

#[test]
fn the_weekly_report_is_said_after_the_record_at_a_terminal_and_never_briefed() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("weekly");
    let repo = fixture.make_git_repo("work");
    let gappy = fixture.dir.join("gappy.json");
    fs::write(
        &gappy,
        r#"{"schema":"vessa-session-headers.v1","sessions":[],"warnings":["a source could not be read"]}"#,
    )
    .unwrap();
    let run = |intent: &str, complete: bool| {
        let mut command = fixture.ohjaa();
        command
            .args(["run", "sh", "--intent", intent, "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo);
        command.env("OHJAA_STUB_SYKLI_COMMIT", "1");
        if !complete {
            command.env("OHJAA_STUB_VESSA_JSON", &gappy);
        }
        under_a_terminal(&command)
    };
    let marker = fixture.dir.join("state/ohjaa/report-at");
    // A report with a gap (vessa could not read a source) is said, and said
    // again.
    let Some(first) = run("one", false) else {
        return; // no `script` here to make a terminal
    };
    let record = first.find("ohjaa: recorded; queued").expect(&first);
    let weekly = first.find("ohjaa: weekly report, ").expect(&first);
    assert!(weekly > record, "after the record: {first}");
    assert!(
        first.contains("incomplete: vessa: a source could not be read")
            && first.contains("said again tomorrow"),
        "it says what the gap is: {first}"
    );
    assert!(
        !marker.exists(),
        "an incomplete report does not mark the week"
    );
    assert!(
        !fixture.argv().join("\n").contains("weekly report"),
        "never into the agent's briefing"
    );
    // A gap that does not go away is said again at most once a day.
    let again = run("one again", false).unwrap();
    assert!(
        !again.contains("weekly report"),
        "not after every session: {again}"
    );
    let shown = fixture.dir.join("state/ohjaa/report-shown-at");
    let two_days_ago = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        - 2 * 86_400_000)
        .to_string();
    fs::write(&shown, two_days_ago).unwrap();
    let second = run("two", true).unwrap();
    assert!(
        second.contains("ohjaa: weekly report, ")
            && second.contains("the next summary is in 7 days"),
        "{second}"
    );
    assert!(marker.is_file(), "a complete one marks the week");
    let third = run("three", true).unwrap();
    assert!(!third.contains("weekly report"), "once a week: {third}");
    // Not at a terminal, nothing is said: no person is reading.
    fs::remove_file(&marker).unwrap();
    fs::remove_file(&shown).unwrap();
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "four", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        !stderr.contains("weekly report") && !marker.exists(),
        "{stderr}"
    );
    fixture.cleanup();
}

/// What a command could change in the log: its digest, its size and when it
/// was last written.
fn log_stamp(log: &Path) -> (String, u64, std::time::SystemTime) {
    use sha2::{Digest, Sha256};
    let meta = fs::metadata(log).unwrap();
    (
        format!("{:x}", Sha256::digest(fs::read(log).unwrap())),
        meta.len(),
        meta.modified().unwrap(),
    )
}

#[test]
fn commands_that_only_read_the_session_log_never_write_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("read-only-log");
    let (repo, _) = report_fixture(&fixture);
    fixture.node(
        &fixture.dir.join("ours/knowledge"),
        "lean",
        "taste",
        "Lean first.",
    );
    let log = fixture.dir.join("state/ohjaa/sessions.redb");
    // Written an hour ago: any write now moves it, however coarse the file
    // system's clock.
    fs::File::options()
        .write(true)
        .open(&log)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap();
    let before = log_stamp(&log);
    let (out, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(&repo));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(log_stamp(&log), before, "usage export wrote the log");
    let export: serde_json::Value = serde_json::from_str(&out).unwrap();
    // The fixture's three: claude's launch, pi's, and the push.
    assert_eq!(export["sessions"].as_array().unwrap().len(), 3, "{out}");
    let id = export["sessions"][0]["id"].as_str().unwrap().to_owned();
    let root = fixture.dir.to_string_lossy().into_owned();
    let reads: [&[&str]; 10] = [
        &["--root", &root, "report", "--json"],
        &["--root", &root, "report", "--since", "14d"],
        &["usage", "export", "--session", &id],
        &["session"],
        &["session", &id],
        &["brief"],
        &["graph", "lean"],
        &["run", "sh", "--intent", "look", "--dry-run"],
        &["intake", "--dry-run", "--source", "report"],
        &[
            "--root",
            &root,
            "intake",
            "--dry-run",
            "--source",
            "transcripts",
        ],
    ];
    for args in reads {
        let (stdout, stderr, code) = output_of(
            fixture
                .ohjaa()
                .args(args)
                .env("OHJAA_STUB_SYKLI_COMMIT", "1")
                .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
                .current_dir(&repo),
        );
        // 4: a perimeter with a repository whose role it cannot derive.
        assert!(
            matches!(code, Some(0 | 4)),
            "ohjaa {args:?}: {stdout}{stderr}"
        );
        assert!(
            !stderr.contains("without the session log") && !stderr.contains("could not be opened"),
            "ohjaa {args:?} read the log: {stdout}{stderr}"
        );
        assert_eq!(log_stamp(&log), before, "ohjaa {args:?} wrote the log");
    }
    fixture.cleanup();
}

/// `ohjaa report --json` is `ohjaa-marks.v1`, for other tools to read, and
/// it moves nothing: not the session log, not the report's bookmarks, even
/// at a terminal, where the report for people does mark the week.
#[test]
fn report_json_is_ohjaa_marks_v1_and_writes_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("marks-json");
    let (repo, _) = report_fixture(&fixture);
    let (_, stderr, code) = output_of(
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
                "Code is Rust.",
            ])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let state = fixture.dir.join("state/ohjaa");
    let log = state.join("sessions.redb");
    let at = state.join("report-at");
    fs::write(&at, "1000\n").unwrap();
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for file in [&log, &at] {
        fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
    }
    let stamps = || (log_stamp(&log), log_stamp(&at));
    let before = stamps();
    let mut command = fixture.ohjaa();
    command
        .arg("--root")
        .arg(&fixture.dir)
        .args(["report", "--json"])
        .env("OHJAA_STUB_SYKLI_COMMIT", "1")
        .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
        .current_dir(&repo);
    let (stdout, stderr, code) = output_of(&mut command);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert_eq!(
        stamps(),
        before,
        "report --json wrote the log or a bookmark"
    );
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["schema"], "ohjaa-marks.v1");
    let keys = |value: &serde_json::Value| {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(
        keys(&json),
        [
            "complete",
            "entries",
            "generated",
            "proposals",
            "report",
            "schema",
            "sessions",
            "unlisted"
        ]
    );
    assert_eq!(json["entries"], serde_json::json!([]), "no entries here");
    // No entries, and listed: none in the home, not none read.
    assert_eq!(json["unlisted"], serde_json::Value::Null);
    assert_eq!(json["proposals"]["listed"], true);
    let waiting = &json["proposals"]["waiting"][0];
    assert_eq!(keys(waiting), ["name", "repo", "scope", "since", "title"]);
    assert_eq!(waiting["name"], "rust-only");
    assert_eq!(waiting["scope"], "ours");
    assert_eq!(waiting["title"], "Code is Rust.");
    assert_eq!(json["report"]["schema"], "ohjaa-report.v1");

    // `--marks`: the same view without the window, its keys the same.
    let mut marks_only = fixture.ohjaa();
    marks_only
        .arg("--root")
        .arg(&fixture.dir)
        .args(["report", "--json", "--marks"])
        .env("OHJAA_STUB_SYKLI_COMMIT", "1")
        .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
        .current_dir(&repo);
    let (stdout, stderr, code) = output_of(&mut marks_only);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let marks: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(keys(&marks), keys(&json));
    assert_eq!(marks["report"], serde_json::Value::Null);
    assert_eq!(marks["proposals"], json["proposals"]);
    assert_eq!(stamps(), before, "report --json --marks wrote");
    if let Some(said) = under_a_terminal(&marks_only) {
        assert!(said.contains("ohjaa-marks.v1"), "{said}");
        assert_eq!(
            stamps(),
            before,
            "report --json --marks at a terminal wrote"
        );
    }

    // At a terminal too: `--json` is never the person's report.
    if let Some(said) = under_a_terminal(&command) {
        assert!(said.contains("ohjaa-marks.v1"), "{said}");
        assert_eq!(stamps(), before, "report --json at a terminal wrote");
        assert!(!state.join("report-shown-at").exists());
        // The control: the report for people, at the same terminal, is
        // the week's.
        let mut human = fixture.ohjaa();
        human
            .arg("--root")
            .arg(&fixture.dir)
            .arg("report")
            .env("OHJAA_STUB_SYKLI_COMMIT", "1")
            .env("OHJAA_STUB_VESSA_JSON", fixture.dir.join("vessa.json"))
            .current_dir(&repo);
        let said = under_a_terminal(&human).unwrap();
        assert!(said.contains("ohjaa report, "), "{said}");
        assert!(state.join("report-shown-at").exists(), "{said}");
        assert_ne!(log_stamp(&at), before.1, "the week was marked");
        assert_eq!(log_stamp(&log), before.0, "the log is only read");
    }
    fixture.cleanup();
}

#[test]
fn a_command_that_only_reads_says_a_held_log_is_busy_and_never_waits_on_it() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("read-held-log");
    let repo = fixture.make_git_repo("work");
    let (_, stderr, code) = run_session(&fixture, &repo, "exit 0");
    assert_eq!(code, Some(0), "{stderr}");
    let log = fixture.dir.join("state/ohjaa/sessions.redb");
    // A writer holds it, as a running `ohjaa run` does while it records.
    let holder = redb::Database::open(&log).unwrap();
    let ohjaa = |args: &[&str]| {
        let started = std::time::Instant::now();
        let out = output_of(
            fixture
                .ohjaa()
                .arg("--root")
                .arg(&fixture.dir)
                .args(args)
                .current_dir(&repo),
        );
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(10),
            "ohjaa {args:?} waited {took:?} on a held log"
        );
        out
    };
    let (stdout, stderr, code) = ohjaa(&["report", "--json"]);
    assert_eq!(
        code,
        Some(4),
        "a gap, never an empty week: {stdout}{stderr}"
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        report["report"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap
                .as_str()
                .unwrap()
                .contains("busy (another ohjaa holds it)")),
        "{stdout}"
    );
    let (stdout, stderr, code) = ohjaa(&["usage", "export"]);
    assert_eq!(code, Some(1), "never an empty export: {stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("busy (another ohjaa holds it)"), "{stderr}");
    let (stdout, stderr, code) = ohjaa(&["brief"]);
    assert!(code.is_some(), "{stdout}{stderr}");
    assert!(stdout.contains("No knowledge recorded yet"), "{stdout}");
    assert!(
        stderr.contains("busy (another ohjaa holds it); carrying on without the session log"),
        "{stderr}"
    );
    drop(holder);
    let (stdout, stderr, code) = ohjaa(&["usage", "export"]);
    assert_eq!(code, Some(0), "{stderr}");
    let export: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(export["sessions"].as_array().unwrap().len(), 1, "{stdout}");
    fixture.cleanup();
}

// ---- KATANA D4 ----------------------------------------------------------------

/// `ohjaa usage export` (ohjaa-sessions.v1) from `repo`.
fn export_of(fixture: &Fixture, repo: &Path) -> serde_json::Value {
    let (stdout, stderr, code) =
        output_of(fixture.ohjaa().args(["usage", "export"]).current_dir(repo));
    assert_eq!(code, Some(0), "{stderr}");
    serde_json::from_str(&stdout).unwrap()
}

/// D4: an Intent has an id of its own, apart from every session's id; a
/// later session continues it with `--intent-id`, and `--task` is recorded.
#[test]
fn an_intent_has_an_id_that_a_later_session_continues() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("intent-id");
    let repo = fixture.make_git_repo("work");
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args([
                "run",
                "sh",
                "--intent",
                "fix the parser",
                "--task",
                "start with the lexer",
                "--",
                "-c",
                "exit 0",
            ])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let briefing = fixture.argv().join("\n");
    let first = export_of(&fixture, &repo)["sessions"][0].clone();
    let id = first["id"].as_str().unwrap().to_owned();
    let intent = first["intent_id"]
        .as_str()
        .expect("an intent id")
        .to_owned();
    assert!(intent.starts_with("in-") && intent != id, "{intent} {id}");
    assert_eq!(first["task"], "start with the lexer");
    assert!(
        briefing.contains(&format!(
            "This session is {id}; its intent is {intent}, new. Continue it in a later \
             session with: ohjaa run <agent> --intent-id {intent}"
        )),
        "{briefing}"
    );

    // Continued: a new session, the same intent, and its text comes with it.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent-id", &intent, "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let export = export_of(&fixture, &repo);
    let second = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] != id.as_str())
        .unwrap()
        .clone();
    assert_eq!(second["intent_id"], intent.as_str());
    assert_eq!(second["intent"], "fix the parser");
    assert!(second.get("task").is_none(), "no task given: {second:#}");
    let argv = fixture.argv();
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--intent" && pair[1] == "fix the parser"),
        "toimija gets the intent's text: {argv:?}"
    );
    let briefing = argv.join("\n");
    assert!(
        briefing.contains(&format!(
            "its intent is {intent}, continued from session {id}"
        )),
        "{briefing}"
    );
    // `brief` names the last session's intent by its id.
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(
        stdout.contains(&format!("- intent: fix the parser (intent {intent})")),
        "{stdout}"
    );

    // Continued in another repository: an Intent may span them, and where it
    // was last worked on is said, on the terminal and in the briefing.
    let other = fixture.make_git_repo("other");
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent-id", &intent, "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&other),
    );
    assert_eq!(code, Some(0), "{stderr}");
    let work = fs::canonicalize(&repo).unwrap();
    assert!(
        stderr.contains(&format!(
            "ohjaa: intent {intent} was last worked on in {} (session ",
            work.display()
        )) && stderr.contains("); it is continued here"),
        "{stderr}"
    );
    let briefing = fixture.argv().join("\n");
    assert!(
        briefing.contains(&format!("which worked on it in {}.", work.display())),
        "{briefing}"
    );

    // An intent no session carries is refused before anything starts.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent-id", "in-1-1", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    assert_eq!(code, Some(3), "{stderr}");
    assert!(
        stderr.contains("no session in the log carries intent in-1-1"),
        "{stderr}"
    );
    assert_eq!(
        export_of(&fixture, &repo)["sessions"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "the refused launch recorded nothing"
    );
    // Neither an intent nor an intent id: nothing to launch for.
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--", "-c", "exit 0"])
            .current_dir(&repo),
    );
    assert_eq!(code, Some(2), "{stderr}");
    fixture.cleanup();
}

/// Whether any process is left in process group `pgid`.
fn group_alive(pgid: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{pgid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// D4, audit finding 5: the session runs in a process group of its own,
/// which its entering record names. A session whose ohjaa was killed while
/// its agent runs on is still running, not orphaned; once the agent is gone
/// too, it is closed as orphaned.
#[test]
fn a_session_whose_agent_outlives_its_killed_ohjaa_is_still_running_not_orphaned() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("still-running");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let agent_pid = fixture.dir.join("agent.pid");
    let mut child = spawn_session(
        &fixture,
        &repo,
        // Written whole, then moved into place: never read half written.
        &format!(
            "echo $$ > '{0}.new'; mv '{0}.new' '{0}'; exec sleep 60",
            agent_pid.display()
        ),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !agent_pid.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let agent: u32 = fs::read_to_string(&agent_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The group is recorded just after the spawn: wait for that write.
    let entering = loop {
        let entering = export_of(&fixture, &repo)["sessions"][0].clone();
        if entering.get("pgid").is_some() || std::time::Instant::now() > deadline {
            break entering;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let id = entering["id"].as_str().unwrap().to_owned();
    let pgid = entering["pgid"]
        .as_u64()
        .expect("the group, in the entering record") as u32;
    assert_eq!(
        pgid, agent,
        "the agent leads its own group (toimija execs it)"
    );
    signal(child.id(), "KILL");
    let _ = child.wait();
    assert!(group_alive(pgid), "the agent outlives its ohjaa");

    // The next launch sees the agent still at work: still running, not orphaned.
    let launch = || {
        output_of(
            fixture
                .ohjaa()
                .args(["run", "sh", "--intent", "next", "--", "-c", "exit 0"])
                .env("OHJAA_STUB_RUN_AGENT", "1")
                .current_dir(&repo),
        )
    };
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("ohjaa: session {id} (sh, started "))
            && stderr.contains(&format!(
                "is still running in process group {pgid}, but the ohjaa that started it is \
                 gone; it is left open"
            )),
        "{stderr}"
    );
    assert!(!stderr.contains("closed as orphaned"), "{stderr}");
    let briefing = fixture.argv().join("\n");
    assert!(
        briefing.contains(&format!("STILL RUNNING: session {id} (sh, long)")),
        "{briefing}"
    );
    let export = export_of(&fixture, &repo);
    let first = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .unwrap()
        .clone();
    assert!(first.get("left").is_none(), "still open: {first:#}");
    // The report says the same, as its own state.
    let (stdout, _, _) = output_of(
        fixture
            .ohjaa()
            .arg("--root")
            .arg(&fixture.dir)
            .args(["report", "--json", "--marks"])
            .current_dir(&repo),
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let state = report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .map(|s| s["state"].clone());
    assert_eq!(state, Some(serde_json::json!("ohjaa_gone")), "{report:#}");

    // The agent ends too: now it is an orphan.
    signal(agent, "KILL");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while group_alive(pgid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let (_, stderr, code) = launch();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("ohjaa: session {id} (sh, started "))
            && stderr.contains("was left open by an ohjaa that is gone; closed as orphaned"),
        "{stderr}"
    );
    let export = export_of(&fixture, &repo);
    let first = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(first["left"]["orphaned"], true, "{first:#}");
    fixture.cleanup();
}

/// Nit 1 of the review of #45: a process the agent left behind in the
/// session's group (a background shell, a server) does not keep a session
/// running whose ohjaa and agent are gone: the group's leader decides.
#[test]
fn a_process_left_in_the_group_does_not_keep_a_dead_session_running() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("stray");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let (pids, stray) = (fixture.dir.join("agent.pid"), fixture.dir.join("stray.pid"));
    let mut child = spawn_session(
        &fixture,
        &repo,
        // Written whole, then moved into place: never read half written.
        &format!(
            "sleep 60 & echo $! > '{1}.new'; mv '{1}.new' '{1}'; \
             echo $$ > '{0}.new'; mv '{0}.new' '{0}'; exec sleep 60",
            pids.display(),
            stray.display()
        ),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !pids.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let read = |path: &Path| -> u32 { fs::read_to_string(path).unwrap().trim().parse().unwrap() };
    let (agent, left_behind) = (read(&pids), read(&stray));
    let entering = loop {
        let entering = export_of(&fixture, &repo)["sessions"][0].clone();
        if entering.get("pgid").is_some() || std::time::Instant::now() > deadline {
            break entering;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let id = entering["id"].as_str().unwrap().to_owned();
    assert_eq!(
        entering["pgid"].as_u64(),
        Some(u64::from(agent)),
        "{entering:#}"
    );
    signal(child.id(), "KILL");
    let _ = child.wait();
    signal(agent, "KILL");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while Command::new("kill")
        .args(["-0", &agent.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        group_alive(agent),
        "what the agent left behind is still in its group"
    );
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "next", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    signal(left_behind, "KILL");
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("ohjaa: session {id} (sh, started "))
            && stderr.contains("was left open by an ohjaa that is gone; closed as orphaned"),
        "{stderr}"
    );
    assert!(!stderr.contains("still running"), "{stderr}");
    fixture.cleanup();
}

/// Nit 2 of the review of #45: the session's group is in its entering record
/// before toimija runs at all, so an ohjaa killed the moment toimija starts
/// still leaves a record that names the group: the next launch sees the
/// agent still running, never an orphan.
#[test]
fn the_group_is_recorded_before_toimija_runs() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("window");
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    let pids = fixture.dir.join("agent.pid");
    let mut ohjaa = fixture
        .ohjaa()
        .args([
            "run",
            "sh",
            "--intent",
            "window",
            "--no-briefing",
            "--",
            "-c",
        ])
        .arg(format!(
            "echo $$ > '{0}.new'; mv '{0}.new' '{0}'; exec sleep 60",
            pids.display()
        ))
        .env("OHJAA_STUB_RUN_AGENT", "1")
        .env("OHJAA_STUB_KILL_PARENT", "1")
        .current_dir(&repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _ = ohjaa.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !pids.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let agent: u32 = fs::read_to_string(&pids).unwrap().trim().parse().unwrap();
    let entering = export_of(&fixture, &repo)["sessions"][0].clone();
    assert_eq!(
        entering["pgid"].as_u64(),
        Some(u64::from(agent)),
        "the group, recorded before toimija ran: {entering:#}"
    );
    let (_, stderr, _) = output_of(
        fixture
            .ohjaa()
            .args(["run", "sh", "--intent", "next", "--", "-c", "exit 0"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .current_dir(&repo),
    );
    signal(agent, "KILL");
    assert!(
        stderr.contains("is still running in process group"),
        "{stderr}"
    );
    assert!(!stderr.contains("closed as orphaned"), "{stderr}");
    fixture.cleanup();
}

/// D4, audit finding 3: a phase whose receipt reused tasks from sykli's
/// cache is said so in the record and the next briefing; cached is never
/// folded into passed.
#[test]
fn a_receipt_that_reused_cached_tasks_is_said_so() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("cached-phase");
    let repo = fixture.make_git_repo("work");
    let script = r#"set -e
        echo one > one; git add one; git commit -q -m one
        tree=$(git rev-parse 'HEAD^{tree}')
        mkdir -p .sykli/receipts
        printf '{"schema":"sykli-receipt.v1","outcome":"passed","started_at_ms":%s000,"subject":{"tree_oid":"%s","head_tree_oid":"%s","dirty":false},"tasks":[{"name":"a","outcome":"passed"},{"name":"b","outcome":"cached"}]}' "$(date +%s)" "$tree" "$tree" > .sykli/receipts/rcpt_one.json
        echo two > two; git add two; git commit -q -m two
        tree=$(git rev-parse 'HEAD^{tree}')
        printf '{"schema":"sykli-receipt.v1","outcome":"cached","started_at_ms":%s000,"subject":{"tree_oid":"%s","head_tree_oid":"%s","dirty":false},"tasks":[{"name":"a","outcome":"cached"},{"name":"b","outcome":"cached"}]}' "$(date +%s)" "$tree" "$tree" > .sykli/receipts/rcpt_two.json"#;
    let (_, stderr, code) = run_session(&fixture, &repo, script);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("receipt: passed (1 of 2 tasks cached)")
            && stderr.contains("receipt: cached (all 2 tasks cached)"),
        "{stderr}"
    );
    let (stdout, _, _) = output_of(fixture.ohjaa().arg("brief").current_dir(&repo));
    assert!(
        stdout.contains(
            "- work: 2 commit(s), 1 whose sykli receipt says passed (1 reusing cached tasks), \
             1 whose receipt says cached (no task run)"
        ),
        "{stdout}"
    );
    fixture.cleanup();
}

// ---- the human view: `ohjaa line`, `ohjaa digest`, `ohjaa collect` --------

/// What an agent's shell carries (the list `ohjaa line --help` names): a
/// person's terminal has none, and these tests may run in an agent's.
const AGENT_MARKERS: [&str; 17] = [
    "OHJAA_SESSION",
    "TOIMIJA_SESSION",
    "TOIMIJA_RUN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_CI",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_MANAGED_BY_NPM",
    "PI_SESSION_ID",
    "PI_CODING_AGENT",
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    "AI_AGENT",
];

/// One page of GitHub's search: a pull request merged into main an hour
/// ago and one opened then, in false-systems.
/// With `extra`, that many more merged into sykli's main half an hour ago.
fn graphql_page(more: bool, extra: u64) -> String {
    graphql_page_at(unix_seconds(), more, extra)
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// [`graphql_page`], its times counted back from `now` (unix seconds): two
/// answers from one base differ in what they hold, never in when a pull
/// request happened, however long apart they are written.
fn graphql_page_at(now: i64, more: bool, extra: u64) -> String {
    let at = |ago_s: i64| {
        let (days, s) = (
            (now - ago_s).div_euclid(86_400),
            (now - ago_s).rem_euclid(86_400),
        );
        // Civil from days (Howard Hinnant), as the binary does.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            s / 3600,
            s % 3600 / 60,
            s % 60
        )
    };
    let repo = |name: &str| {
        serde_json::json!({"nameWithOwner": format!("false-systems/{name}"),
                           "defaultBranchRef": {"name": "main"}})
    };
    let mut page = serde_json::json!({"data": {"search": {
        "issueCount": if more { 250 } else { 2 },
        "pageInfo": {"hasNextPage": more, "endCursor": if more { "next" } else { "" }},
        "nodes": [
            {"number": 76, "title": "Ask git once", "url": "https://github.com/x/76",
             "createdAt": at(7200), "mergedAt": at(3600), "closedAt": at(3600),
             "baseRefName": "main", "author": {"login": "yairfalse"},
             "mergedBy": {"login": "yairfalse"}, "repository": repo("sykli")},
            {"number": 16, "title": "docs: nothing refuses \u{1b}[2J", "url": "https://github.com/x/16",
             "createdAt": at(3600), "mergedAt": null, "closedAt": null,
             "baseRefName": "main", "author": {"login": "yairfalse"}, "mergedBy": null,
             "repository": repo("faija")},
            {},
        ],
    }}});
    let nodes = page["data"]["search"]["nodes"].as_array_mut().unwrap();
    for number in 0..extra {
        nodes.push(serde_json::json!({
            "number": 100 + number, "title": format!("change {number}"),
            "url": format!("https://github.com/x/{}", 100 + number),
            "createdAt": at(1800), "mergedAt": at(1800), "closedAt": at(1800),
            "baseRefName": "main", "author": {"login": "yairfalse"},
            "mergedBy": {"login": "yairfalse"}, "repository": repo("sykli")}));
    }
    page.to_string()
}

impl Fixture {
    /// The workflow home with an entry of false-systems (whose pull
    /// requests are asked for) and a clone of it under the roots, its one
    /// commit at origin/main unproven, and GitHub's answer for the search.
    fn human_view(&self, more: bool) {
        self.human_view_with(more, 0);
    }

    fn human_view_with(&self, more: bool, extra: u64) {
        let entry = self.dir.join("config/ohjaa/repos/false-systems/sykli");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("repo.toml"), "branch = \"main\"\n").unwrap();
        let clone = self.dir.join("sykli");
        fs::create_dir_all(&clone).unwrap();
        run_git(&clone, &["init", "-q", "-b", "main"]);
        run_git(
            &clone,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/false-systems/sykli.git",
            ],
        );
        run_git(
            &clone,
            &[
                "-c",
                "user.email=o@example.invalid",
                "-c",
                "user.name=O",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "one",
            ],
        );
        run_git(&clone, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        fs::write(self.dir.join("graphql.json"), graphql_page(more, extra)).unwrap();
    }

    /// The real binary as a person's terminal runs it: no agent's markers,
    /// the fixture as its root, gh answering the search.
    fn person(&self) -> Command {
        let mut command = self.ohjaa();
        for marker in AGENT_MARKERS {
            command.env_remove(marker);
        }
        command
            .arg("--root")
            .arg(&self.dir)
            .current_dir(self.dir.join("home"))
            .env("OHJAA_STUB_GH_GRAPHQL", self.dir.join("graphql.json"))
            .env("TZ", "UTC");
        command
    }

    fn human_dir(&self) -> PathBuf {
        self.dir.join("state/ohjaa/human")
    }

    fn graphql_calls(&self) -> usize {
        fs::read_to_string(self.dir.join("home/gh-graphql-calls"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Wait for the background collect a line started: its snapshot
    /// written, and its lock let go.
    fn collected(&self) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let written = self.human_dir().join("collected.json").is_file();
            if written && !lock_held(&self.human_dir().join("collect.lock")) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }
}

/// Whether another process holds the flock(2) on `path`.
fn lock_held(path: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(file) = fs::OpenOptions::new().write(true).open(path) else {
        return false;
    };
    // SAFETY: flock(2) on a descriptor this function owns.
    let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    !free
}

/// Run `command` with its stdout on a pseudo-terminal, as a terminal's
/// opening runs it: what it printed there (line breaks as `\n`), and its
/// exit.
fn on_a_terminal(command: &mut Command) -> (String, Option<i32>) {
    use std::io::Read as _;
    use std::os::fd::FromRawFd;
    // SAFETY: plain pty calls on descriptors this function owns.
    let (mut master, slave) = unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "no pty");
        assert!(libc::grantpt(master) == 0 && libc::unlockpt(master) == 0);
        let name = std::ffi::CStr::from_ptr(libc::ptsname(master)).to_owned();
        let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0, "no pty slave");
        (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave))
    };
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(slave))
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // A pty holds little: read while it writes.
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(n) = master.read(&mut buffer) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buffer[..n]);
        }
        out
    });
    let status = child.wait().unwrap();
    // Dropping `command`'s copy of the slave closes the last one.
    drop(std::mem::replace(command, Command::new("true")));
    let out = reader.join().unwrap();
    (
        String::from_utf8_lossy(&out).replace('\r', ""),
        status.code(),
    )
}

/// Audit finding: agent shells used up the digest. Without a terminal, or
/// in an agent's shell at one, `line` prints, reads, writes and starts
/// nothing.
#[test]
fn line_says_nothing_and_moves_nothing_without_a_person_at_a_terminal() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-guard");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    run_session(&fixture, &repo, "exit 0");
    let (stdout, _, code) = output_of(fixture.person().arg("line"));
    assert_eq!((stdout.as_str(), code), ("", Some(0)), "piped: nothing");
    for marker in [
        "OHJAA_SESSION",
        "CLAUDECODE",
        "CODEX_THREAD_ID",
        "CODEX_CI",
        "PI_CODING_AGENT",
        "AI_AGENT",
    ] {
        let (said, code) = on_a_terminal(fixture.person().arg("line").env(marker, "1"));
        assert_eq!((said.as_str(), code), ("", Some(0)), "{marker}: nothing");
    }
    assert!(!fixture.human_dir().exists(), "nothing written, no collect");
    assert_eq!(fixture.graphql_calls(), 0);
    // A person at a terminal: a collect starts, and what is new is said.
    let (said, _) = on_a_terminal(fixture.person().arg("line"));
    assert!(said.contains("needs your attention"), "{said}");
    assert!(!said.contains("agent session ended"), "{said}");
    assert!(fixture.collected());
    assert!(fixture.human_dir().join("seen.json").is_file());
    fixture.cleanup();
}

/// The ruling: the first `line` of each local day prints the day's digest,
/// then one line only when something is new. Audit findings: seeing a line
/// moves the cursor, and sessions without read-back or handover are said.
#[test]
fn startup_is_attention_only_and_activity_stays_unread_until_the_digest() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-day");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    run_session(&fixture, &repo, "exit 0");
    let (_, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(0), "{stderr}");
    let day = |date: &str| {
        let (said, code) = on_a_terminal(fixture.person().arg("line").env("OHJAA_TODAY", date));
        assert_eq!(code, Some(0));
        said
    };
    let digest = day("2026-10-27");
    assert!(
        digest.starts_with("ohjaa — needs your attention"),
        "{digest}"
    );
    assert!(
        !digest.contains("agent session ended") && !digest.contains("#76"),
        "{digest}"
    );
    assert_eq!(day("2026-10-27"), "", "attention already shown");
    run_session(&fixture, &repo, "exit 2");
    assert_eq!(day("2026-10-27"), "", "activity does not interrupt startup");
    let next = day("2026-10-28");
    assert!(next.starts_with("ohjaa — needs your attention"), "{next}");
    assert!(
        !next.contains("retry uploads") && !next.contains("#76"),
        "{next}"
    );
    assert_eq!(day("2026-10-28"), "");
    let (digest, _) = on_a_terminal(
        fixture
            .person()
            .args(["digest", "--no-collect"])
            .env("OHJAA_TODAY", "2026-10-28"),
    );
    for said in [
        "2 agent sessions ended",
        "Pull requests: 1 merged, 1 opened",
        "sh \"retry uploads\" · process exited successfully (code 0)",
        "process exited with code 2",
        "no task confirmation or completion note recorded",
        "#76 merged by yairfalse: Ask git once",
        "#16 opened by yairfalse: docs: nothing refuses \u{fffd}[2J",
    ] {
        assert!(digest.contains(said), "missing `{said}`: {digest}");
    }
    assert!(!digest.contains("#76 opened"), "said once: {digest}");
    assert_eq!(day("2026-10-28"), "");
    fixture.cleanup();
}

/// The ruling: `line` makes no network call and starts no git process; with
/// a fresh snapshot it starts nothing at all.
#[test]
fn quiet_startup_retains_unread_sessions_and_pull_requests_across_days() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("quiet-history");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    run_session(&fixture, &repo, "exit 0");
    output_of(fixture.person().arg("collect"));
    // The rendering fixture has activity but no outstanding branch notices.
    let path = fixture.human_dir().join("collected.json");
    let mut collected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    collected["local"]["value"]["branches"] = serde_json::json!([]);
    fs::write(path, serde_json::to_string(&collected).unwrap()).unwrap();
    let day = |date: &str| on_a_terminal(fixture.person().arg("line").env("OHJAA_TODAY", date)).0;
    assert_eq!(day("2026-10-27"), "");
    assert_eq!(day("2026-10-28"), "");
    let seen: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fixture.human_dir().join("seen.json")).unwrap())
            .unwrap();
    assert!(
        seen["digest"].is_object(),
        "quiet startup anchors the unread window"
    );
    let (digest, code) = on_a_terminal(
        fixture
            .person()
            .args(["digest", "--no-collect"])
            .env("OHJAA_TODAY", "2026-10-28"),
    );
    assert_eq!(code, Some(0));
    assert!(
        digest.contains("sh \"retry uploads\" · process exited successfully (code 0)"),
        "{digest}"
    );
    assert_eq!(
        digest.matches("#76 merged by yairfalse").count(),
        1,
        "{digest}"
    );
    assert!(digest.contains("#16 opened by yairfalse"), "{digest}");
    let (again, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(again.contains("No unread activity recorded"), "{again}");
    fixture.cleanup();
}

#[test]
fn line_starts_no_git_and_touches_no_network() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-quiet");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    run_session(&fixture, &repo, "exit 0");
    output_of(fixture.person().arg("collect"));
    // Every program it could start, each logging that it was started.
    let shims = fixture.dir.join("shims");
    fs::create_dir_all(&shims).unwrap();
    for program in ["git", "gh", "vessa", "sykli", "toimija", "ps", "kill", "sh"] {
        let path = shims.join(program);
        write_executable(
            &path,
            format!(
                "#!/bin/sh\necho {program} >> '{}'\n",
                fixture.dir.join("started").display()
            ),
        );
    }
    let trace = fixture.dir.join("git-trace");
    let mut command = fixture.person();
    command
        .arg("line")
        .current_dir(&repo)
        .env("PATH", &shims)
        .env("GIT_TRACE2_EVENT", &trace)
        .env("OHJAA_TODAY", "2026-10-27");
    let (said, code) = on_a_terminal(&mut command);
    assert_eq!(code, Some(0));
    assert!(
        said.starts_with("ohjaa — needs your attention"),
        "attention without processes: {said}"
    );
    assert!(!fixture.dir.join("started").exists(), "started a process");
    assert!(!trace.exists(), "started git");
    fixture.cleanup();
}

/// A stale snapshot starts one collect in the background, never one per
/// terminal; a running one is never joined by a second.
#[test]
fn a_stale_snapshot_starts_exactly_one_background_collect() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-collect");
    fixture.human_view(false);
    fs::create_dir_all(fixture.human_dir()).unwrap();
    // A collect runs: the lock is held, so no line starts another.
    let lock = fixture.human_dir().join("collect.lock");
    let held = fs::File::create(&lock).unwrap();
    {
        use std::os::fd::AsRawFd;
        // SAFETY: flock(2) on a descriptor this test owns.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    }
    on_a_terminal(fixture.person().arg("line"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!fixture.human_dir().join("collected.json").exists());
    drop(held);
    on_a_terminal(fixture.person().arg("line"));
    assert!(fixture.collected(), "the line's collect wrote a snapshot");
    assert_eq!(fixture.graphql_calls(), 1);
    // Fresh now: no collect.
    on_a_terminal(fixture.person().arg("line"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(fixture.graphql_calls(), 1);
    fixture.cleanup();
}

/// viemari's, ported: GitHub is asked at most twice, and a source that
/// fails never fails the collect: it is said, as of when it last answered.
#[test]
fn collect_asks_github_at_most_twice_and_says_what_failed() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("collect-gh");
    fixture.human_view(true);
    // A third page is never asked for: what it held is not read, and said.
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    assert_eq!(fixture.graphql_calls(), 2, "never a third page");
    let (digest, _, _) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert!(
        digest.contains("250 pull requests were updated since"),
        "{digest}"
    );
    // gh cannot be used: the collect says so and exits 4, keeping what
    // GitHub said before.
    let (stdout, _, code) = output_of(
        fixture
            .person()
            .arg("collect")
            .env_remove("OHJAA_STUB_GH_GRAPHQL"),
    );
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("github  unavailable: gh api graphql: gh: To get started"),
        "{stdout}"
    );
    let (digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{digest}");
    assert!(
        digest.contains("GitHub (pull request activity) as of "),
        "{digest}"
    );
    assert!(digest.contains("#76 merged by yairfalse"), "kept: {digest}");
    fixture.cleanup();
}

/// viemari's, ported: a state directory the cursor cannot be written to
/// never repeats the day's digest at every terminal.
#[test]
fn a_cursor_that_cannot_be_written_never_repeats_the_days_digest() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-readonly");
    fixture.human_view(false);
    output_of(fixture.person().arg("collect"));
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture.human_dir();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let (said, code) = on_a_terminal(fixture.person().arg("line"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!((said.as_str(), code), ("", Some(0)));
    fixture.cleanup();
}

/// The digest moves the cursor only when a person reads it; `--since`
/// never does. What a session cost is from vessa, by the agent's own
/// session ohjaa found at leaving.
#[test]
fn the_digest_says_what_a_session_cost_and_moves_the_cursor_only_for_a_person() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("digest");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    fixture.set_context(r#"{"repo":{"name":"work","role":"a test repo","not_roles":[]}}"#);
    fake_claude(&fixture);
    let native = "5e1976e4-71c3-407c-9533-0be05b7e1b70";
    let (_, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "costed"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_FAKE_NATIVE", native)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stderr}");
    fs::write(
        fixture.dir.join("home/vessa-usage.json"),
        serde_json::json!({"schema": "vessa-usage.v2", "warnings": [], "sessions": [
            {"agent": "claude", "id": native, "limitations": [],
             "totals": [{"input_tokens": 62, "output_tokens": 20305,
                         "cache_read_tokens": 2923935, "cache_write_tokens": 94881}]}]})
        .to_string(),
    )
    .unwrap();
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let calls = fs::read_to_string(fixture.dir.join("home/vessa-calls")).unwrap();
    let usage: Vec<&str> = calls.lines().filter(|l| l.starts_with("usage")).collect();
    assert_eq!(usage.len(), 1, "{calls}");
    assert!(
        usage[0].starts_with("usage --json --input ")
            && usage[0].ends_with(&format!("{native}.jsonl")),
        "only its transcript: {calls}"
    );
    let (digest, _, code) = output_of(fixture.person().arg("digest"));
    assert_eq!(code, Some(0), "{digest}");
    assert!(
        digest.contains("claude \"costed\" · process exited successfully (code 0)"),
        "{digest}"
    );
    // Cache reads apart: they are most of it.
    assert!(
        digest.contains("115k tokens + 2.9M cache reads"),
        "{digest}"
    );
    assert!(!fixture.human_dir().join("seen.json").exists(), "piped");
    let (said, _) = on_a_terminal(fixture.person().args(["digest", "--since", "7d"]));
    assert!(said.contains("Sessions ended"), "{said}");
    assert!(!fixture.human_dir().join("seen.json").exists(), "--since");
    let (said, _) = on_a_terminal(fixture.person().arg("digest"));
    assert!(said.contains("Sessions ended"), "{said}");
    assert!(
        fixture.human_dir().join("seen.json").is_file(),
        "a person read it"
    );
    // Seen: today's digest is done, and nothing is new.
    let (said, _) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(said, "");
    fixture.cleanup();
}

#[test]
fn shell_init_adds_the_line_after_the_launcher() {
    let fixture = Fixture::new("shell-init-line");
    let (stdout, _, code) = output_of(fixture.ohjaa().args(["shell-init", "zsh"]));
    assert_eq!(code, Some(0));
    assert!(stdout.contains("_ohjaa_launch()"), "{stdout}");
    assert!(
        stdout
            .trim_end()
            .ends_with("[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line"),
        "{stdout}"
    );
    fixture.cleanup();
}

// ---- the review of #63 --------------------------------------------------------

impl Fixture {
    /// The snapshot as if collected `hours` ago: every part read then.
    fn age_snapshot(&self, hours: i64) {
        let path = self.human_dir().join("collected.json");
        let mut snapshot: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let back = |value: &mut serde_json::Value| {
            if let Some(ms) = value.as_i64() {
                *value = serde_json::json!(ms - hours * 3_600_000);
            }
        };
        back(&mut snapshot["at_ms"]);
        for part in ["local", "github", "usage"] {
            back(&mut snapshot[part]["as_of_ms"]);
        }
        for branch in snapshot["local"]["value"]["branches"]
            .as_array_mut()
            .unwrap()
        {
            back(&mut branch["as_of_ms"]);
        }
        fs::write(&path, snapshot.to_string()).unwrap();
    }

    /// Wait until GitHub was asked `calls` times and no collect runs.
    fn asked_github(&self, calls: usize) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if self.graphql_calls() >= calls && !lock_held(&self.human_dir().join("collect.lock")) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }
}

/// Finding 1: an entry that cannot be marked, or repositories whose sessions
/// cannot be listed, are said as what could not be read, in the day's
/// digest and the full one, and the digest exits 4: never an all-clear.
/// Finding 6: the fleet is not in the snapshot; the digest asks for it.
#[test]
fn what_could_not_be_read_is_said_never_an_all_clear() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-unread");
    fixture.human_view(false);
    let valvo = fixture.dir.join("config/ohjaa/repos/false-systems/valvo");
    fs::create_dir_all(&valvo).unwrap();
    fs::write(valvo.join("repo.toml"), "branch = \"main\"\n").unwrap();
    fixture.make_repo("broken");
    let (stdout, _, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(4), "{stdout}");
    assert!(stdout.contains("1 could not be read"), "{stdout}");
    let snapshot = fs::read_to_string(fixture.human_dir().join("collected.json")).unwrap();
    assert!(!snapshot.contains("\"fleet\""), "{snapshot}");
    let (digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{digest}");
    for said in [
        "! marks",
        "1 could not be read",
        "false-systems/valvo",
        "! sykli",
        "main: no verified passing result for 1 recent commit",
        "could not be scanned",
    ] {
        assert!(digest.contains(said), "missing `{said}`: {digest}");
    }
    assert!(!digest.contains("nobody is live"), "{digest}");
    let (daily, _) = on_a_terminal(
        fixture
            .person()
            .arg("line")
            .env("OHJAA_TODAY", "2026-10-27"),
    );
    assert!(daily.contains("! marks"), "{daily}");
    assert!(daily.contains("1 could not be read"), "{daily}");
    fixture.cleanup();
}

/// Finding 3: what the day's digest left out (`… N more: ohjaa digest`) is
/// what `ohjaa digest` shows next.
#[test]
fn what_the_days_digest_left_out_is_what_ohjaa_digest_shows() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-more");
    fixture.human_view_with(false, 20);
    output_of(fixture.person().arg("collect"));
    let (daily, _) = on_a_terminal(
        fixture
            .person()
            .arg("line")
            .env("OHJAA_TODAY", "2026-10-27"),
    );
    assert!(
        daily.contains("Full details and activity: ohjaa digest"),
        "{daily}"
    );
    assert!(!daily.contains("#119 merged"), "{daily}");
    let (digest, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    for number in [76, 100, 110, 119] {
        assert!(
            digest.contains(&format!("#{number} merged by yairfalse")),
            "#{number}: {digest}"
        );
    }
    let (again, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(
        again.contains("No unread activity recorded"),
        "seen in full: {again}"
    );
    fixture.cleanup();
}

/// Finding 4: the first terminal of a day gets the day's digest from the
/// snapshot there is, said as of when it was collected, and a collect starts
/// beside it; nothing waits.
#[test]
fn the_days_first_terminal_gets_its_digest_from_an_old_snapshot() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-old");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    run_session(&fixture, &repo, "exit 0");
    output_of(fixture.person().arg("collect"));
    fixture.age_snapshot(9);
    let (daily, _) = on_a_terminal(
        fixture
            .person()
            .arg("line")
            .env("OHJAA_TODAY", "2026-10-27"),
    );
    assert!(daily.starts_with("ohjaa — needs your attention"), "{daily}");
    assert!(daily.contains("Cached status collected "), "{daily}");
    assert!(!daily.contains("retry uploads"), "{daily}");
    assert!(fixture.asked_github(2), "a collect started beside it");
    let (again, _) = on_a_terminal(
        fixture
            .person()
            .arg("line")
            .env("OHJAA_TODAY", "2026-10-27"),
    );
    assert_eq!(again, "", "the day's digest once");
    fixture.cleanup();
}

/// Finding 7: terminals opened together say a line once: the one that
/// claims it says it.
#[test]
fn terminals_opened_together_say_a_line_once() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-claim");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    output_of(fixture.person().arg("collect"));
    on_a_terminal(
        fixture
            .person()
            .arg("line")
            .env("OHJAA_TODAY", "2026-10-27"),
    );
    run_session(&fixture, &repo, "exit 0");
    let clone = fixture.dir.join("sykli");
    run_git(
        &clone,
        &[
            "-c",
            "user.email=o@example.invalid",
            "-c",
            "user.name=O",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "two",
        ],
    );
    run_git(&clone, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    output_of(fixture.person().arg("collect"));
    let claim = fixture.human_dir().join("seen.lock");
    let held = fs::File::create(&claim).unwrap();
    {
        use std::os::fd::AsRawFd;
        // SAFETY: flock(2) on a descriptor this test owns.
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    }
    let line = || {
        on_a_terminal(
            fixture
                .person()
                .arg("line")
                .env("OHJAA_TODAY", "2026-10-27"),
        )
        .0
    };
    assert_eq!(line(), "", "another terminal is saying it");
    drop(held);
    assert!(line().contains("sykli main: no passing verification recorded"));
    assert_eq!(line(), "");
    fixture.cleanup();
}

// ---- the second review of #63 -------------------------------------------------

/// Hold the flock(2) on `path` until the file is dropped.
fn hold(path: &Path) -> fs::File {
    use std::os::fd::AsRawFd;
    let held = fs::File::create(path).unwrap();
    // SAFETY: flock(2) on a descriptor this function owns.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    held
}

/// Finding 1: when the entries themselves cannot be listed, every branch
/// stays as it was last read, said with that time, and nothing like an
/// entry named `(entries)` takes their place.
#[test]
fn a_branch_stays_as_last_read_when_the_entries_cannot_be_listed() {
    if !git_available() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("human-unlisted");
    fixture.human_view(false);
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let owner = fixture.dir.join("config/ohjaa/repos/false-systems");
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o000)).unwrap();
    let unlisted = output_of(fixture.person().arg("collect"));
    let marks = output_of(fixture.person().args(["report", "--json", "--marks"]));
    fs::set_permissions(&owner, fs::Permissions::from_mode(0o755)).unwrap();
    let (stdout, stderr, code) = unlisted;
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    // Review of #67, finding 3: `ohjaa-marks.v1` says why no entry is
    // listed, in its own key.
    let (stdout, stderr, code) = marks;
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        (&json["complete"], &json["entries"]),
        (&serde_json::json!(false), &serde_json::json!([]))
    );
    assert!(
        json["unlisted"]
            .as_str()
            .is_some_and(|why| why.starts_with("the entries cannot be listed: cannot read")),
        "{stdout}"
    );
    let snapshot = fs::read_to_string(fixture.human_dir().join("collected.json")).unwrap();
    assert!(!snapshot.contains("(entries)"), "{snapshot}");
    let (digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{digest}");
    let mut rows = digest.lines().skip_while(|line| !line.contains("! sykli"));
    let first = rows
        .next()
        .unwrap_or_else(|| panic!("sykli dropped from what stays: {digest}"));
    let still_open = std::iter::once(first)
        .chain(rows.take_while(|line| line.starts_with("    ")))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        still_open.contains("main: no verified passing result for 1 recent commit")
            && still_open.contains("Could not refresh:"),
        "{digest}"
    );
    fixture.cleanup();
}

/// The nit: a git that fails is no missing clone. The branch stays as it
/// was last read (the first review's finding 2, at the CLI), and the reason
/// says the clone could not be read.
#[test]
fn a_git_that_fails_is_said_as_that_never_as_a_missing_clone() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-git-fails");
    fixture.human_view(false);
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let broken = fixture.dir.join("broken-git");
    fs::create_dir_all(&broken).unwrap();
    let git = broken.join("git");
    write_executable(
        &git,
        "#!/bin/sh\necho 'fatal: git is broken here' >&2\nexit 128\n",
    );
    let path = std::env::join_paths(
        [broken, fixture.stub_dir.clone()]
            .into_iter()
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect").env("PATH", &path));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    let snapshot = fs::read_to_string(fixture.human_dir().join("collected.json")).unwrap();
    assert!(
        !snapshot.contains("no clone under the roots has it as origin"),
        "{snapshot}"
    );
    assert!(
        snapshot.contains("may have it as origin could not be read")
            && snapshot.contains("git is broken here"),
        "{snapshot}"
    );
    let (digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{digest}");
    assert!(
        digest.contains("main: no verified passing result for 1 recent commit")
            && digest.contains("Could not refresh:"),
        "{digest}"
    );
    fixture.cleanup();
}

/// Finding 2: what a day's digest left out stays until the person reads it
/// in full: the next day's digest points to it, and `ohjaa digest` shows it.
#[test]
fn what_a_days_digest_left_out_is_kept_past_the_next_day() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-kept");
    fixture.human_view_with(false, 20);
    output_of(fixture.person().arg("collect"));
    let day = |date: &str| on_a_terminal(fixture.person().arg("line").env("OHJAA_TODAY", date)).0;
    let first = day("2026-10-27");
    assert!(
        first.contains("Full details and activity: ohjaa digest"),
        "{first}"
    );
    let next = day("2026-10-28");
    assert!(next.starts_with("ohjaa — needs your attention"), "{next}");
    assert!(
        !next.contains("#76") && next.contains("ohjaa digest"),
        "{next}"
    );
    let (digest, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    for number in [76, 100, 119] {
        assert!(
            digest.contains(&format!("#{number} merged by yairfalse")),
            "#{number}: {digest}"
        );
    }
    let (again, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(again.contains("No unread activity recorded"), "{again}");
    let after = day("2026-10-29");
    assert!(!after.contains("left out"), "read in full: {after}");
    fixture.cleanup();
}

/// Finding 3: a pull request GitHub indexes late, merged before the
/// snapshot a digest was shown from, is in the next digest; what the last
/// digest said already is not said again.
#[test]
fn a_pull_request_github_indexes_late_is_in_the_next_digest_once() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-late");
    fixture.human_view(false);
    // Both of GitHub's answers count back from one time: a pull request
    // happened when it happened, whenever its answer is written.
    let base = unix_seconds();
    let page = |extra: u64| {
        fs::write(
            fixture.dir.join("graphql.json"),
            graphql_page_at(base, false, extra),
        )
        .unwrap();
    };
    page(0);
    // Review of #67, finding 2: a second or more between writing GitHub's
    // answer and reading it moves no pull request across the overlap's edge.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    output_of(fixture.person().arg("collect"));
    let (first, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(first.contains("#76 merged by yairfalse"), "{first}");
    assert!(!first.contains("#100"), "{first}");
    // #100 merged half an hour ago, before that snapshot, and is only now
    // in GitHub's search.
    page(1);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    output_of(fixture.person().arg("collect"));
    assert_eq!(fixture.graphql_calls(), 2, "no call more for the overlap");
    let (next, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(next.contains("#100 merged by yairfalse"), "{next}");
    assert!(!next.contains("#76"), "said already: {next}");
    assert!(!next.contains("#16"), "said already: {next}");
    fixture.cleanup();
}

/// The nit: `digest` moves the cursor under the claim a terminal's line
/// takes. One it cannot claim shows all the same and moves nothing, so
/// nothing is said twice or lost.
#[test]
fn a_digest_moves_the_cursor_only_under_the_claim() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-digest-claim");
    fixture.human_view(false);
    output_of(fixture.person().arg("collect"));
    let held = hold(&fixture.human_dir().join("seen.lock"));
    let (said, code) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(0), "{said}");
    assert!(said.contains("#76 merged by yairfalse"), "{said}");
    assert!(
        !fixture.human_dir().join("seen.json").exists(),
        "moved without the claim"
    );
    drop(held);
    let (said, _) = on_a_terminal(fixture.person().args(["digest", "--no-collect"]));
    assert!(
        said.contains("#76 merged by yairfalse"),
        "shown again: {said}"
    );
    assert!(fixture.human_dir().join("seen.json").is_file());
    fixture.cleanup();
}
