//! The whole loop, walked once by the real tools, in a world of its own:
//!
//! 1. **Start.** `ohjaa run claude` briefs the agent with the knowledge (the
//!    second time, also with the last session's handover) and launches it
//!    through the real toimija, which gives it the briefing in its launch
//!    prompt. The agent reads back with `ohjaa oriented`.
//! 2. **Work.** It commits in a worktree of its own and runs `ohjaa gate`: the
//!    real sykli runs the gate derived from the commit, the fixture's test
//!    among it, and writes a receipt.
//! 3. **Ship.** It pushes to origin's main. The hook hands the push to sykli,
//!    which accepts it and keeps the receipt that proved it, and the marks
//!    say ✓. Once the worktree is removed and that copy is set aside, they
//!    still say ✓, from the receipt sykli kept of the run.
//! 4. **Learn.** It hands over with `ohjaa handover`, and leaving writes the
//!    record. `ohjaa intake` drafts a proposal from what the person said in
//!    the session, which the real vessa finds in the agent's transcript.
//! 5. **The next Start.** A person accepts the proposal, and the next
//!    briefing carries it, with the handover.
//!
//! The second session then pushes a commit that breaks the fixture's test,
//! as observe-mark-flag lets it: it is marked ✗, and the briefing after it
//! opens with the flag.
//!
//! Ignored by default, since CI's runners have none of these tools. Run it
//! with the installed ones. Measured on 2026-10-27, it takes 8 to 16 seconds
//! on macOS and 3 to 8 on Linux:
//!
//! ```sh
//! OHJAA_TEST_SYKLI=$(command -v sykli) OHJAA_TEST_TOIMIJA=$(command -v toimija) \
//!   OHJAA_TEST_VESSA=$(command -v vessa) cargo test --test loop -- --ignored
//! ```
//!
//! Isolated as `tests/config_home.rs` is. Every command runs with an emptied
//! environment: HOME, XDG_CONFIG_HOME, XDG_STATE_HOME, CARGO_HOME, TMPDIR and
//! OHJAA_OURS in the sandbox, GIT_CONFIG_GLOBAL at a sandbox file,
//! GIT_CONFIG_NOSYSTEM=1, and a PATH of the sandbox's `bin` (this ohjaa, the
//! three tools named, git, and stand-ins), the toolchain that builds this
//! test (the fixture's cargo, with no rustup between, which would look for
//! its toolchains in the sandbox's HOME) and the system's directories. git
//! is linked into `bin`, so a folder it shares with other tools (Homebrew's)
//! is never on PATH. The
//! real claude never runs (a stand-in agent is first on PATH), nor the real
//! gh (a stand-in that is not signed in), nor a real drafter. There is no
//! network: origin is a bare repository in the sandbox, and the fixture crate
//! has no dependencies (`cargo test --offline`).
//!
//! Checked by hand on 2026-10-27, around two runs: the modification times of
//! every file in the real `~/.config/ohjaa`, of `~/.local/state/ohjaa` and
//! `~/.local/state/toimija` two levels down, and of `~/.gitconfig`,
//! `~/.zshrc`, `~/.config/git/ignore` and `~/.cargo/config.toml`. Around the
//! second run none moved. Around the first, the session log and toimija's
//! `tmp` moved, as the person's live sessions write them; none of those files
//! names the sandbox, nor does any folder of `~/.claude/projects`. The test
//! does not check this itself: those live sessions would make it flaky.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// A stand-in for claude. toimija hands it its launch prompt as its last
/// argument, as it hands the real one; it keeps it in `$LOOP_OUT/briefing`
/// and, with `LOOP_IDLE` set, does nothing more. Otherwise it works as an
/// agent briefed by ohjaa does. It keeps its transcript in the sandbox's
/// HOME, as Claude Code does, each record timed to the fraction of a second
/// (leaving compares them with the launch's own instant): its launch prompt
/// first, then what the person said (`LOOP_SAID`), each followed by its
/// reply. From a shell whose
/// `CLAUDE_CODE_SESSION_ID` is its own session (`LOOP_NATIVE`), as Claude
/// Code's tool shells have it, it reads back, makes a worktree of origin's
/// main (`LOOP_WORKTREE`, on branch `LOOP_BRANCH`), commits `LOOP_LIB` as
/// `src/lib.rs` there, runs `ohjaa gate`, pushes to main whatever the gate
/// said, and hands over (`LOOP_UNFINISHED`, `LOOP_NEXT`). What each step
/// said is kept in `$LOOP_OUT`; a step that fails ends it with its own code.
const AGENT: &str = r#"#!/bin/sh
for prompt; do :; done
mkdir -p "$LOOP_OUT"
printf '%s' "$prompt" > "$LOOP_OUT/briefing"
[ -n "${LOOP_IDLE:-}" ] && exit 0
cwd=$(pwd -P)
dir="$HOME/.claude/projects/$(printf '%s' "$cwd" | sed 's/[^A-Za-z0-9]/-/g')"
mkdir -p "$dir"
json() { awk 'BEGIN { ORS = "" } { gsub(/\\/, "\\\\"); gsub(/"/, "\\\""); gsub(/\t/, "\\t"); if (NR > 1) print "\\n"; print }'; }
at() { date -u +%Y-%m-%dT%H:%M:%S.%NZ; }
user() {
  printf '{"type":"user","uuid":"u-%s-%s","sessionId":"%s","cwd":"%s","timestamp":"%s","isSidechain":false,"message":{"role":"user","content":"%s"}}\n' \
    "$LOOP_NATIVE" "$1" "$LOOP_NATIVE" "$cwd" "$(at)" "$2"
}
reply() {
  printf '{"type":"assistant","uuid":"a-%s-%s","requestId":"req_011Cloop-%s-%s","sessionId":"%s","cwd":"%s","timestamp":"%s","isSidechain":false,"message":{"id":"msg_011Cloop-%s-%s","role":"assistant","model":"loop-model","content":[{"type":"text","text":"%s"}],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}\n' \
    "$LOOP_NATIVE" "$1" "$LOOP_NATIVE" "$1" "$LOOP_NATIVE" "$cwd" "$(at)" "$LOOP_NATIVE" "$1" "$2"
}
transcript="$dir/$LOOP_NATIVE.jsonl"
user 1 "$(printf '%s' "$prompt" | json)" >> "$transcript"
reply 1 "I will work in a worktree of my own." >> "$transcript"
if [ -n "${LOOP_SAID:-}" ]; then
  user 2 "$(printf '%s' "$LOOP_SAID" | json)" >> "$transcript"
  reply 2 "Understood." >> "$transcript"
