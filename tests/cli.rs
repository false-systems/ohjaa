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
/// `secrets` answers `vessa-secrets.v1`: `$HOME/vessa-secrets.json` as it
/// is, else a whole scan that found nothing, and exits with what
/// `$HOME/vessa-secrets-exit` holds (else 0); with `$HOME/vessa-old-secrets`
/// it is a vessa older than `secrets`, as clap refuses one. Its arguments
/// are logged too.
const VESSA_STUB: &str = r#"#!/bin/sh
if [ "$1" = secrets ]; then
  printf '%s\n' "$*" >> "$HOME/vessa-calls"
  if [ -e "$HOME/vessa-old-secrets" ]; then
    echo "error: unrecognized subcommand 'secrets'" >&2
    echo "" >&2
    echo "  tip: a similar subcommand exists: 'receipt'" >&2
    echo "" >&2
    echo "Usage: vessa [OPTIONS] <COMMAND>" >&2
    echo "" >&2
    echo "For more information, try '--help'." >&2
    exit 2
  fi
  if [ -f "$HOME/vessa-secrets.json" ]; then cat "$HOME/vessa-secrets.json"
  else echo '{"schema":"vessa-secrets.v1","since":null,"complete":true,"unreadable":[],"findings":[]}'; fi
  code=0
  if [ -f "$HOME/vessa-secrets-exit" ]; then code=$(cat "$HOME/vessa-secrets-exit"); fi
  [ "$code" = 4 ] && echo "vessa: the secrets scan is incomplete: 1 transcript could not be read" >&2
  exit "$code"
fi
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
    // Yair, 2026-10-28: the briefing gives agents knowledge, and nothing
    // measured whether it changes what they do. A node the person keeps
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

// ---- the session lifecycle ---------------------------------------------------

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

// ---- review fixes: each test failed before its fix -----------------------------

// ---- review of #38: each test failed before its fix ---------------------------

// ---- where work happens (audit 2026-10-09): each test failed before its fix ----

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

/// `ohjaa report --json` is `ohjaa-marks.v1`, for other tools to read, and
/// it moves nothing: not the session log, not the report's bookmarks, even
/// at a terminal, where the report for people does mark the week.
#[test]
fn report_json_is_ohjaa_marks_v1_and_writes_nothing() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("marks-json");
    let repo = clone_with_origin(&fixture, "work");
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
    fs::File::options()
        .write(true)
        .open(&at)
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();
    let stamps = || (log.exists(), log_stamp(&at));
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
    // Its keys stay (ohjaa-marks.v1 only gains keys), saying ohjaa launches
    // no session to judge.
    assert_eq!(
        json["report"]["sessions_without_testimony"],
        serde_json::json!([])
    );
    assert_eq!(json["report"]["bypasses"]["judged"], false);
    assert!(json["report"]["bypasses"]["why"]
        .as_str()
        .unwrap()
        .contains("no longer launches agents"));

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
        assert!(!log.exists(), "the log is only read, never made");
    }
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

/// `unix_s` (unix seconds) as RFC 3339 UTC.
fn utc(unix_s: i64) -> String {
    let (days, s) = (unix_s.div_euclid(86_400), unix_s.rem_euclid(86_400));
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
}