fi
export CLAUDE_CODE_SESSION_ID="$LOOP_NATIVE"
ohjaa oriented "goal: $LOOP_GOAL; rules: the tests run offline; nobody else works here" \
  > "$LOOP_OUT/oriented" 2>&1 || exit 10
git worktree add -q -b "$LOOP_BRANCH" "$LOOP_WORKTREE" origin/main > "$LOOP_OUT/worktree" 2>&1 || exit 11
cd "$LOOP_WORKTREE" || exit 12
cp "$LOOP_LIB" src/lib.rs || exit 13
git commit -q -am "$LOOP_GOAL" > "$LOOP_OUT/commit" 2>&1 || exit 14
ohjaa gate > "$LOOP_OUT/gate" 2>&1
echo $? > "$LOOP_OUT/gate-exit"
git push origin HEAD:main > "$LOOP_OUT/push" 2>&1 || exit 15
ohjaa handover --unfinished "$LOOP_UNFINISHED" --next "$LOOP_NEXT" --surprised "nothing" \
  > "$LOOP_OUT/handover" 2>&1 || exit 16
"#;

/// A stand-in drafter, never a real one: it keeps every prompt in
/// `$SANDBOX/drafted`, and drafts `$SANDBOX/draft.json` from a batch that
/// holds the person's correction (`$SANDBOX/correction`), nothing from any
/// other.
const DRAFTER: &str = r#"#!/bin/sh
prompt=$(cat)
printf '%s\n' "$prompt" >> "$SANDBOX/drafted"
case "$prompt" in
  *"$(cat "$SANDBOX/correction")"*) cat "$SANDBOX/draft.json";;
  *) echo '[]';;
esac
"#;

/// Never the real gh: no network. It is not signed in, as on a machine that
/// never ran `gh auth login`.
const GH: &str = r#"#!/bin/sh
echo "gh: To get started with GitHub CLI, please run:  gh auth login" >&2
exit 4
"#;

/// The fixture's own gate: its one test, offline.
const CONTRACT: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"cargo test --offline --quiet","inputs":["Cargo.toml","Cargo.lock","src/lib.rs"]}]}"#;

const CARGO_TOML: &str = "[package]\nname = \"widget\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// What the fixture says it is, as every repository of the platform does;
/// toimija reads it, and the briefing's perimeter is complete.
const REPO_YAML: &str =
    "repo:\n  name: widget\n  role: the loop test's fixture crate\n  not:\n    - a product\n";

/// What toimija writes beside `repo.yaml`, as ohjaa's own `.gitignore` has it.
const GITIGNORE: &str = "target/\n.toimija/current.md\n.toimija/current.json\n\
                         .toimija/sessions/\n.toimija/history.jsonl\n.toimija/history.lock\n\
                         .toimija/runs/\n.toimija/tmp/\n";

const LIB: &str = "pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(2, 2), 4);
    }
}
";

/// The first session's work: `triple`, and its passing test.
const LIB_TRIPLE: &str = "pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn triple(a: u32) -> u32 {
    a * 3
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(2, 2), 4);
    }

    #[test]
    fn triples() {
        assert_eq!(super::triple(2), 6);
    }
}
";

/// The second session's: the doc comment the person asked for, and a
/// `triple` its test fails.
const LIB_BROKEN: &str = "pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

/// Three times `a`.
pub fn triple(a: u32) -> u32 {
    a * 4
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(2, 2), 4);
    }

    #[test]
    fn triples() {
        assert_eq!(super::triple(2), 6);
    }
}
";

/// Knowledge current before the first launch.
const KNOWLEDGE: &str = "The widget's tests run offline, with cargo test --offline. \
                         Never add a dependency that needs the network.";

/// What the person said in the first session: a correction.
const CORRECTION: &str = "I told you, every public function gets a doc comment, triple() included.";

/// What the stand-in drafter drafts from it, and the person accepts.
const PROPOSAL: &str = "document-public-functions";
const LESSON: &str = "Give every public function a doc comment.";

const UNFINISHED: &str = "triple() has no doc comment yet";
const NEXT: &str = "the widget's gate is cargo test; prove it with ohjaa gate";
const NEXT_BROKEN: &str = "fix triple(): its test fails";

const SENTINEL: &str = "<!-- ohjaa-briefing v1 -->\n";