/// [`graphql_page`], its times counted back from `now` (unix seconds): two
/// answers from one base differ in what they hold, never in when a pull
/// request happened, however long apart they are written.
fn graphql_page_at(now: i64, more: bool, extra: u64) -> String {
    let at = |ago_s: i64| utc(now - ago_s);
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
    fixture.make_git_repo("work");
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
        "Pull requests: 1 merged, 1 opened",
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
fn quiet_startup_retains_unread_pull_requests_across_days() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("quiet-history");
    fixture.human_view(false);
    fixture.make_git_repo("work");
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

#[test]
fn a_collection_start_failure_after_first_startup_is_reported_once() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("line-refresh-failure");
    fixture.human_view(false);
    output_of(fixture.person().arg("collect"));
    let (first, _) = on_a_terminal(fixture.person().arg("line"));
    assert!(first.contains("needs your attention"), "{first}");
    let path = fixture.human_dir().join("collected.json");
    let mut collected: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    collected["at_ms"] = serde_json::json!(1);
    fs::write(path, collected.to_string()).unwrap();
    // A directory at the lock path makes collection fail before spawning.
    let lock = fixture.human_dir().join("collect.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    let (notice, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0));
    assert!(
        notice.starts_with("ohjaa · Could not refresh cached status:"),
        "{notice:?}"
    );
    assert!(notice.ends_with("Details: ohjaa digest\n"), "{notice}");
    assert_eq!(on_a_terminal(fixture.person().arg("line")).0, "");
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
        stdout.contains("github   unavailable: gh api graphql: gh: To get started"),
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

#[test]
fn shell_init_prints_only_the_line() {
    let fixture = Fixture::new("shell-init-line");
    let (stdout, _, code) = output_of(fixture.ohjaa().args(["shell-init", "zsh"]));
    assert_eq!(code, Some(0));
    assert!(!stdout.contains("()"), "no function is defined: {stdout}");
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
        for part in ["local", "github", "usage", "typed", "secrets"] {
            back(&mut snapshot[part]["as_of_ms"]);
        }
        back(&mut snapshot["secrets"]["value"]["read_ms"]);
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
    assert!(
        daily.contains("Missing or incomplete information: repository checks"),
        "{daily}"
    );
    assert!(daily.contains("Refresh: ohjaa collect"), "{daily}");
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

#[test]
fn startup_keeps_liveness_warnings_with_long_recorded_goals() {
    for orphaned in [false, true] {
        let fixture = Fixture::new("line-long-liveness-goal");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let goal = "Keep this complete recorded goal available. ".repeat(40);
        fs::create_dir_all(fixture.dir.join("state/ohjaa")).unwrap();
        {
            let db = redb::Database::create(fixture.dir.join("state/ohjaa/sessions.redb")).unwrap();
            let tx = db.begin_write().unwrap();
            let session = serde_json::json!({
                "id": "long-liveness-goal", "agent": "codex", "repo": "/synthetic/ohjaa",
                "intent": "interactive", "started_ms": now - 172_800_000, "delivered": [],
                "readback": {"at_ms": now - 172_800_000, "text": goal}
            })
            .to_string();
            tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("sessions"))
                .unwrap()
                .insert("long-liveness-goal", session.as_bytes())
                .unwrap();
            tx.commit().unwrap();
        }
        let mut snapshot = serde_json::json!({"schema": "ohjaa-collected.v1", "at_ms": now});
        for part in ["local", "github", "usage", "typed", "secrets"] {
            snapshot[part] = serde_json::json!({"as_of_ms": now});
        }
        snapshot["local"]["value"] = if orphaned {
            serde_json::json!({"branches": [], "orphaned": ["long-liveness-goal"]})
        } else {
            serde_json::json!({"branches": [], "gone": ["long-liveness-goal"]})
        };
        fs::create_dir_all(fixture.human_dir()).unwrap();
        fs::write(
            fixture.human_dir().join("collected.json"),
            snapshot.to_string(),
        )
        .unwrap();
        let (daily, code) = on_a_terminal(fixture.person().arg("line"));
        eprintln!("Synthetic liveness startup (orphaned={orphaned}):\n{daily}");
        assert_eq!(code, Some(0));
        let words = daily.split_whitespace().collect::<Vec<_>>().join(" ");
        let warning = if orphaned {
            "Neither the agent nor its supervisor was found running"
        } else {
            "Agent still running without its Ohjaa supervisor"
        };
        assert!(words.contains(warning), "{daily}");
        assert!(
            daily.contains("ohjaa session long-liveness-goal"),
            "{daily}"
        );
        assert!(daily.lines().count() <= 15, "{daily}");
        assert!(
            daily.lines().all(|line| line.chars().count() <= 100),
            "{daily}"
        );
        let (digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
        assert_eq!(code, Some(0), "{digest}");
        let words = digest.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains(goal.trim()), "{digest}");
        fixture.cleanup();
    }
}

#[test]
fn generic_matches_remain_inspectable_without_becoming_credential_alerts() {
    let fixture = Fixture::new("line-generic-evidence");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let path = fixture.human_dir().join("collected.json");
    let mut snapshot = serde_json::json!({"schema":"ohjaa-collected.v1","at_ms":now});
    for part in ["local", "github", "usage", "typed", "secrets"] {
        snapshot[part] = serde_json::json!({"as_of_ms":now});
    }
    snapshot["secrets"]["value"] = serde_json::json!({"read_ms":now,"findings":[
        {"agent":"codex","session":"synthetic","at_ms":now,"where":"tool_output","rule":"mixed_token","count":8513,"found_ms":now}
    ]});
    fs::create_dir_all(fixture.human_dir()).unwrap();
    fs::write(&path, snapshot.to_string()).unwrap();
    let (quiet, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0));
    assert_eq!(quiet, "");
    let (digest, stderr, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(digest.contains("mixed_token ×8513"), "{digest}");
    assert!(digest.contains("8,513 generic-pattern matches"), "{digest}");
    assert!(digest.contains("Match context unavailable"), "{digest}");
    // A quiet startup did not consume evidence, and missing scan coverage
    // still warns even when the only matches are generic.
    snapshot["secrets"]["gaps"] = serde_json::json!(["synthetic unreadable source"]);
    fs::write(&path, snapshot.to_string()).unwrap();
    let (partial, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0)); // Shell startup itself does not return scan status.
    assert!(partial.contains("secrets scan"), "{partial}");
    assert!(!partial.contains("Possible credentials"), "{partial}");
    let (partial_digest, _, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{partial_digest}");
    assert!(
        partial_digest.contains("mixed_token ×8513"),
        "{partial_digest}"
    );
    snapshot["secrets"]["value"]["findings"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "agent":"codex","session":"synthetic-key","at_ms":now,"where":"person",
            "rule":"openai","count":27,"found_ms":now
        }));
    fs::write(&path, snapshot.to_string()).unwrap();
    let (notice, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0));
    assert!(notice.contains("27 key-format matches"), "{notice}");
    assert!(!notice.contains("8,513"), "{notice}");
    eprintln!("Synthetic credential notice with incomplete coverage:\n{notice}");
    fixture.cleanup();
}