/// What a command said, and how it ended.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Ran {
    fn said(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// The sandbox: the person's home, the repositories, origin and the tools.
struct World {
    dir: PathBuf,
    path: String,
}

impl World {
    fn new() -> World {
        let tools = [
            ("sykli", "OHJAA_TEST_SYKLI"),
            ("toimija", "OHJAA_TEST_TOIMIJA"),
            ("vessa", "OHJAA_TEST_VESSA"),
        ]
        .map(|(name, var)| (name, tool(var)));
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-loop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        for sub in [
            "bin",
            "home",
            "config",
            "state",
            "cargo",
            "tmp",
            "projects",
            "remotes/acme",
            "worktrees",
            "ours/knowledge",
            "launches",
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let bin = dir.join("bin");
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ohjaa"), bin.join("ohjaa")).unwrap();
        for (name, path) in tools {
            std::os::unix::fs::symlink(path, bin.join(name)).unwrap();
        }
        write_executable(&bin.join("claude"), AGENT);
        write_executable(&bin.join("gh"), GH);
        write_executable(&dir.join("drafter"), DRAFTER);
        // git alone, never the folder it is in: Homebrew's holds the real gh.
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("git"))
            .find(|git| git.is_file())
            .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
        std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
        let path = format!(
            "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
            bin.display(),
            toolchain().display()
        );
        World { dir, path }
    }

    fn clone(&self) -> PathBuf {
        self.dir.join("projects/widget")
    }

    fn bare(&self) -> PathBuf {
        self.dir.join("remotes/acme/widget.git")
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("HOME", self.dir.join("home"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CARGO_HOME", self.dir.join("cargo"))
            .env("TMPDIR", self.dir.join("tmp"))
            .env("OHJAA_OURS", self.dir.join("ours"))
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env("GIT_AUTHOR_NAME", "Ohjaa Test")
            .env("GIT_AUTHOR_EMAIL", "ohjaa@example.invalid")
            .env("GIT_COMMITTER_NAME", "Ohjaa Test")
            .env("GIT_COMMITTER_EMAIL", "ohjaa@example.invalid")
            .env("SANDBOX", &self.dir);
        command
    }

    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> Ran {
        ran(self.command(program, cwd).args(args))
    }

    fn ohjaa(&self, cwd: &Path, args: &[&str]) -> Ran {
        self.run(cwd, "ohjaa", args)
    }

    /// `program args` in `cwd`, which must succeed.
    fn ok(&self, cwd: &Path, program: &str, args: &[&str]) -> Ran {
        let ran = self.run(cwd, program, args);
        assert_eq!(ran.code, Some(0), "{program} {args:?}: {}", ran.said());
        ran
    }

    /// git's answer, trimmed; it must succeed.
    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.ok(cwd, "git", args).stdout.trim().to_owned()
    }

    /// `acme/widget`: a crate whose one test passes, its own gate locked by
    /// the real sykli, committed long ago (a reflog entry of the second a
    /// session starts would count as made in it) and pushed to a bare
    /// origin.
    fn fixture(&self) {
        let (clone, bare) = (self.clone(), self.bare());
        self.git(
            &self.dir,
            &["init", "-q", "--bare", "-b", "main", path(&bare)],
        );
        self.git(&self.dir, &["init", "-q", "-b", "main", path(&clone)]);
        fs::create_dir_all(clone.join("src")).unwrap();
        fs::write(clone.join("Cargo.toml"), CARGO_TOML).unwrap();
        fs::write(clone.join("src/lib.rs"), LIB).unwrap();
        fs::write(clone.join(".gitignore"), GITIGNORE).unwrap();
        fs::create_dir_all(clone.join(".toimija")).unwrap();
        fs::write(clone.join(".toimija/repo.yaml"), REPO_YAML).unwrap();
        fs::write(clone.join("sykli.json"), CONTRACT).unwrap();
        self.ok(&clone, "cargo", &["generate-lockfile", "--offline"]);
        self.ok(&clone, "sykli", &["lock", "sykli.json"]);
        self.git(&clone, &["add", "-A"]);
        let settled = |args: &[&str]| {
            let ran = ran(self
                .command("git", &clone)
                .args(args)
                .env("GIT_COMMITTER_DATE", "@1000000000 +0000"));
            assert_eq!(ran.code, Some(0), "git {args:?}: {}", ran.said());
        };
        settled(&["commit", "-q", "-m", "a widget that adds"]);
        settled(&["remote", "add", "origin", path(&bare)]);
        settled(&["push", "-q", "-u", "origin", "main"]);
        self.git(&clone, &["remote", "set-head", "origin", "main"]);
    }

    /// `ohjaa run claude` in the clone, its agent given `agent`; what the
    /// launch said, and the folder where the agent kept what it saw and said.
    fn launch(&self, n: usize, intent: &str, agent: &[(&str, &str)]) -> (Ran, PathBuf) {
        let out = self.dir.join(format!("launches/{n}"));
        let mut command = self.command("ohjaa", &self.clone());
        command
            .args(["run", "claude", "--intent", intent, "--task", intent])
            .env("LOOP_OUT", &out);
        for (key, value) in agent {
            command.env(key, value);
        }
        (ran(&mut command), out)
    }

    /// What the agent of launch `out` kept in `name`.
    fn kept(&self, out: &Path, name: &str) -> String {
        fs::read_to_string(out.join(name))
            .unwrap_or_else(|error| panic!("{}: {error}\n{}", name, self.trace(out)))
    }

    /// Everything the agent of a launch kept, for a failure's message.
    fn trace(&self, out: &Path) -> String {
        let mut names: Vec<PathBuf> = fs::read_dir(out)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default();
        names.sort();
        names
            .iter()
            .filter(|path| !path.ends_with("briefing"))
            .map(|path| {
                format!(
                    "--- {}\n{}",
                    path.display(),
                    fs::read_to_string(path).unwrap_or_default()
                )
            })
            .collect()
    }

    /// The entry's marks, as `ohjaa report --json --marks` derives them now.
    fn marks(&self) -> Value {
        let ran = self.ohjaa(&self.clone(), &["report", "--json", "--marks"]);
        assert_eq!(ran.code, Some(0), "{}", ran.said());
        serde_json::from_str(&ran.stdout).unwrap()
    }

    /// The session's record, as `ohjaa usage export` gives it.
    fn record(&self, session: &str) -> Value {
        let ran = self.ohjaa(&self.clone(), &["usage", "export", "--session", session]);
        assert_eq!(ran.code, Some(0), "{}", ran.said());
        let export: Value = serde_json::from_str(&ran.stdout).unwrap();
        export["sessions"][0].clone()
    }

    /// `ohjaa <args>` under a pseudo-terminal, as a person runs it at
    /// theirs: `tests/cli.rs`'s `under_a_terminal`, from the sandbox's
    /// emptied environment. What it printed, and how it ended: `script`
    /// exits with the command's status (`-e` on Linux).
    fn under_a_terminal(&self, cwd: &Path, args: &[&str]) -> Ran {
        let quoted = std::iter::once("ohjaa")
            .chain(args.iter().copied())
            .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut script = self.command("script", cwd);
        if cfg!(target_os = "macos") {
            script.args(["-q", "/dev/null", "sh", "-c", &quoted]);
        } else {
            script.args(["-qec", &quoted, "/dev/null"]);
        }
        let output = script
            .output()
            .expect("`script` makes the terminal a person has");
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).replace('\r', ""),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // A failure keeps the world to look at, and says where.
        if std::thread::panicking() {
            eprintln!("the world is kept in {}", self.dir.display());
        } else {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn tool(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "{var} names no file; this test needs the real sykli, toimija and vessa: \
                 OHJAA_TEST_SYKLI=<path> OHJAA_TEST_TOIMIJA=<path> OHJAA_TEST_VESSA=<path> \
                 cargo test --test loop -- --ignored"
            )
        })
}

/// The folder of the cargo that builds this test, with its rustc beside it.
fn toolchain() -> PathBuf {
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO")));
    let dir = cargo.parent().unwrap().to_path_buf();
    assert!(
        dir.join("rustc").is_file(),
        "no rustc beside {}",
        cargo.display()
    );
    dir
}

fn ran(command: &mut Command) -> Ran {
    let output = command.output().unwrap();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Write an executable that no process holds open for writing when it runs.
/// Linux refuses to exec a file any process has open for writing (ETXTBSY),
/// and a child another test thread forks holds a copy of every descriptor
/// this process has open until that child execs: a file written here, then
/// closed, even renamed, can still be open for writing in such a child at
/// the moment it is run. So this process writes only the text, beside it,
/// and `cp`, a process of its own that forks nothing, writes the executable.
fn write_executable(path: &Path, text: &str) {
    let name = path.file_name().unwrap().to_string_lossy();
    let source = path.with_file_name(format!(".{name}.text"));
    fs::write(&source, text).unwrap();
    let copied = Command::new("cp").arg(&source).arg(path).status().unwrap();
    assert!(copied.success(), "cp {}", path.display());
    fs::remove_file(&source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn short(commit: &str) -> &str {
    &commit[..7]
}

/// The ohjaa session a briefing names: `This session is <id>;`.
fn session_named(briefing: &str) -> String {
    let (_, rest) = briefing
        .split_once("This session is ")
        .unwrap_or_else(|| panic!("no session named: {briefing}"));
    rest.split(';').next().unwrap().to_owned()
}

/// Real Toimija must start a Codex turn, not only attach idle instructions.
#[test]
#[ignore = "needs real tools: OHJAA_TEST_SYKLI, OHJAA_TEST_TOIMIJA and OHJAA_TEST_VESSA"]
fn concrete_intent_starts_a_turn_through_real_toimija() {
    let world = World::new();
    let source = world.dir.join("argv.rs");
    fs::write(
        &source,
        r#"
        fn main() {
            let args = std::env::args().skip(1).collect::<Vec<_>>().join("\0");
            std::fs::write(std::env::var_os("CAPTURE").unwrap(), args).unwrap();
        }
    "#,
    )
    .unwrap();
    world.ok(
        &world.dir,
        "rustc",
        &[path(&source), "-o", path(&world.dir.join("bin/codex"))],
    );
    let capture = world.dir.join("argv");
    let launch = |args: &[&str]| {
        let result = ran(world
            .command("ohjaa", &world.dir)
            .args(args)
            .env("CAPTURE", &capture));
        assert_eq!(result.code, Some(0), "{}", result.said());
        let args = fs::read_to_string(&capture).unwrap();
        let mut args = args.split('\0');
        let mut prompts = Vec::new();
        while let Some(arg) = args.next() {
            if arg == "-c" {
                args.next().expect("configuration value");
            } else {
                prompts.push(arg.to_owned());
            }
        }
        prompts
    };
    let goal = "reduce Git pressure";
    let prompts = launch(&["run", "codex", "--intent", goal]);
    assert_eq!(prompts.len(), 1, "a concrete intent starts a turn");
    assert!(
        prompts[0].contains("Task: reduce Git pressure"),
        "{}",
        prompts[0]
    );
    let exported = world.ok(&world.dir, "ohjaa", &["usage", "export"]);
    let exported: Value = serde_json::from_str(&exported.stdout).unwrap();
    assert!(exported["sessions"][0].get("task").is_none());
    let explicit = launch(&[
        "run",
        "codex",
        "--intent",
        goal,
        "--task",
        "start with the lexer",
    ]);
    assert_eq!(explicit.len(), 1);
    assert!(explicit[0].contains("Task: start with the lexer"));
    for placeholder in ["", "unknown", "interactive"] {
        assert!(
            launch(&["run", "codex", "--intent", placeholder]).is_empty(),
            "{placeholder:?} stays idle"
        );
    }
}

/// The mark of `sha`, from an entry's marks.
fn mark<'a>(entry: &'a Value, sha: &str) -> &'a Value {
    entry["commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|commit| commit["sha"] == sha)
        .unwrap_or_else(|| panic!("{sha} is not marked: {entry:#}"))
}

#[test]
#[ignore = "needs the real sykli, toimija and vessa: OHJAA_TEST_SYKLI=<path> OHJAA_TEST_TOIMIJA=<path> OHJAA_TEST_VESSA=<path> cargo test --test loop -- --ignored"]
fn the_whole_loop_on_the_real_tools() {
    let world = World::new();
    let (clone, bare) = (world.clone(), world.bare());
    world.fixture();
    world.ok(
        &world.dir,
        "ohjaa",
        &["apply", "--root", path(&world.dir.join("projects"))],
    );
    world.ok(&world.dir, "ohjaa", &["adopt", path(&clone)]);
    // A person proves main as it is, so the loop starts from a proven main.
    let gate = world.ohjaa(&clone, &["gate"]);
    assert_eq!(gate.code, Some(0), "{}", gate.said());
    let start = world.git(&clone, &["rev-parse", "HEAD"]);
    assert_eq!(mark(&world.marks()["entries"][0], &start)["mark"], "proven");
    fs::write(
        world.dir.join("ours/knowledge/widget-tests-run-offline.md"),
        format!(
            "+++\nid = \"widget-tests-run-offline\"\nkind = \"decision\"\nstatus = \"current\"\n\
             as_of = 2026-10-20\norigin = [\"2026-10-20: 'the tests never reach the network'\"]\n\
             +++\n{KNOWLEDGE}\n"
        ),
    )
    .unwrap();

    // ---- 1. Start ----------------------------------------------------------
    let native = "5e1976e4-71c3-407c-9533-0be05b7e0001";
    let lib = world.dir.join("lib-triple.rs");
    fs::write(&lib, LIB_TRIPLE).unwrap();
    let worktree = world.dir.join("worktrees/widget-triple");
    let (first, out) = world.launch(
        1,
        "add triple()",
        &[
            ("LOOP_NATIVE", native),
            ("LOOP_SAID", CORRECTION),
            ("LOOP_GOAL", "add triple()"),
            ("LOOP_LIB", path(&lib)),
            ("LOOP_BRANCH", "add-triple"),
            ("LOOP_WORKTREE", path(&worktree)),
            ("LOOP_UNFINISHED", UNFINISHED),
            ("LOOP_NEXT", NEXT),
        ],
    );
    assert_eq!(
        first.code,
        Some(0),
        "{}\n{}",
        first.said(),
        world.trace(&out)
    );
    let briefing = world.kept(&out, "briefing");
    let session = session_named(&briefing);
    let (_, ohjaas) = briefing
        .split_once(SENTINEL)
        .unwrap_or_else(|| panic!("toimija passed no briefing of ohjaa's: {briefing}"));
    assert!(ohjaas.contains(KNOWLEDGE), "{ohjaas}");
    assert!(
        !ohjaas.contains("FLAG:") && !ohjaas.contains("Last session here"),
        "main is proven and nobody worked here before: {ohjaas}"
    );
    assert!(
        world
            .kept(&out, "oriented")
            .contains(&format!("recorded the read-back for session {session}")),
        "{}",
        world.trace(&out)
    );

    // ---- 2. Work: ohjaa gate, and a receipt sykli verifies -----------------
    assert_eq!(
        world.kept(&out, "gate-exit").trim(),
        "0",
        "{}",
        world.trace(&out)
    );
    assert!(
        world.kept(&out, "gate").contains(
            "ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven"
        ),
        "{}",
        world.trace(&out)
    );
    let commit = world.git(&worktree, &["rev-parse", "HEAD"]);
    let record = world.record(&session);
    let validation = record["validations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["commit"] == commit.as_str())
        .unwrap_or_else(|| panic!("the gate's request is not recorded: {record:#}"))
        .clone();
    assert_eq!(validation["kind"], "candidate", "{validation:#}");
    let contract = validation["contract"].as_str().unwrap();
    let receipt = validation["receipt"]["path"].as_str().unwrap();
    let verified = world.run(
        &worktree,
        "sykli",
        &[
            "verify",
            receipt,
            "--contract",
            contract,
            "--commit",
            &commit,
        ],
    );
    assert_eq!(verified.code, Some(0), "{}", verified.said());
    // A receipt verifies against the contract it ran, so it must also say the
    // fixture's test ran: a gate with no task would verify as well.
    let ran: Value = serde_json::from_str(&fs::read_to_string(receipt).unwrap()).unwrap();
    let tasks: Vec<(&str, &str)> = ran["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("a receipt without tasks: {ran:#}"))
        .iter()
        .map(|task| {
            (
                task["name"].as_str().unwrap_or_default(),
                task["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(tasks, [("test", "passed")], "{ran:#}");

    // ---- 3. Ship: sykli's hook accepts the push; ✓, and ✓ from the run alone
    let push = world.kept(&out, "push");
    assert!(
        push.contains("HEAD -> main") && !push.contains("ohjaa:"),
        "a proven push is quiet: {push}"
    );
    assert_eq!(world.git(&bare, &["rev-parse", "main"]), commit);
    // sykli's pre-push hook accepted it, and keeps the receipt that proved it.
    let digest = validation["receipt"]["digest"].as_str().unwrap();
    let hooks = clone.join(".git/sykli/receipts");
    assert!(
        hooks.join(format!("rcpt_{digest}.json")).is_file(),
        "sykli's hook kept no receipt of the push: {push}"
    );
    let marks = &world.marks()["entries"][0];
    assert_eq!(marks["tip"], commit.as_str(), "{marks:#}");
    assert_eq!(mark(marks, &commit)["mark"], "proven", "{marks:#}");
    assert_eq!(mark(marks, &commit)["tasks"], 1, "{marks:#}");
    assert_eq!(marks["proven_at_tip"], true, "{marks:#}");
    world.git(&clone, &["worktree", "remove", path(&worktree)]);
    assert!(!worktree.exists());
    // With the worktree gone and the hook's copy set aside, what proves the
    // commit is the receipt sykli kept beside its run's journal, in the clone.
    fs::rename(&hooks, world.dir.join("hook-receipts-set-aside")).unwrap();
    let marks = &world.marks()["entries"][0];
    assert_eq!(
        mark(marks, &commit)["mark"],
        "proven",
        "the proof outlives the worktree: {marks:#}"
    );
    let kept = clone.join(format!(
        ".git/sykli/runs/{}/rcpt_{digest}.json",
        validation["run"].as_str().unwrap()
    ));
    let verified = world.run(
        &clone,
        "sykli",
        &[
            "verify",
            path(&kept),
            "--contract",
            contract,
            "--commit",
            &commit,
        ],
    );
    assert_eq!(verified.code, Some(0), "{}", verified.said());

    // ---- 4. Learn: the record, then intake ---------------------------------
    assert!(
        first
            .stderr
            .contains("ohjaa: recorded; queued for `ohjaa intake`"),
        "{}",
        first.stderr
    );
    assert_eq!(
        record["readback"]["text"],
        "goal: add triple(); rules: the tests run offline; nobody else works here",
        "{record:#}"
    );
    assert_eq!(record["handover"]["unfinished"], UNFINISHED, "{record:#}");
    assert_eq!(record["handover"]["next"], NEXT, "{record:#}");
    let left = &record["left"];
    assert_eq!(left["exit"]["code"], 0, "{record:#}");
    let phases = left["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 1, "{record:#}");
    assert_eq!(phases[0]["commit"], commit.as_str());
    assert_eq!(phases[0]["evidence"]["outcome"], "passed", "{record:#}");
    assert_eq!(left["landed"][0]["commit"], commit.as_str(), "{record:#}");
    assert_eq!(
        left["landed"][0]["check"]["verdict"], "passed",
        "{record:#}"
    );
    // The agent's own session: found by its launch prompt in its transcript,
    // and claimed by its environment.
    let found = left["native"]["sessions"].as_array().unwrap();
    assert_eq!(found.len(), 1, "{record:#}");
    assert_eq!(found[0]["agent"], "claude");
    assert_eq!(found[0]["session_id"], native);
    assert_eq!(found[0]["env"], true, "{record:#}");
    assert!(
        found[0]["source"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/{native}.jsonl")),
        "{record:#}"
    );
    assert_eq!(record["native_env"][0]["session_id"], native, "{record:#}");

    fs::write(world.dir.join("correction"), CORRECTION).unwrap();
    fs::write(
        world.dir.join("draft.json"),
        serde_json::json!([{
            "scope": "mine", "repo": null, "id": PROPOSAL, "kind": "lesson", "body": LESSON,
            "origin": [format!("the loop test: '{CORRECTION}'")],
            "refines": null, "supersedes": [],
        }])
        .to_string(),
    )
    .unwrap();
    let intake = world.ohjaa(
        &clone,
        &["intake", "--drafter", path(&world.dir.join("drafter"))],
    );
    assert_eq!(intake.code, Some(0), "{}", intake.said());
    assert!(
        intake
            .stderr
            .contains("the pre-filter (phrase-rules.v3) kept 1 (correction 1)"),
        "{}",
        intake.said()
    );
    assert!(
        intake.said().contains(&format!("proposed {PROPOSAL}\n")),
        "{}",
        intake.said()
    );
    // What the drafter was sent: the person's words, as vessa read them from
    // the transcript, and the session's handover.
    let drafted = fs::read_to_string(world.dir.join("drafted")).unwrap();
    assert!(
        drafted.contains(&format!("(claude session {native})"))
            && drafted.contains(&format!("    said: {CORRECTION}\n"))
            && drafted.contains(&format!("    said: unfinished: {UNFINISHED}\n")),
        "{drafted}"
    );
    assert_eq!(world.record(&session)["drafted"], true);
    let waiting = world
        .dir
        .join(format!("state/ohjaa/proposals/mine/{PROPOSAL}.md"));
    assert!(
        fs::read_to_string(&waiting)
            .unwrap()
            .contains("status = \"proposed\""),
        "the proposal waits outside every repository"
    );

    // ---- 5. The next Start: a person accepts it ----------------------------
    let listed = world.ohjaa(&clone, &["review", "--list"]);
    assert!(
        listed.code == Some(0)
            && listed.stdout.contains(PROPOSAL)
            && listed.stdout.contains(LESSON),
        "{}",
        listed.said()
    );
    let proposals = &world.marks()["proposals"];
    assert!(
        proposals["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == PROPOSAL),
        "{proposals:#}"
    );
    // Without a terminal, as an agent runs it, accepting is refused.
    let refused = world.ohjaa(&clone, &["accept", PROPOSAL]);
    assert_eq!(refused.code, Some(3), "{}", refused.said());
    assert!(
        refused.stderr.contains("only a person may run it"),
        "{}",
        refused.stderr
    );
    assert!(waiting.is_file(), "a refused decision changes nothing");
    // At a terminal, as the person runs it. Here the test is that person,
    // deciding a proposal of the sandbox's own: an agent never makes a
    // terminal to accept on a person's behalf, outside a world of its own
    // (AGENTS.md).
    let accepted = world.under_a_terminal(&clone, &["accept", PROPOSAL]);
    assert!(
        accepted.code == Some(0) && accepted.stdout.contains(&format!("accepted {PROPOSAL}")),
        "{}",
        accepted.said()
    );
    assert!(!waiting.exists(), "{}", accepted.said());
    assert!(
        fs::read_to_string(world.dir.join(format!("config/ohjaa/mine/{PROPOSAL}.md")))
            .unwrap()
            .contains("status = \"current\"")
    );

    let native_next = "5e1976e4-71c3-407c-9533-0be05b7e0002";
    let broken = world.dir.join("lib-broken.rs");
    fs::write(&broken, LIB_BROKEN).unwrap();
    let worktree = world.dir.join("worktrees/widget-doc");
    let (second, out) = world.launch(
        2,
        "document triple()",
        &[
            ("LOOP_NATIVE", native_next),
            ("LOOP_GOAL", "document triple()"),
            ("LOOP_LIB", path(&broken)),
            ("LOOP_BRANCH", "document-triple"),
            ("LOOP_WORKTREE", path(&worktree)),
            ("LOOP_UNFINISHED", "triple() is broken"),
            ("LOOP_NEXT", NEXT_BROKEN),
        ],
    );
    assert_eq!(
        second.code,
        Some(0),
        "{}\n{}",
        second.said(),
        world.trace(&out)
    );
    let briefing = world.kept(&out, "briefing");
    let next_session = session_named(&briefing);
    let (_, ohjaas) = briefing.split_once(SENTINEL).unwrap();
    assert!(
        ohjaas.contains(&format!("- [lesson] {LESSON}\n  ({PROPOSAL} · as of ")),
        "the accepted knowledge: {ohjaas}"
    );
    assert!(
        ohjaas.contains(&format!("Last session here ({session}, claude, ended "))
            && ohjaas.contains(&format!("- unfinished: {UNFINISHED}\n"))
            && ohjaas.contains(&format!("- for the next agent: {NEXT}\n")),
        "the first session's handover: {ohjaas}"
    );
    assert!(!ohjaas.contains("FLAG:"), "{ohjaas}");

    // ---- The failing variant: pushed anyway, ✗, and flagged first ----------
    assert_eq!(
        world.kept(&out, "gate-exit").trim(),
        "1",
        "{}",
        world.trace(&out)
    );
    let push = world.kept(&out, "push");
    assert!(
        push.contains("ohjaa: UNPROVEN push to main ("),
        "pushed, and flagged: {push}"
    );
    let broke = world.git(&bare, &["rev-parse", "main"]);
    assert_ne!(broke, commit);
    assert!(
        second.stderr.contains(&format!(
            "ohjaa: flagged: origin/main at {} in widget has no passing evidence",
            short(&broke)
        )),
        "{}",
        second.stderr
    );
    let marks = &world.marks()["entries"][0];
    assert_eq!(marks["tip"], broke.as_str(), "{marks:#}");
    assert_eq!(mark(marks, &broke)["mark"], "failed", "{marks:#}");
    assert_eq!(mark(marks, &commit)["mark"], "proven", "{marks:#}");
    assert_eq!(marks["proven_at_tip"], false, "{marks:#}");

    let (third, out) = world.launch(3, "fix triple()", &[("LOOP_IDLE", "1")]);
    assert_eq!(third.code, Some(0), "{}", third.said());
    let flag = format!(
        "FLAG: main of acme/widget is unproven since {} (1 commit: ✗ {};",
        short(&broke),
        short(&broke)
    );
    let briefing = world.kept(&out, "briefing");
    let (_, ohjaas) = briefing.split_once(SENTINEL).unwrap();
    assert!(ohjaas.starts_with(&flag), "the flag first: {ohjaas}");
    assert!(
        ohjaas.contains(&format!(
            "WARNING from the last session here ({next_session}, claude"
        )) && ohjaas.contains(&format!("- for the next agent: {NEXT_BROKEN}\n")),
        "{ohjaas}"
    );
    assert!(
        third.stderr.contains(&format!("ohjaa: {flag}")),
        "{}",
        third.stderr
    );
    let brief = world.ohjaa(&clone, &["brief"]);
    assert!(
        brief.code == Some(0) && brief.stdout.starts_with(&flag),
        "{}",
        brief.said()
    );
}