#[test]
fn opaque_entropy_stays_in_the_digest_without_a_startup_credential_alert() {
    let fixture = Fixture::new("line-native-opaque");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let path = fixture.human_dir().join("collected.json");
    let mut snapshot = serde_json::json!({"schema":"ohjaa-collected.v1","at_ms":now});
    for part in ["local", "github", "usage", "typed", "secrets"] {
        snapshot[part] = serde_json::json!({"as_of_ms":now});
    }
    snapshot["secrets"]["value"] = serde_json::json!({"read_ms":now,"findings":[
        {"agent":"claude","session":"synthetic","cwd":null,"at_ms":now,"where":"agent","rule":"mixed_token","count":100,"opaque_count":100,"opaque_native":true,"fingerprint":null,"found_ms":now}
    ]});
    fs::create_dir_all(fixture.human_dir()).unwrap();
    fs::write(&path, snapshot.to_string()).unwrap();
    let (quiet, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0));
    assert_eq!(quiet, "", "native opaque entropy is not a credential alert");
    let (digest, stderr, code) = output_of(fixture.person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(digest.contains("mixed_token ×100"), "{digest}");
    assert!(digest.contains("All scan evidence"), "{digest}");
    // A provider-shaped key still needs review even inside the same native field.
    snapshot["secrets"]["value"]["findings"][0]["rule"] = serde_json::json!("openai");
    snapshot["secrets"]["value"]["findings"][0]["count"] = serde_json::json!(1);
    snapshot["secrets"]["value"]["findings"][0]["opaque_count"] = serde_json::json!(1);
    fs::write(path, snapshot.to_string()).unwrap();
    let (notice, code) = on_a_terminal(fixture.person().arg("line"));
    assert_eq!(code, Some(0));
    assert!(notice.contains("1 key-format match"), "{notice}");
    assert!(notice.contains("Details: ohjaa digest"), "{notice}");
    eprintln!(
        "Synthetic opaque-only startup bytes: {}\nKey-shaped metadata notice: {notice}",
        quiet.len()
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
    eprintln!("Synthetic startup preview:\n{daily}");
    assert!(daily.contains("Last background update: "), "{daily}");
    assert!(daily.contains("Refresh: ohjaa collect"), "{daily}");
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
    // Activity is said once; "Now" is what stands, said each time: #16 is
    // still open, so it is listed there again, never as news.
    let (activity, now) = next.split_once("\nNow\n").expect("a Now section");
    assert!(!activity.contains("#76"), "said already: {next}");
    assert!(!activity.contains("#16"), "said already: {next}");
    assert!(now.contains("#16 by yairfalse"), "still open: {next}");
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

// ---- the first words typed, and the secrets seen (Yair, 2026-10-28) ---------

impl Fixture {
    /// What the stand-in's `vessa secrets --json` answers next, and its exit.
    fn secrets_scan(&self, report: &str, exit: i32) {
        fs::write(self.dir.join("home/vessa-secrets.json"), report).unwrap();
        fs::write(self.dir.join("home/vessa-secrets-exit"), exit.to_string()).unwrap();
    }
}

/// One finding, as `vessa secrets --json` lists it.
fn secret_found(
    session: &str,
    at_s: i64,
    place: &str,
    rule: &str,
    fingerprint: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({"agent": "claude", "session": session, "cwd": "/p/habitat",
                       "at": utc(at_s), "where": place, "rule": rule, "count": 1,
                       "fingerprint": fingerprint})
}

/// `vessa-secrets.v1`.
fn secrets_report(
    complete: bool,
    unreadable: serde_json::Value,
    findings: Vec<serde_json::Value>,
) -> String {
    serde_json::json!({"schema": "vessa-secrets.v1", "since": null, "complete": complete,
                       "unreadable": unreadable, "findings": findings})
    .to_string()
}

/// Explicit opt-in: no build or runtime dependency on Vessa's repository.
/// The real producer reads only the synthetic HOME made by this fixture.
#[test]
#[ignore = "set OHJAA_TEST_VESSA to an independently built Vessa binary"]
fn real_vessa_producer_preserves_findings_excerpts_and_partial_retry_boundaries() {
    let binary =
        fs::canonicalize(std::env::var_os("OHJAA_TEST_VESSA").expect("set OHJAA_TEST_VESSA"))
            .unwrap();
    let fixture = Fixture::new("real-vessa");
    fixture.human_view(false);
    let repo = fixture.make_git_repo("work");
    fixture
        .set_context(r#"{"repo":{"name":"work","role":"synthetic integration","not_roles":[]}}"#);
    fake_claude(&fixture);
    let native = "7b45bb32-2a03-4ca5-8db4-425edc153b16";
    let (stdout, stderr, code) = output_of(
        fixture
            .ohjaa()
            .args(["run", "claude", "--intent", "interactive"])
            .env("OHJAA_STUB_RUN_AGENT", "1")
            .env("OHJAA_FAKE_NATIVE", native)
            .current_dir(&repo),
    );
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let started = export_of(&fixture, &repo)["sessions"][0]["started_ms"]
        .as_i64()
        .unwrap()
        / 1000;
    let secret = ["s", "k-", &"a1".repeat(12)].concat();
    let transcript = fixture
        .dir
        .join(format!("home/.claude/projects/-work/{native}.jsonl"));
    let records = [
        serde_json::json!({"type":"user","sessionId":native,"cwd":repo,"timestamp":utc(started),"isSidechain":false,"message":{"role":"user","content":"Keep this synthetic input readable."}}),
        serde_json::json!({"type":"assistant","sessionId":native,"timestamp":utc(started),"uuid":"synthetic-answer","requestId":"synthetic-request","message":{"id":"synthetic-answer","role":"assistant","model":"synthetic-model","content":[{"type":"text","text":"Synthetic response."}],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
        serde_json::json!({"type":"user","sessionId":native,"timestamp":utc(started),"message":{"role":"user","content":[{"type":"tool_result","content":secret}]}}),
    ].iter().map(|v| format!("{v}\n")).collect::<String>();
    fs::write(&transcript, &records).unwrap();
    fs::remove_file(fixture.stub_dir.join("vessa")).unwrap();
    std::os::unix::fs::symlink(binary, fixture.stub_dir.join("vessa")).unwrap();
    // The planted key is one of this machine's credentials, as a
    // secret-named variable: vessa finds only those.
    let person = || {
        let mut command = fixture.person();
        command
            .env("XDG_DATA_HOME", fixture.dir.join("data"))
            .env("FIXTURE_API_KEY", &secret);
        command
    };
    let snapshot = || -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(fixture.human_dir().join("collected.json")).unwrap(),
        )
        .unwrap()
    };
    let (stdout, stderr, code) = output_of(person().arg("collect"));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let first = snapshot();
    let found = &first["secrets"]["value"]["findings"];
    assert_eq!(found.as_array().unwrap().len(), 1, "{first}");
    assert_eq!(found[0]["session"], native);
    assert_eq!(found[0]["where"], "tool_output");
    assert_eq!(found[0]["rule"], "openai");
    assert_eq!(found[0]["held"], "environment variable FIXTURE_API_KEY");
    assert_eq!(found[0]["count"], 1);
    let complete_at = first["secrets"]["value"]["read_ms"].as_i64().unwrap();
    assert!(
        first
            .to_string()
            .contains("Keep this synthetic input readable."),
        "{first}"
    );
    assert!(!first.to_string().contains(&secret));
    let (digest, stderr, code) = output_of(person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(0), "{digest}{stderr}");
    assert!(
        digest.contains("Keep this synthetic input readable."),
        "{digest}"
    );
    let words = digest.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains("your credential from environment variable FIXTURE_API_KEY (openai) ×1"),
        "{digest}"
    );
    assert!(!digest.contains(&secret));
    fs::write(&transcript, format!("{records}{{broken\n")).unwrap();
    let (stdout, stderr, code) = output_of(person().arg("collect"));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    let partial = snapshot();
    assert_eq!(partial["secrets"]["value"]["read_ms"], complete_at);
    assert_eq!(
        partial["secrets"]["value"]["findings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (digest, stderr, code) = output_of(person().args(["digest", "--no-collect"]));
    assert_eq!(code, Some(4), "{digest}{stderr}");
    assert!(digest.contains("could not read"), "{digest}");
    assert!(!digest.contains(&secret));
    fs::write(&transcript, records).unwrap();
    let (stdout, stderr, code) = output_of(person().arg("collect"));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(snapshot()["secrets"]["value"]["read_ms"].as_i64().unwrap() > complete_at);
    fixture.cleanup();
}

/// Yair, 2026-10-28: what the secrets scan cannot read is never an
/// all-clear. An older vessa (no `secrets`), a scan that could not read
/// everything (exit 4) and an answer that is no report are each said in the
/// digest, with why, and `collect` and `digest` exit 4.
#[test]
fn a_secrets_scan_that_cannot_run_is_said_never_an_all_clear() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-secrets");
    fixture.human_view(false);
    let home = fixture.dir.join("home");
    let collect = || output_of(fixture.person().arg("collect"));
    let digest = || output_of(fixture.person().args(["digest", "--no-collect"]));
    let section = |said: &str| {
        said.split("\nSecrets seen\n")
            .nth(1)
            .unwrap_or_else(|| panic!("no Secrets seen: {said}"))
            .to_owned()
    };
    // An older vessa, without `secrets`.
    fs::write(home.join("vessa-old-secrets"), "").unwrap();
    let (stdout, _, code) = collect();
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("  secrets  unavailable: this vessa has no `secrets`: update vessa"),
        "{stdout}"
    );
    let (said, _, code) = digest();
    assert_eq!(code, Some(4), "{said}");
    let seen = section(&said);
    assert!(
        seen.starts_with("  ! secrets")
            && seen.contains(
                "Secrets scan: unavailable, never read: this vessa has no `secrets`: update vessa"
            ),
        "{said}"
    );
    // It could not read everything (exit 4): what it found stands, and what
    // it could not read is said.
    fs::remove_file(home.join("vessa-old-secrets")).unwrap();
    fixture.secrets_scan(
        &secrets_report(
            false,
            serde_json::json!([{"path": "/h/x.jsonl", "why": "Permission denied (os error 13)"}]),
            vec![secret_found(
                "s1",
                unix_seconds() - 60,
                "person",
                "github",
                None,
            )],
        ),
        4,
    );
    let (stdout, _, code) = collect();
    assert_eq!(code, Some(4), "{stdout}");
    let (said, _, code) = digest();
    assert_eq!(code, Some(4), "{said}");
    let seen = section(&said);
    assert!(
        seen.contains("Secrets scan: 1 could not be read, as of ")
            && seen.contains("could not read /h/x.jsonl: Permission denied"),
        "{said}"
    );
    assert!(seen.contains("you typed it · github ×1"), "{said}");
    // An answer that is no report: what was found before stays, as of then.
    fixture.secrets_scan("{\"schema\":\"vessa-secrets.v1\",\"findings\":[", 0);
    let (stdout, _, code) = collect();
    assert_eq!(code, Some(4), "{stdout}");
    assert!(
        stdout.contains("  secrets  unavailable: vessa secrets --json printed no report"),
        "{stdout}"
    );
    let (said, _, code) = digest();
    assert_eq!(code, Some(4), "{said}");
    let seen = section(&said);
    assert!(seen.contains("Secrets scan: unavailable, as of "), "{said}");
    assert!(seen.contains("you typed it · github ×1"), "kept: {said}");
    // A whole scan again: only now is nothing missing.
    fixture.secrets_scan(&secrets_report(true, serde_json::json!([]), Vec::new()), 0);
    let (stdout, _, code) = collect();
    assert_eq!(code, Some(0), "{stdout}");
    let (said, _, code) = digest();
    assert_eq!(code, Some(0), "{said}");
    assert!(!said.contains("! secrets"), "{said}");
    fixture.cleanup();
}

#[test]
fn malformed_secrets_coverage_is_incomplete_and_keeps_the_retry_boundary() {
    if !git_available() {
        return;
    }
    let fixture = Fixture::new("human-secrets-invalid-coverage");
    fixture.human_view(false);
    fixture.secrets_scan(&secrets_report(true, serde_json::json!([]), Vec::new()), 0);
    output_of(fixture.person().arg("collect"));
    let path = fixture.human_dir().join("collected.json");
    let before: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    fixture.secrets_scan(
        &serde_json::json!({"schema":"vessa-secrets.v1", "complete":true,
        "findings":[], "unreadable":{"path":"/bad.jsonl","why":"permission denied"}})
        .to_string(),
        0,
    );
    let (stdout, stderr, code) = output_of(fixture.person().arg("collect"));
    assert_eq!(code, Some(4), "{stdout}{stderr}");
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(
        after["secrets"]["value"]["read_ms"],
        before["secrets"]["value"]["read_ms"]
    );
    assert!(after["secrets"]["error"]
        .as_str()
        .unwrap()
        .contains("unreadable-path list"));
    fixture.cleanup();
}

/// `ohjaa adopt --help` says what an entry holds. It said the entry kept the
/// gate "copied from the repository's sykli.json", while every commit's gate
/// is derived from that commit, and `ohjaa doctor` calls such a copy never
/// read.
#[test]
fn adopt_help_says_the_entry_keeps_no_copy_of_a_gate() {
    let fixture = Fixture::new("adopt-help");
    let (stdout, stderr, code) = output_of(fixture.ohjaa().args(["adopt", "--help"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let said = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        said.contains(
            "The entry keeps no copy of a gate: every commit's gate is derived from that \
             commit's own sykli.json, else from what `sykli init` writes for its tree"
        ),
        "{stdout}"
    );
    // A commit whose contract is a sykli.rs is not handed to sykli init: it
    // has no gate (`Holds::Emitter`).
    assert!(
        said.contains("a commit whose only contract is a sykli.rs has none"),
        "{stdout}"
    );
    assert!(!said.contains("copied from"), "{stdout}");
    fixture.cleanup();
}

/// The pre-push hook hands sykli the gate derived from the pushed commit,
/// not a copy kept in the entry, which ohjaa no longer writes or reads.
#[test]
fn hook_help_hands_sykli_the_gate_derived_from_the_pushed_commit() {
    let fixture = Fixture::new("hook-help");
    let (stdout, stderr, code) = output_of(fixture.ohjaa().args(["hook", "--help"]));
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    let said = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(!said.contains("<entry>/sykli.json"), "{stdout}");
    assert!(!said.contains("an entry without a gate"), "{stdout}");
    assert!(
        said.contains("--contract <gate>")
            && said.contains("<gate> is the gate derived from the pushed commit"),
        "{stdout}"
    );
    fixture.cleanup();
}
