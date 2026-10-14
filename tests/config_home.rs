//! The workflow home from outside the repositories: `apply`, `doctor`,
//! `adopt`, `hook`, `gate` and `shell-init`, run as the real binary against
//! real git.
//!
//! Every command runs with an emptied environment: HOME, XDG_CONFIG_HOME,
//! XDG_STATE_HOME, CARGO_HOME and TMPDIR inside the sandbox,
//! GIT_CONFIG_GLOBAL at a sandbox file, GIT_CONFIG_NOSYSTEM=1, no ZDOTDIR, and
//! a PATH of stubs (sykli, toimija, claude, codex) plus the system directories.
//! `apply` writes git's global config and ~/.zshrc, so nothing here may reach
//! the person's real ones, nor run a real agent or tool.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Never the real sykli. `hook pre-push --help` names `--contract` and
/// `--branch` with STUB_SYKLI_CONTRACT=1, only `--contract` with `nobranch`,
/// neither without it; with `fast`, every `--help` also names `--repo-root`
/// and `--git-common-dir`. The pre-push hook keeps its stdin and, like the
/// real one, judges exactly the branch it is given: a push to it is refused
/// in sykli's words unless the repository has `.sykli/receipt.json`, and an
/// accepted one says so on stderr. `run` passes unless `$SANDBOX/fail-gate`
/// exists, and its receipt names the tree in `$SANDBOX/tree`; `verify` passes
/// a receipt that says passed (or cached, as sykli does), and refuses one made under `other-contract`
/// as a contract mismatch (exit 4). Its `verify --help` names `--commit` and
/// no sentence ohjaa could lean on; with `$SANDBOX/verify-json` it also names
/// `--json`, and answers it with a `sykli-verify.v1` verdict (in the schema,
/// and with the tiers established and not, that `$SANDBOX/verify-schema`,
/// `$SANDBOX/verify-established` and `$SANDBOX/verify-not-established` hold,
/// when they exist; with `$SANDBOX/verify-claimed-outcome`, as a newer sykli,
/// its `claimed_outcome` field says the receipt's claim, and the outcome
/// check's words do not). `run` writes its receipt as sykli does,
/// also as `.sykli/receipts/rcpt_<sha256>.json`: one task, passed (with
/// `$SANDBOX/with-cached` a second one reused from the cache; with
/// `$SANDBOX/all-cached` both reused, the run `cached`). With STUB_SYKLI_D3 it is the
/// durable-run sykli of KATANA D3, exactly as DECISIONS.md writes it: `run
/// --json` names its `run`, `run --detach --json` answers `{schema:
/// "sykli-run.v1", run}` once the run is recorded, and `status --run <id>
/// --json` answers `sykli-run-status.v1` (a run is held `running` while
/// `$SANDBOX/hold-runs` exists). With STUB_SYKLI_CAPTURE, `run` first saves
/// the session's record as it stands (`ohjaa usage export`) in
/// `$SANDBOX/at-dispatch.json` (and, dispatched by ohjaa itself outside a
/// session, the whole log in `$SANDBOX/at-dispatch-all.json`). With
/// STUB_SYKLI_STATUS_COMMIT it also has `status --commit <rev> --contract
/// <c> --json` (`sykli-commit-status.v1`): `$SANDBOX/commit-status/<rev>`
/// when a test wrote one, else the verdict in `$SANDBOX/commit-verdict`
/// (default `none`), each ask noted in `$SANDBOX/commit-asks`. A detached
/// run, with `$SANDBOX/record-checkout`, first records the checkout as it
/// found it (`git status` with untracked and ignored files, `.sykli/` aside,
/// and HEAD) in `$SANDBOX/runs/<run>/status` and `head`; with
/// `$SANDBOX/cargo-run`, runs `cargo run -q` there (`runs/<run>/out`); with
/// `$SANDBOX/dirty-the-checkout`, leaves leftovers (an untracked file, a
/// changed tracked one, a build folder); with `$SANDBOX/sub-work`, builds in
/// the submodule `sub` (and `$SANDBOX/sub-dirty` changes a file in it), and
/// with `$SANDBOX/nested-sykli` leaves a `.sykli` folder below the top. `status
/// --run` cannot say with `$SANDBOX/run-status-fails`, and says
/// `$SANDBOX/run-status-json` when a test wrote one. With
/// `$SANDBOX/worktree-cache` it is a sykli of KATANA D10: its `run --help`
/// names `${worktree_cache}` (an older one does not). `lock <contract>` pins
/// the contract's sha256 in the `sykli.lock` beside it, as
/// `{"schema":"sykli-lock.v2","contracts":{"sykli.json":"<sha256>"}}`, and
/// fails (exit 2, nothing written) with `$SANDBOX/lock-fails`. Without those
/// it runs no git, so a trace2 count is ohjaa's own.
const SYKLI_STUB: &str = r#"#!/bin/sh
printf '%s|%s\n' "$PWD" "$*" >> "$SANDBOX/sykli-calls"
# Who asks about runs: the caller a test names, so a straggler is never counted.
[ "$1" = status ] && printf '%s|%s\n' "${SYKLI_CALLER:-}" "$*" >> "$SANDBOX/status-calls"
facts() {
  if [ "${STUB_SYKLI_CONTRACT:-}" = fast ]; then
    echo "      --repo-root <DIR>        the repository's work tree"
    echo "      --git-common-dir <DIR>   its common git directory"
  fi
}
digest() { (sha256sum 2>/dev/null || shasum -a 256) < "$1" | cut -c1-64; }
# The receipt of a run: printed, and kept as sykli keeps it.
receipt() {
  tree=$(cat "$SANDBOX/tree" 2>/dev/null)
  # With $SANDBOX/dirty-tree, a run of uncommitted changes: that tree, dirty.
  ran=$tree; dirty=false
  [ -f "$SANDBOX/dirty-tree" ] && { ran=$(cat "$SANDBOX/dirty-tree"); dirty=true; }
  t1=passed; top=$1; more=""
  if [ -f "$SANDBOX/with-cached" ] || [ -f "$SANDBOX/all-cached" ]; then
    more=',{"name":"u","outcome":"cached","source":"cache"}'
  fi
  [ -f "$SANDBOX/all-cached" ] && { t1=cached; top=cached; }
  [ "$1" = failed ] && { t1=failed; top=failed; }
  run_field=""
  [ -n "$2" ] && run_field="\"run\":\"$2\","
  mkdir -p .sykli/receipts
  printf '{"schema":"sykli-receipt.v1",%s"contract_hash":"c0ffee","lock_digest":"10c4","outcome":"%s","started_at_ms":%s,"subject":{"tree_oid":"%s","head_tree_oid":"%s","dirty":%s},"tasks":[{"name":"t","outcome":"%s"}%s]}\n' \
    "$run_field" "$top" "$(date +%s)000" "$ran" "$tree" "$dirty" "$t1" "$more" > .sykli/receipts/.new
  sum=$(digest .sykli/receipts/.new)
  mv .sykli/receipts/.new ".sykli/receipts/rcpt_$sum.json"
  echo "$PWD/.sykli/receipts/rcpt_$sum.json"
}
case "$1" in
  hook)
    if [ "$3" = --help ]; then
      echo "Usage: sykli hook pre-push [OPTIONS] [REMOTE] [URL]"
      case "${STUB_SYKLI_CONTRACT:-}" in
        1|fast) echo "      --contract <CONTRACT>  a contract outside the repository"
           echo "      --branch <BRANCH>      gate exactly this branch";;
        nobranch) echo "      --contract <CONTRACT>  a contract outside the repository";;
      esac
      facts
      exit 0
    fi
    shift 2; branch=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --contract|--repo-root|--git-common-dir) shift;;
        --branch) branch=$2; shift;;
        *) break;;
      esac
      shift
    done
    cat > "$SANDBOX/sykli-stdin"
    [ -n "$branch" ] || { echo "error: no --branch" >&2; exit 2; }
    if grep -q " refs/heads/$branch " "$SANDBOX/sykli-stdin" && [ ! -f .sykli/receipt.json ]; then
      echo "sykli hook pre-push: refusing to push 0123456789ab to refs/heads/$branch: no receipt in this worktree is a run of its tree" >&2
      echo "sykli hook pre-push: to produce a receipt, check out 0123456789ab and run: sykli run" >&2
      exit 1
    fi
    echo "sykli hook pre-push: kept the receipt as evidence" >&2
    exit 0;;
  init)
    if [ "$2" = --help ]; then
      echo "Usage: sykli init [OPTIONS] [PATH]"
      [ -n "${STUB_SYKLI_CONTRACT:-}" ] && echo "      --contract <CONTRACT>  write the contract there"
      exit 0
    fi
    [ "$2" = --contract ] && { printf '{"version":"1","tasks":{}}\n' > "$3"; exit 0; }
    exit 2;;
  lock)
    [ -f "$SANDBOX/lock-fails" ] && { echo "error: cannot hash $2" >&2; exit 2; }
    lock="$(dirname "$2")/sykli.lock"
    printf '{"schema":"sykli-lock.v2","contracts":{"sykli.json":"%s"}}\n' "$(digest "$2")" > "$lock"
    echo "locked: $lock"; exit 0;;
  run)
    if [ "$2" = --help ]; then
      echo "Usage: sykli run [OPTIONS] [CONTRACT]"
      [ -n "${STUB_SYKLI_D3:-}" ] && echo "      --detach  return once the run is recorded"
      facts
      if [ -f "$SANDBOX/worktree-cache" ]; then
        echo 'A declared env value may name ${worktree_cache}: a directory of this worktree.'
        echo '  "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target"'
      fi
      exit 0
    fi
    if [ -n "${STUB_SYKLI_CAPTURE:-}" ] && [ -n "${OHJAA_SESSION:-}" ]; then
      ohjaa usage export --session "$OHJAA_SESSION" > "$SANDBOX/at-dispatch.json" 2>&1
    elif [ -n "${STUB_SYKLI_CAPTURE:-}" ]; then
      # Dispatched by ohjaa itself (an integrated validation): the whole log.
      ohjaa usage export > "$SANDBOX/at-dispatch-all.json" 2>&1
    fi
    detach=""
    for arg in "$@"; do [ "$arg" = --detach ] && detach=1; done
    run=""
    [ -n "${STUB_SYKLI_D3:-}" ] && run="run-$(date +%s)-$$"
    if [ -n "$detach" ]; then
      [ -n "$run" ] || { echo "error: unexpected argument '--detach' found" >&2; exit 2; }
      mkdir -p "$SANDBOX/runs/$run"
      printf '%s\n' "$PWD" > "$SANDBOX/runs/$run/cwd"
      # With $SANDBOX/record-checkout: the checkout as the run found it.
      if [ -f "$SANDBOX/record-checkout" ]; then
        git status --porcelain=v1 --untracked-files=all --ignored -- . ':(exclude).sykli' \
          > "$SANDBOX/runs/$run/status" 2>&1
        git rev-parse HEAD > "$SANDBOX/runs/$run/head"
        # A submodule as the run found it: what it holds, and its status.
        if [ -d sub ]; then
          { ls -A sub; ls -A sub/target 2>/dev/null; git -C sub status --porcelain 2>/dev/null; } \
            > "$SANDBOX/runs/$run/sub" 2>&1
        fi
        find . -path ./.sykli -prune -o -name .sykli -print > "$SANDBOX/runs/$run/nested-sykli"
      fi
      # With $SANDBOX/sub-work, a gate that builds in its submodule (and with
      # $SANDBOX/sub-dirty, changes a file there); with $SANDBOX/nested-sykli,
      # one that leaves a .sykli folder below the top.
      if [ -f "$SANDBOX/sub-work" ]; then
        git -c protocol.file.allow=always submodule update --init -q >/dev/null 2>&1
        mkdir -p sub/target && echo x > "sub/target/leftover-$run"
        [ -f "$SANDBOX/sub-dirty" ] && echo dirty >> sub/file
      fi
      if [ -f "$SANDBOX/nested-sykli" ]; then
        mkdir -p target/deep/.sykli && echo k > "target/deep/.sykli/keep-$run"
      fi
      # With $SANDBOX/cargo-run: what the checkout's program prints, built by cargo.
      if [ -f "$SANDBOX/cargo-run" ]; then
        cargo run -q > "$SANDBOX/runs/$run/out" 2> "$SANDBOX/runs/$run/err"
      fi
      # With $SANDBOX/dirty-the-checkout: a run that leaves its leftovers.
      if [ -f "$SANDBOX/dirty-the-checkout" ]; then
        echo junk > leftover; echo more >> README; mkdir -p _build && echo o > _build/o
      fi
      if [ ! -f "$SANDBOX/hold-runs" ]; then
        receipt passed "$run" > "$SANDBOX/runs/$run/receipt"
      fi
      printf '{"schema":"sykli-run.v1","run":"%s"}\n' "$run"
      exit 0
    fi
    if [ -f "$SANDBOX/fail-gate" ]; then
      cat "$(receipt failed "$run")"; echo "failed: says-one" >&2; exit 1
    fi
    cat "$(receipt passed "$run")"; echo "passed: says-one" >&2; exit 0;;
  status)
    if [ "$2" = --help ]; then
      echo "Usage: sykli status [OPTIONS] [ID]"
      [ -n "${STUB_SYKLI_D3:-}" ] && echo "      --run <ID>  a validation run"
      if [ -n "${STUB_SYKLI_STATUS_COMMIT:-}" ]; then
        echo "      --commit <REV>        receipts and runs that apply to this commit"
        echo "      --contract <CONTRACT> with --commit: the contract to judge by"
      fi
      facts; exit 0
    fi
    # sykli-commit-status.v1: $SANDBOX/commit-status/<rev> when a test wrote
    # one, else a verdict of $SANDBOX/commit-verdict (default none).
    if [ -n "${STUB_SYKLI_STATUS_COMMIT:-}" ] && [ "$2" = --commit ]; then
      rev=$3
      printf '%s|%s\n' "$PWD" "$*" >> "$SANDBOX/commit-asks"
      if [ -f "$SANDBOX/commit-status/$rev" ]; then cat "$SANDBOX/commit-status/$rev"; exit 0; fi
      verdict=$(cat "$SANDBOX/commit-verdict" 2>/dev/null || echo none)
      printf '{"schema":"sykli-commit-status.v1","commit":"%s","tree":"","contract":{"path":"x","hash":"c0ffee"},"verdict":"%s","current":[],"historical":[],"runs":[]}\n' "$rev" "$verdict"
      exit 0
    fi
    [ -n "${STUB_SYKLI_D3:-}" ] && [ "$2" = --run ] || { echo "error: no such production" >&2; exit 2; }
    run=$3
    [ -d "$SANDBOX/runs/$run" ] || { echo "error: no run $run" >&2; exit 2; }
    # With $SANDBOX/run-status-fails, sykli cannot say; with
    # $SANDBOX/run-status-json, it says that (RUN replaced by the run's id).
    [ -f "$SANDBOX/run-status-fails" ] && { echo "error: the run's journal cannot be read" >&2; exit 2; }
    if [ -f "$SANDBOX/run-status-json" ]; then
      sed "s/RUN/$run/" "$SANDBOX/run-status-json"; exit 0
    fi
    if [ ! -f "$SANDBOX/runs/$run/receipt" ] && [ ! -f "$SANDBOX/hold-runs" ]; then
      (cd "$(cat "$SANDBOX/runs/$run/cwd")" && receipt passed "$run") > "$SANDBOX/runs/$run/receipt"
    fi
    tree=$(cat "$SANDBOX/tree" 2>/dev/null); dirty=false
    [ -f "$SANDBOX/dirty-tree" ] && { tree=$(cat "$SANDBOX/dirty-tree"); dirty=true; }
    if [ -f "$SANDBOX/run-state" ] && [ ! -f "$SANDBOX/runs/$run/receipt" ]; then
      # With $SANDBOX/run-state, a run that ended so, without a receipt.
      printf '{"schema":"sykli-run-status.v1","run":"%s","state":"%s","subject":{"tree":"%s","commit":null,"dirty":%s},"tasks":[{"name":"t","state":"interrupted","attempts":[]}],"receipt":null}\n' \
        "$run" "$(cat "$SANDBOX/run-state")" "$tree" "$dirty"
    elif [ -f "$SANDBOX/runs/$run/receipt" ]; then
      path=$(cat "$SANDBOX/runs/$run/receipt")
      printf '{"schema":"sykli-run-status.v1","run":"%s","state":"completed","subject":{"tree":"%s","commit":null,"dirty":%s},"contract_digest":"c0ffee","lock_digest":"10c4","tasks":[{"name":"t","state":"passed","attempts":[{"id":"a1","started":1,"finished":2,"exit":0,"signal":null,"outcome":"passed","supersedes":null}]},{"name":"u","state":"cached","attempts":[]}],"receipt":{"path":"%s","digest":"%s"}}\n' \
        "$run" "$tree" "$dirty" "$path" "$(digest "$path")"
    else
      printf '{"schema":"sykli-run-status.v1","run":"%s","state":"running","subject":{"tree":"%s","commit":null,"dirty":%s},"contract_digest":"c0ffee","lock_digest":"10c4","tasks":[{"name":"t","state":"running","attempts":[{"id":"a1","started":1,"finished":null,"exit":null,"signal":null,"outcome":null,"supersedes":null}]},{"name":"u","state":"pending","attempts":[]}],"receipt":null}\n' \
        "$run" "$tree" "$dirty"
    fi
    exit 0;;
  verify)
    if [ "$2" = --help ]; then
      echo "Usage: sykli verify [OPTIONS] <RECEIPT>"
      echo "      --commit <REV>  judge the receipt as evidence for this commit"
      [ -f "$SANDBOX/verify-json" ] && echo "      --json          print the verdict as one document"
      facts; exit 0
    fi
    json=""; rev=""; prev=""; rcpt=$2
    for arg in "$@"; do
      [ "$arg" = --json ] && json=1
      [ "$prev" = --commit ] && rev=$arg
      prev=$arg
    done
    # With --json, the verdict as sykli-verify.v1 (verified, exit, the
    # receipt's claimed outcome as its `outcome` check), in the schema and
    # with the tiers $SANDBOX/verify-schema, $SANDBOX/verify-established and
    # $SANDBOX/verify-not-established say, when they do.
    verdict() {
      [ -n "$json" ] || return 0
      schema=$(cat "$SANDBOX/verify-schema" 2>/dev/null || echo sykli-verify.v1)
      tiers=$(cat "$SANDBOX/verify-established" 2>/dev/null \
        || echo '"schema","contract","source","consistency","outcome"')
      not=$(cat "$SANDBOX/verify-not-established" 2>/dev/null \
        || echo '"provenance","producer","execution"')
      claimed=passed
      grep -q '"outcome":"cached"' "$rcpt" && claimed=cached
      grep -q failed "$rcpt" && claimed=failed
      # With $SANDBOX/verify-claimed-outcome, the newer sykli: the claim as
      # its own field, and the check's words no longer saying it.
      field=""; actual="$claimed with 1 task records"
      if [ -f "$SANDBOX/verify-claimed-outcome" ]; then
        field="\"claimed_outcome\":\"$(cat "$SANDBOX/verify-claimed-outcome")\","
        actual="as claimed, with 1 task records"
      fi
      printf '{"schema":"%s","subject":{"kind":"commit","rev":"%s"},"verified":%s,"exit":%s,%s"checks":[{"name":"outcome","ok":%s,"expected":"passed or cached with complete task records","actual":"%s"}],"trust":{"established":[%s],"not_established":[%s]}}\n' \
        "$schema" "$rev" "$1" "$2" "$field" "$1" "$actual" "$tiers" "$not"
    }
    if grep -q other-contract "$2"; then
      verdict false 4; echo "refused: contract expected 1111 but got 2222" >&2; exit 4
    fi
    # passed, or cached (every task reused): sykli verify accepts both.
    grep -q passed "$2" || { grep -q '"outcome":"cached"' "$2" && ! grep -q failed "$2"; } \
      || { verdict false 1; echo "refused: outcome expected passed or cached" >&2; exit 1; }
    verdict true 0
    exit 0;;
esac
exit 2
"#;

/// Never the real toimija: no live sessions (unless a test writes a
/// checkout's `sessions --json` into `$SANDBOX/toimija-sessions/<its folder
/// name>`), so `ohjaa run --dry-run` gets as far as its warnings; `run` runs
/// the agent command after `--`, as toimija does, and keeps its argv in
/// `$SANDBOX/toimija-argv`.
const TOIMIJA_STUB: &str = r#"#!/bin/sh
case "$1" in
  sessions)
    # A checkout's sessions: $SANDBOX/toimija-sessions/<its folder name>, else none.
    listed="$SANDBOX/toimija-sessions/$(basename "$PWD")"
    if [ -f "$listed" ]; then cat "$listed"
    else echo '{"sessions":[],"warnings":[],"coverage":{"status":"available"}}'; fi;;
  context) echo '{}';;
  run)
    printf '%s\n' "$@" > "$SANDBOX/toimija-argv"
    while [ $# -gt 0 ] && [ "$1" != "--" ]; do shift; done
    shift
    exec "$@";;
esac
"#;

const CLAUDE_STUB: &str = "#!/bin/sh\necho \"direct $*\"\n";
const CODEX_STUB: &str = "#!/bin/sh\necho \"direct-codex $*\"\n";

const OLD_ZSHRC: &str = "export PATH=\"$HOME/.local/bin:$PATH\"\n\
    # Route claude/codex through ohjaa (agent delivery workflow, step 1). Bypass: command claude, or OHJAA_WRAP=0.\n\
    [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh\n\
    alias ll='ls -l'\n";

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// `command` in a session of its own (setsid(2)): no controlling terminal,
/// whatever the test runner has.
fn without_terminal(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid(2) is async-signal-safe and allocates nothing.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "macos")]
const TIOCSCTTY: libc::c_ulong = 0x2000_7461;
#[cfg(not(target_os = "macos"))]
const TIOCSCTTY: libc::Ioctl = libc::TIOCSCTTY;

/// `ptsname(3)` is not thread-safe, and tests run side by side.
static PTSNAME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `command` at a terminal of its own, as a person's shell runs one: a
/// session of its own whose controlling terminal is a new pseudo-terminal
/// (its stdio stay pipes, so its output is read). Its output.
fn with_terminal(command: &mut Command) -> Output {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: plain libc calls on a descriptor this function owns; the
    // pre_exec closure makes only async-signal-safe calls.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "{}", std::io::Error::last_os_error());
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let name = {
            let _one = PTSNAME.lock().unwrap();
            std::ffi::CStr::from_ptr(libc::ptsname(master)).to_owned()
        };
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let fd = libc::open(name.as_ptr(), libc::O_RDWR);
            if fd < 0 || libc::ioctl(fd, TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Kept open: on macOS a terminal whose last descriptor closes is
            // no longer the session's.
            Ok(())
        });
        let output = command.output().unwrap();
        libc::close(master);
        output
    }
}

/// The names of the files in a directory, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

struct Sandbox {
    dir: PathBuf,
    path: String,
    /// STUB_SYKLI_CONTRACT: "" (an old sykli), "nobranch" or "1".
    stub_contract: &'static str,
    /// STUB_SYKLI_D3: a sykli with durable runs (KATANA D3).
    stub_d3: bool,
    /// STUB_SYKLI_STATUS_COMMIT: a sykli with `status --commit` (KATANA D3).
    stub_status_commit: bool,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ohjaa-home-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        for sub in [
            "bin", "home", "config", "state", "cargo", "tmp", "projects", "remotes",
        ] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let bin = dir.join("bin");
        write_executable(&bin.join("sykli"), SYKLI_STUB);
        write_executable(&bin.join("claude"), CLAUDE_STUB);
        write_executable(&bin.join("toimija"), TOIMIJA_STUB);
        write_executable(&bin.join("codex"), CODEX_STUB);
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ohjaa"), bin.join("ohjaa")).unwrap();
        // Only the stubs and the system: no real sykli, cargo, agent or ohjaa.
        let git_dir = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .find(|dir| dir.join("git").is_file())
            .unwrap_or_else(|| PathBuf::from("/usr/bin"));
        let path = format!(
            "{}:{}:/usr/bin:/bin:/usr/sbin:/sbin",
            bin.display(),
            git_dir.display()
        );
        Sandbox {
            dir,
            path,
            stub_contract: "",
            stub_d3: false,
            stub_status_commit: false,
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    fn config(&self) -> PathBuf {
        self.dir.join("config").join("ohjaa")
    }

    /// An entry's own hooks folder.
    fn entry_hooks(&self, owner: &str, name: &str) -> PathBuf {
        self.config()
            .join("repos")
            .join(owner)
            .join(name)
            .join("hooks")
    }

    /// The launcher line apply writes: the sandbox's ohjaa, by its path.
    fn launch_line(&self) -> String {
        let ohjaa = self.dir.join("bin/ohjaa");
        format!(
            "[[ -x '{0}' ]] && eval \"$('{0}' shell-init zsh)\"",
            ohjaa.display()
        )
    }

    fn projects(&self) -> String {
        self.dir.join("projects").to_string_lossy().into_owned()
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("CARGO_HOME", self.dir.join("cargo"))
            .env("TMPDIR", self.dir.join("tmp"))
            .env("GIT_CONFIG_GLOBAL", self.dir.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.dir)
            .env("GIT_AUTHOR_NAME", "Ohjaa Test")
            .env("GIT_AUTHOR_EMAIL", "ohjaa@example.invalid")
            .env("GIT_COMMITTER_NAME", "Ohjaa Test")
            .env("GIT_COMMITTER_EMAIL", "ohjaa@example.invalid")
            .env("SANDBOX", &self.dir);
        if !self.stub_contract.is_empty() {
            command.env("STUB_SYKLI_CONTRACT", self.stub_contract);
        }
        if self.stub_d3 {
            command.env("STUB_SYKLI_D3", "1");
        }
        if self.stub_status_commit {
            command.env("STUB_SYKLI_STATUS_COMMIT", "1");
        }
        command
    }

    fn ohjaa_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("ohjaa", cwd).args(args).output().unwrap()
    }

    fn ohjaa(&self, args: &[&str]) -> Output {
        self.ohjaa_in(&self.dir, args)
    }

    fn git_out(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("git", cwd).args(args).output().unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_out(cwd, args);
        let (stdout, stderr) = text(&output);
        assert!(
            output.status.success(),
            "git {args:?} in {}: {stderr}",
            cwd.display()
        );
        stdout.trim().to_owned()
    }

    /// `core.hooksPath` as git resolves it in `repo`, `None` when unset.
    fn hooks_path(&self, repo: &Path) -> Option<String> {
        let output = self.git_out(repo, &["config", "--get", "core.hooksPath"]);
        output
            .status
            .success()
            .then(|| text(&output).0.trim().to_owned())
    }

    /// Whether git runs an entry's hooks in `repo`.
    fn gated(&self, repo: &Path) -> bool {
        let repos = format!("{}/", self.config().join("repos").display());
        self.hooks_path(repo)
            .is_some_and(|path| path.starts_with(&repos) && path.ends_with("/hooks"))
    }

    /// `projects/<name>`, with one commit holding `files`, pushed to a bare
    /// `remotes/<owner>/<name>.git` as origin's main, and origin/HEAD set.
    fn repo(&self, owner: &str, name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let bare = self
            .dir
            .join("remotes")
            .join(owner)
            .join(format!("{name}.git"));
        fs::create_dir_all(&bare).unwrap();
        self.git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        let repo = self.dir.join("projects").join(name);
        fs::create_dir_all(&repo).unwrap();
        self.git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README"), "hello\n").unwrap();
        for (file, contents) in files {
            fs::write(repo.join(file), contents).unwrap();
        }
        self.git(&repo, &["add", "-A"]);
        self.git(&repo, &["commit", "-q", "-m", "init"]);
        self.git(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
        self.git(&repo, &["push", "-q", "-u", "origin", "main"]);
        self.git(&repo, &["remote", "set-head", "origin", "main"]);
        (repo, bare)
    }

    /// An entry written by hand: `repo.toml`, and a gate unless `None`.
    fn entry(&self, owner: &str, name: &str, gate: Option<&str>) {
        let dir = self.config().join("repos").join(owner).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("repo.toml"), "branch = \"main\"\n").unwrap();
        if let Some(gate) = gate {
            fs::write(dir.join("sykli.json"), gate).unwrap();
        }
    }

    fn commit(&self, repo: &Path, file: &str) -> String {
        fs::write(repo.join(file), file).unwrap();
        self.git(repo, &["add", file]);
        self.git(repo, &["commit", "-q", "-m", file]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    fn status(&self, repo: &Path) -> String {
        self.git(
            repo,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignored",
            ],
        )
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    fn apply(&self) {
        let output = self.ohjaa(&["apply", "--root", &self.projects()]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }

    fn adopt(&self, repo: &Path) {
        let output = self.ohjaa(&["adopt", &repo.to_string_lossy()]);
        let (stdout, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    }

    /// The ohjaa processes git starts for `args` in `repo`: its hook runs from
    /// the home's hooks folder, as GIT_TRACE reports them.
    fn ohjaa_runs(&self, repo: &Path, args: &[&str]) -> usize {
        let output = self
            .command("git", repo)
            .env("GIT_TRACE", "1")
            .args(args)
            .output()
            .unwrap();
        let (_, stderr) = text(&output);
        // `trace: run_command: [VAR=value …] <entry>/hooks/<name> args`
        let repos = format!(" {}/", self.config().join("repos").display());
        stderr
            .lines()
            .filter(|line| {
                line.contains("run_command:") && line.contains(&repos) && line.contains("/hooks/")
            })
            .count()
    }

    /// The `start` events of the git processes `command` starts, from git's
    /// own trace2 event stream, with its output. A missing or empty stream
    /// fails the test: a count of nothing must never pass for zero.
    fn git_starts(&self, command: &mut Command) -> (Vec<String>, Output) {
        let (events, output) = self.git_events(command);
        let starts = events
            .into_iter()
            .filter(|line| line.contains("\"event\":\"start\""))
            .collect();
        (starts, output)
    }

    /// Every trace2 event of the git processes `command` starts; a missing
    /// or empty stream fails the test.
    fn git_events(&self, command: &mut Command) -> (Vec<String>, Output) {
        static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let trace = self.dir.join(format!("trace2-{n}.json"));
        command.env("GIT_TRACE2_EVENT", &trace);
        let output = command.output().unwrap();
        let events = fs::read_to_string(&trace).unwrap_or_default();
        assert!(
            !events.trim().is_empty(),
            "no trace2 events in {}: git never ran under {command:?}\n{}",
            trace.display(),
            text(&output).1
        );
        (events.lines().map(str::to_owned).collect(), output)
    }
}

/// Whether one of the `start` events is git's own `git … <word> …` (trace2
/// names git by the path it ran from).
fn started(starts: &[String], word: &str) -> bool {
    let word = format!("\"{word}\"");
    starts.iter().any(|line| {
        line.split_once("\"argv\":[")
            .and_then(|(_, argv)| argv.split(',').next())
            .is_some_and(|program| program.ends_with("git\""))
            && line.contains(&word)
    })
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

#[test]
fn apply_dry_run_says_every_change_and_makes_none() {
    let sandbox = Sandbox::new("dry");
    fs::write(sandbox.home().join(".zshrc"), OLD_ZSHRC).unwrap();
    sandbox.repo("acme", "any", &[]);
    let output = sandbox.ohjaa(&["apply", "--dry-run", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    for said in [
        format!(
            "would write {} with {}, the roots apply and doctor scan",
            sandbox.config().join("roots").display(),
            sandbox.projects()
        ),
        format!(
            "would write {}",
            sandbox.config().join("git/config").display()
        ),
        format!(
            "would add include.path = {} to git's global config, {}",
            sandbox.config().join("git/config").display(),
            sandbox.dir.join("gitconfig").display()
        ),
        format!(
            "would add .sykli/ .teko/ to {}",
            sandbox.dir.join("config/git/ignore").display()
        ),
        format!("would change {}", sandbox.home().join(".zshrc").display()),
        "  - [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh".to_owned(),
        format!("  + {}", sandbox.launch_line()),
        "dry run; nothing was changed".to_owned(),
    ] {
        assert!(stdout.contains(&said), "missing {said:?} in:\n{stdout}");
    }
    assert!(!sandbox.config().exists(), "nothing written in the home");
    assert!(
        !sandbox.dir.join("gitconfig").exists(),
        "global config untouched"
    );
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        OLD_ZSHRC
    );
}

#[test]
fn apply_includes_one_file_and_no_repository_without_an_entry_runs_ohjaa() {
    let sandbox = Sandbox::new("apply");
    fs::write(sandbox.home().join(".zshrc"), OLD_ZSHRC).unwrap();
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    let (other, bare) = sandbox.repo("acme", "other", &[]);
    let (widget, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&widget);

    // One include, no global core.hooksPath.
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get-all", "include.path"]
        ),
        sandbox.config().join("git/config").to_string_lossy()
    );
    let global = sandbox.git_out(
        &sandbox.dir,
        &["config", "--global", "--get", "core.hooksPath"],
    );
    assert!(!global.status.success(), "no global core.hooksPath");
    // The entry's own folder: only pre-push, since the clone has no hook of
    // its own; the dispatcher names ohjaa and sykli by their paths.
    let hooks = sandbox.entry_hooks("acme", "widget");
    assert_eq!(names_in(&hooks), ["pre-push"]);
    let dispatcher = fs::read_to_string(hooks.join("pre-push")).unwrap();
    assert!(dispatcher.contains(&format!(
        "OHJAA_SYKLI='{}'; export OHJAA_SYKLI\n",
        sandbox.dir.join("bin/sykli").display()
    )));
    assert!(dispatcher.ends_with(&format!(
        "exec '{}' hook pre-push \"$@\"\n",
        sandbox.dir.join("bin/ohjaa").display()
    )));
    assert!(executable(&hooks.join("pre-push")));
    assert_eq!(names_in(&sandbox.config().join("git")), ["config"]);
    assert_eq!(
        sandbox.hooks_path(&widget).as_deref(),
        Some(hooks.to_str().unwrap())
    );
    // The roots apply scanned are written down.
    assert!(sandbox
        .read("config/ohjaa/roots")
        .contains(&sandbox.projects()));
    // A repository whose remote names no entry never runs ohjaa.
    assert!(!sandbox.gated(&other));
    assert_eq!(
        sandbox.ohjaa_runs(&other, &["commit", "-q", "--allow-empty", "-m", "x"]),
        0
    );
    assert_eq!(
        sandbox.ohjaa_runs(&other, &["push", "-q", "origin", "main"]),
        0
    );
    // git init and git clone still work.
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "cloned"],
    );
    // Git reads the ignore file apply wrote: .sykli/ is ignored everywhere.
    let ignore = sandbox.read("config/git/ignore");
    assert!(
        ignore.starts_with("*.swp\n") && ignore.contains("\n.sykli/\n"),
        "{ignore}"
    );
    sandbox.git(&other, &["check-ignore", "-q", ".sykli/receipt.json"]);
    let zshrc = fs::read_to_string(sandbox.home().join(".zshrc")).unwrap();
    assert!(!zshrc.contains("ohjaa-launch.zsh"), "{zshrc}");
    assert!(
        zshrc.ends_with(&format!("{}\nalias ll='ls -l'\n", sandbox.launch_line())),
        "{zshrc}"
    );

    let before = (
        sandbox.read("gitconfig"),
        sandbox.read("config/git/ignore"),
        zshrc,
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    assert_eq!(
        before,
        (
            sandbox.read("gitconfig"),
            sandbox.read("config/git/ignore"),
            fs::read_to_string(sandbox.home().join(".zshrc")).unwrap()
        )
    );

    // An edited dispatcher is drift, and apply puts it back.
    fs::write(hooks.join("pre-push"), "#!/bin/sh\nexit 0\n").unwrap();
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("pre-push is not one of apply's dispatchers"),
        "{stdout}"
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    assert!(
        text(&output).0.contains("wrote 1 dispatcher into"),
        "{}",
        text(&output).0
    );
}

/// Every form a remote URL takes, against the patterns apply generates for
/// the entry false-systems/ohjaa (and git's insteadOf rules).
#[test]
fn the_remote_patterns_gate_every_url_form_and_nothing_else() {
    let sandbox = Sandbox::new("patterns");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "url.git@github.com:.insteadOf", "gh:"],
    );
    sandbox.git(
        &sandbox.dir,
        &[
            "config",
            "--global",
            "url.git@github.com:false-systems/.insteadOf",
            "fs:",
        ],
    );
    sandbox.entry("false-systems", "ohjaa", Some("{}\n"));
    sandbox.apply();
    let matrix: &[(&[&str], bool)] = &[
        (&["git@github.com:false-systems/ohjaa.git"], true),
        (&["git@github.com:false-systems/ohjaa"], true),
        (&["https://github.com/false-systems/ohjaa"], true),
        (&["https://github.com/false-systems/ohjaa.git"], true),
        (&["https://github.com/false-systems/ohjaa/"], true),
        (&["https://github.com/false-systems/ohjaa.git/"], true),
        (&["git@github.com:false-systems/ohjaa.git/"], true),
        (&["ssh://git@github.com/false-systems/ohjaa.git"], true),
        (&["ssh://git@github.com:22/false-systems/ohjaa.git"], true),
        (&["github.com:false-systems/ohjaa.git"], true),
        (&["https://x@github.com/false-systems/ohjaa.git"], true),
        (&["https://github.com/False-Systems/ohjaa.git"], true),
        (&["https://GITHUB.com/FALSE-SYSTEMS/OHJAA"], true),
        (&["gh:false-systems/ohjaa"], true),
        (&["fs:ohjaa.git"], true),
        (
            &[
                "git@github.com:yairfalse/fork.git",
                "git@github.com:false-systems/ohjaa.git",
            ],
            true,
        ),
        (
            &["https://evil.example/github.com/false-systems/ohjaa"],
            true,
        ),
        (&["https://github.com/false-systems/ohjaa-other.git"], false),
        (&["git@github.com:false-systems/ohjaa-other"], false),
        (&["git@github.com:yairfalse/ohjaa.git"], false),
        (&["https://github.com/other/ohjaa"], false),
        (&["git@github.com:false-systems/xohjaa"], false),
        (&["fs:ohjaa-other"], false),
        (&[], false),
    ];
    let mut wrong = Vec::new();
    for (at, (urls, gated)) in matrix.iter().enumerate() {
        let repo = sandbox.dir.join(format!("r{at}"));
        sandbox.git(&sandbox.dir, &["init", "-q", &repo.to_string_lossy()]);
        for (n, url) in urls.iter().enumerate() {
            sandbox.git(&repo, &["remote", "add", &format!("r{n}"), url]);
        }
        let label = if urls.is_empty() {
            "(no remote)".to_owned()
        } else {
            urls.join(" + ")
        };
        eprintln!(
            "{label:<72} gated: {}",
            if sandbox.gated(&repo) { "yes" } else { "no" }
        );
        if sandbox.gated(&repo) != *gated {
            wrong.push(label);
        }
    }
    assert!(wrong.is_empty(), "wrongly gated or not: {wrong:?}");

    // A linked worktree of a gated clone is gated too.
    let gated = sandbox.dir.join("r0");
    sandbox.git(&gated, &["commit", "-q", "--allow-empty", "-m", "x"]);
    let linked = sandbox.dir.join("r0-linked");
    sandbox.git(
        &gated,
        &["worktree", "add", "-q", &linked.to_string_lossy()],
    );
    assert!(sandbox.gated(&linked));
}

#[test]
fn a_gated_repository_starts_ohjaa_only_for_push_and_for_hooks_it_has() {
    let sandbox = Sandbox::new("count");
    let (gated, _) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let (free, _) = sandbox.repo("acme", "free", &[]);
    write_executable(
        &own.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho own >> \"$SANDBOX/own-calls\"\n",
    );
    fs::create_dir_all(gated.join(".sykli")).unwrap();
    fs::write(gated.join(".sykli/receipt.json"), "{}").unwrap();
    // (status, commit, fetch, push): the ohjaa processes git starts.
    let counts = |label: &str, repo: &Path| {
        let status = sandbox.ohjaa_runs(repo, &["status"]);
        let commit = sandbox.ohjaa_runs(repo, &["commit", "-q", "--allow-empty", "-m", "n"]);
        let fetch = sandbox.ohjaa_runs(repo, &["fetch", "-q", "origin"]);
        let push = sandbox.ohjaa_runs(repo, &["push", "-q", "origin", "HEAD:refs/heads/side"]);
        eprintln!("{label:<36} status {status}  commit {commit}  fetch {fetch}  push {push}");
        (status, commit, fetch, push)
    };

    sandbox.apply();
    sandbox.adopt(&gated);
    sandbox.adopt(&own);
    // Each entry has its own folder: own's holds its pre-commit too (adopt
    // wrote it), gated's only pre-push.
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "gated")),
        ["pre-push"]
    );
    let own_hooks = sandbox.entry_hooks("acme", "own");
    assert_eq!(names_in(&own_hooks), ["pre-commit", "pre-push"]);
    fs::remove_file(own_hooks.join("pre-commit")).unwrap();
    sandbox.apply();
    assert_eq!(
        names_in(&own_hooks),
        ["pre-commit", "pre-push"],
        "apply arrives at it too"
    );

    assert_eq!(counts("entry, with its own pre-commit", &own), (0, 1, 0, 1));
    assert_eq!(counts("entry, without", &gated), (0, 0, 0, 1));
    assert_eq!(counts("no entry", &free), (0, 0, 0, 0));
    assert_eq!(
        sandbox.read("own-calls"),
        "own\n",
        "the own pre-commit ran once"
    );
}

#[test]
fn apply_refuses_a_global_hooks_path_unless_forced_and_undo_puts_it_back() {
    let sandbox = Sandbox::new("refuse");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "core.hooksPath", "/elsewhere/hooks"],
    );
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(
        stderr.contains("core.hooksPath is /elsewhere/hooks"),
        "{stderr}"
    );
    assert!(stderr.contains("ohjaa apply --force"), "{stderr}");
    assert!(!sandbox.config().exists(), "nothing half applied");
    assert!(
        !sandbox.home().join(".zshrc").exists(),
        "nothing half applied"
    );

    let output = sandbox.ohjaa(&["apply", "--force", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("removed git's global core.hooksPath (/elsewhere/hooks)"),
        "{stdout}"
    );
    let global = sandbox.git_out(
        &sandbox.dir,
        &["config", "--global", "--get", "core.hooksPath"],
    );
    assert!(!global.status.success());

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        sandbox.git(
            &sandbox.dir,
            &["config", "--global", "--get", "core.hooksPath"]
        ),
        "/elsewhere/hooks"
    );
}

#[test]
fn undo_puts_back_exactly_what_apply_changed() {
    let sandbox = Sandbox::new("undo");
    let gitconfig = "[user]\n\tname = Someone\n[alias]\n\tst = status\n";
    fs::write(sandbox.dir.join("gitconfig"), gitconfig).unwrap();
    // No final newline in either: undo keeps it so.
    let zshrc = OLD_ZSHRC.trim_end();
    fs::write(sandbox.home().join(".zshrc"), zshrc).unwrap();
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp").unwrap();
    let (repo, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    assert!(sandbox.gated(&repo));

    let output = sandbox.ohjaa(&["apply", "--undo", "--dry-run"]);
    let (stdout, _) = text(&output);
    for said in [
        "would remove include.path = ",
        "would remove the \"ohjaa apply\" block from ",
        "would change ",
        "  + [[ -r ~/projects/ohjaa-launch.zsh ]] && source ~/projects/ohjaa-launch.zsh",
        "would remove what apply generated in ",
        "repos/acme/widget/hooks/pre-push",
        "repos/acme/widget/gated.config",
        "dry run; nothing was changed",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
    assert!(
        sandbox.entry_hooks("acme", "widget").exists() && sandbox.gated(&repo),
        "a dry run undoes nothing"
    );

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(sandbox.read("gitconfig"), gitconfig);
    assert_eq!(
        fs::read_to_string(sandbox.home().join(".zshrc")).unwrap(),
        zshrc
    );
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp");
    assert_eq!(
        names_in(&sandbox.config()),
        ["repos", "roots"],
        "entries and roots kept"
    );
    assert_eq!(
        names_in(&sandbox.config().join("repos/acme/widget")),
        ["repo.toml", "sykli.json"]
    );
    assert!(!sandbox.gated(&repo));
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert!(text(&output).0.contains("nothing to undo"));
}

#[test]
fn undo_removes_the_files_apply_created() {
    let sandbox = Sandbox::new("undo-created");
    sandbox.apply();
    assert!(sandbox.home().join(".zshrc").exists());
    assert!(sandbox.dir.join("config/git/ignore").exists());
    assert!(sandbox.dir.join("gitconfig").exists());
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(!sandbox.home().join(".zshrc").exists());
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    assert!(
        !sandbox.dir.join("gitconfig").exists(),
        "a global config apply created is removed, not left empty"
    );
    assert!(
        names_in(&sandbox.config()).is_empty(),
        "{:?}",
        names_in(&sandbox.config())
    );
}

/// Each change is recorded before it is made: an apply that fails midway
/// can still be undone.
#[test]
fn an_apply_that_fails_midway_can_be_undone() {
    let sandbox = Sandbox::new("midway");
    let gitconfig = "[user]\n\tname = Someone\n";
    fs::write(sandbox.dir.join("gitconfig"), gitconfig).unwrap();
    // The ignore file's folder cannot be written: apply fails there, after
    // the include is in git's global config.
    let folder = sandbox.dir.join("config/git");
    fs::create_dir_all(&folder).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
    let output = sandbox.ohjaa(&["apply", "--root", &sandbox.projects()]);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(output.status.code(), Some(0), "{}", text(&output).0);
    assert!(sandbox.read("gitconfig").contains("[include]"));
    assert!(sandbox
        .read("config/ohjaa/applied.toml")
        .contains("include = "));
    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("gitconfig"), gitconfig);
}

#[test]
fn apply_writes_the_ignore_block_where_core_excludes_file_points() {
    let sandbox = Sandbox::new("excludes");
    sandbox.git(
        &sandbox.dir,
        &["config", "--global", "core.excludesFile", "~/my-ignore"],
    );
    sandbox.apply();
    assert!(fs::read_to_string(sandbox.home().join("my-ignore"))
        .unwrap()
        .contains(".sykli/"));
    assert!(!sandbox.dir.join("config/git/ignore").exists());
}

#[test]
fn adopt_copies_the_gate_from_the_default_branch_and_never_writes_into_the_repo() {
    let sandbox = Sandbox::new("adopt");
    let contract = "{\"version\":\"1\",\"tasks\":{\"test\":{}}}\n";
    let lock = "{\"schema\":\"sykli-lock.v2\"}\n";
    let (repo, _) = sandbox.repo(
        "acme",
        "widget",
        &[
            ("sykli.json", contract),
            ("sykli.lock", lock),
            (".gitignore", "target/\n"),
        ],
    );
    sandbox.apply();
    let global_before = sandbox.read("gitconfig");
    // Someone's uncommitted work: the gate comes from origin/main, not from it.
    fs::write(repo.join("sykli.json"), "{\"edited\":true}\n").unwrap();
    fs::write(repo.join("notes.txt"), "untracked\n").unwrap();
    fs::create_dir_all(repo.join("target")).unwrap();
    fs::write(repo.join("target/x"), "ignored\n").unwrap();
    let before = sandbox.status(&repo);
    assert!(!sandbox.gated(&repo));
    // A tracked file whose stat no longer matches the index: a `git status`
    // would rewrite .git/index (and take index.lock) to refresh it.
    fs::write(repo.join("README"), "hello\n").unwrap();
    let index = repo.join(".git/index");
    let index_before = fs::metadata(&index).unwrap().modified().unwrap();

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::metadata(&index).unwrap().modified().unwrap(),
        index_before,
        "adopt touched .git/index"
    );
    let entry = sandbox.config().join("repos/acme/widget");
    assert!(stdout.contains(&entry.display().to_string()), "{stdout}");
    assert!(stdout.contains("copied from origin/main"), "{stdout}");
    assert!(stdout.contains(&format!(
        "wrote {}",
        sandbox.config().join("git/config").display()
    )));
    assert!(stdout.contains("  observed: git runs"), "{stdout}");
    assert!(fs::read_to_string(entry.join("repo.toml"))
        .unwrap()
        .contains("\nbranch = \"main\"\n"));
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        contract
    );
    assert_eq!(fs::read_to_string(entry.join("sykli.lock")).unwrap(), lock);
    assert_eq!(
        sandbox.status(&repo),
        before,
        "adopt wrote into the repository"
    );
    assert_eq!(
        sandbox.read("gitconfig"),
        global_before,
        "adopt left git's global config alone"
    );
    assert!(sandbox.gated(&repo), "git runs ohjaa's hooks there now");

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    assert_eq!(output.status.code(), Some(3), "adopting twice is refused");
    assert!(text(&output).1.contains("already adopted"));
    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy(), "--force"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn adopt_asks_sykli_for_a_gate_kept_outside_the_repo_and_refuses_when_it_cannot() {
    let mut sandbox = Sandbox::new("adopt-init");
    let (plain, _) = sandbox.repo("acme", "plain", &[]);
    let before = sandbox.status(&plain);
    let output = sandbox.ohjaa(&["adopt", &plain.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("names no --contract"), "{stderr}");
    assert!(
        !sandbox.config().join("repos/acme/plain").exists(),
        "a refused adopt leaves no entry"
    );

    sandbox.stub_contract = "1";
    let output = sandbox.ohjaa(&["adopt", &plain.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let entry = sandbox.config().join("repos/acme/plain");
    assert!(entry.join("sykli.json").is_file());
    assert!(entry.join("repo.toml").is_file());
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|init --contract {}",
            plain.display(),
            entry.join("sykli.json").display()
        )),
        "{}",
        sandbox.read("sykli-calls")
    );
    assert_eq!(sandbox.status(&plain), before);
    // Before apply, adopt cannot make ohjaa observe it: git's global config
    // does not include the home's file, and adopt never writes git's global
    // config.
    assert!(
        stderr.contains("its pushes are not observed (ohjaa apply)"),
        "{stderr}"
    );
}

// ---- KATANA D10: a gate builds in its worktree's own directory -------------

/// A Rust repository's gate, laid out as sykli writes one: tasks that run
/// cargo (one with a `workdir`, one with an `env` of its own), one that names
/// its own target dir, one that inherits a build dir, and one that runs no
/// cargo.
const RUST_GATE: &str = r#"{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "fmt",
      "run": "cargo fmt --check",
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "doc",
      "run": "RUSTDOCFLAGS='-D warnings' cargo doc --no-deps",
      "workdir": "crates/core",
      "after": [
        "fmt"
      ],
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "test",
      "run": "cargo test --locked",
      "env": {
        "RUST_BACKTRACE": "1"
      },
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "own",
      "run": "cargo build --release",
      "env": {
        "CARGO_TARGET_DIR": "target/own"
      }
    },
    {
      "name": "inherited",
      "run": "cargo clippy",
      "inherit": [
        "CARGO_BUILD_BUILD_DIR"
      ]
    },
    {
      "name": "lint",
      "run": "./scripts/lint.sh"
    }
  ]
}
"#;

/// [`RUST_GATE`] as adopt leaves it in the entry: each cargo task that
/// declared neither variable gains both, where sykli's own emitter puts
/// `env` (after `run`, or after `workdir`), or at the end of its own `env`.
/// Nothing else moves.
const RUST_GATE_DECLARED: &str = r#"{
  "schema": "sykli-contract.v1",
  "tasks": [
    {
      "name": "fmt",
      "run": "cargo fmt --check",
      "env": {
        "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target",
        "CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build"
      },
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "doc",
      "run": "RUSTDOCFLAGS='-D warnings' cargo doc --no-deps",
      "workdir": "crates/core",
      "env": {
        "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target",
        "CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build"
      },
      "after": [
        "fmt"
      ],
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "test",
      "run": "cargo test --locked",
      "env": {
        "RUST_BACKTRACE": "1",
        "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target",
        "CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build"
      },
      "inputs": [
        "Cargo.toml"
      ]
    },
    {
      "name": "own",
      "run": "cargo build --release",
      "env": {
        "CARGO_TARGET_DIR": "target/own"
      }
    },
    {
      "name": "inherited",
      "run": "cargo clippy",
      "inherit": [
        "CARGO_BUILD_BUILD_DIR"
      ]
    },
    {
      "name": "lint",
      "run": "./scripts/lint.sh"
    }
  ]
}
"#;

/// An Erlang gate, like ote's: rebar3 and make under mise, no cargo.
const ERLANG_GATE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"fmt","run":"env REBAR_CACHE_DIR=$PWD/_build/rebar-cache rebar3 fmt --check","inputs":["rebar.config"]},{"name":"test","run":"mise exec -- make test","after":["fmt"]}]}
"#;

const COPIED_LOCK: &str = "{\"schema\":\"sykli-lock.v2\",\"contracts\":{\"sykli.json\":\"0\"}}\n";

/// The lock the stub sykli writes for `contract`.
fn stub_lock(contract: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{{\"schema\":\"sykli-lock.v2\",\"contracts\":{{\"sykli.json\":\"{:x}\"}}}}\n",
        Sha256::digest(contract.as_bytes())
    )
}

#[test]
fn adopt_declares_a_worktree_cache_for_each_cargo_task_and_locks_the_gate_again() {
    let sandbox = Sandbox::new("d10-adopt");
    fs::write(sandbox.dir.join("worktree-cache"), "").unwrap();
    let (rusty, _) = sandbox.repo(
        "acme",
        "rusty",
        &[("sykli.json", RUST_GATE), ("sykli.lock", COPIED_LOCK)],
    );
    let (erl, _) = sandbox.repo(
        "acme",
        "erl",
        &[("sykli.json", ERLANG_GATE), ("sykli.lock", COPIED_LOCK)],
    );
    let before = sandbox.status(&rusty);
    let entry = sandbox.config().join("repos/acme/rusty");

    let output = sandbox.ohjaa(&["adopt", &rusty.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        RUST_GATE_DECLARED
    );
    assert_eq!(
        fs::read_to_string(entry.join("sykli.lock")).unwrap(),
        stub_lock(RUST_GATE_DECLARED),
        "the entry's gate is locked again"
    );
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|lock {}\n",
            rusty.display(),
            entry.join("sykli.json").display()
        )),
        "{}",
        sandbox.read("sykli-calls")
    );
    for said in [
        "  ${worktree_cache}: CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR declared for fmt, \
         doc, test, and sykli.lock locked again",
        "kept as they declare it: own, inherited",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
    assert_eq!(
        sandbox.status(&rusty),
        before,
        "adopt wrote into the repository"
    );

    // Adopted again: the copy gains the lines once, never twice.
    let output = sandbox.ohjaa(&["adopt", &rusty.to_string_lossy(), "--force"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        RUST_GATE_DECLARED
    );
    assert_eq!(
        fs::read_to_string(entry.join("sykli.lock")).unwrap(),
        stub_lock(RUST_GATE_DECLARED)
    );

    // No task runs cargo: the gate is copied byte for byte, nothing locked.
    let calls = sandbox.read("sykli-calls").lines().count();
    let output = sandbox.ohjaa(&["adopt", &erl.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let erl_entry = sandbox.config().join("repos/acme/erl");
    assert_eq!(
        fs::read_to_string(erl_entry.join("sykli.json")).unwrap(),
        ERLANG_GATE
    );
    assert_eq!(
        fs::read_to_string(erl_entry.join("sykli.lock")).unwrap(),
        COPIED_LOCK
    );
    assert_eq!(
        sandbox.read("sykli-calls").lines().count(),
        calls,
        "sykli is not even asked: {}",
        sandbox.read("sykli-calls")
    );
    assert!(!stdout.contains("worktree_cache"), "{stdout}");
}

#[test]
fn an_older_sykli_gets_no_worktree_cache_lines_and_adopt_says_so() {
    let sandbox = Sandbox::new("d10-old");
    let (rusty, _) = sandbox.repo(
        "acme",
        "rusty",
        &[("sykli.json", RUST_GATE), ("sykli.lock", COPIED_LOCK)],
    );
    let entry = sandbox.config().join("repos/acme/rusty");

    let output = sandbox.ohjaa(&["adopt", &rusty.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        RUST_GATE
    );
    assert_eq!(
        fs::read_to_string(entry.join("sykli.lock")).unwrap(),
        COPIED_LOCK
    );
    for said in [
        "  ${worktree_cache}: not declared for fmt, doc, test: the sykli on PATH (",
        "does not name ${worktree_cache} in `sykli run --help`",
        "gate runs may share a target dir across worktrees",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
    assert!(
        !sandbox.read("sykli-calls").contains("|lock "),
        "{}",
        sandbox.read("sykli-calls")
    );

    // The sykli on PATH knows it, but the pre-push dispatcher hands pushes to
    // an older one, as apply recorded it: still no lines.
    fs::write(sandbox.dir.join("worktree-cache"), "").unwrap();
    sandbox.apply();
    let old = sandbox.dir.join("old/sykli");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    write_executable(
        &old,
        "#!/bin/sh\necho 'Usage: sykli run [OPTIONS] [CONTRACT]'\n",
    );
    let record = sandbox.config().join("applied.toml");
    let recorded = fs::read_to_string(&record).unwrap();
    let line = recorded
        .lines()
        .find(|line| line.starts_with("sykli = "))
        .unwrap_or_else(|| panic!("apply recorded no sykli:\n{recorded}"))
        .to_owned();
    fs::write(
        &record,
        recorded.replace(&line, &format!("sykli = \"{}\"", old.display())),
    )
    .unwrap();
    let output = sandbox.ohjaa(&["adopt", &rusty.to_string_lossy(), "--force"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        RUST_GATE
    );
    assert!(
        stdout.contains(&format!(
            "not declared for fmt, doc, test: the sykli the pre-push dispatcher runs ({}) \
             does not name ${{worktree_cache}}",
            old.display()
        )),
        "{stdout}"
    );
}

#[test]
fn a_gate_sykli_cannot_lock_again_is_kept_as_copied() {
    let sandbox = Sandbox::new("d10-lock");
    fs::write(sandbox.dir.join("worktree-cache"), "").unwrap();
    fs::write(sandbox.dir.join("lock-fails"), "").unwrap();
    let (rusty, _) = sandbox.repo(
        "acme",
        "rusty",
        &[("sykli.json", RUST_GATE), ("sykli.lock", COPIED_LOCK)],
    );
    let entry = sandbox.config().join("repos/acme/rusty");
    let output = sandbox.ohjaa(&["adopt", &rusty.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert_eq!(
        fs::read_to_string(entry.join("sykli.json")).unwrap(),
        RUST_GATE
    );
    assert_eq!(
        fs::read_to_string(entry.join("sykli.lock")).unwrap(),
        COPIED_LOCK
    );
    assert!(
        stderr.contains(&format!(
            "ohjaa adopt: warning: sykli lock {} failed (error: cannot hash",
            entry.join("sykli.json").display()
        )) && stderr.contains("the gate is kept as copied, without ${worktree_cache}"),
        "{stderr}"
    );
    assert!(!stdout.contains("declared for"), "{stdout}");
}

#[test]
fn doctor_flags_cargo_tasks_that_may_share_a_target_dir_across_worktrees() {
    let sandbox = Sandbox::new("d10-doctor");
    let (rusty, _) = sandbox.repo(
        "acme",
        "rusty",
        &[("sykli.json", RUST_GATE), ("sykli.lock", COPIED_LOCK)],
    );
    let (erl, _) = sandbox.repo("acme", "erl", &[("sykli.json", ERLANG_GATE)]);
    sandbox.apply();
    sandbox.adopt(&rusty);
    sandbox.adopt(&erl);

    // An older sykli: said, but no drift (adopting again would change nothing).
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "note   acme/rusty: fmt, doc, test run cargo without ${worktree_cache}: gate runs \
             may share a target dir across worktrees, and the sykli on PATH ("
        ),
        "{stdout}"
    );

    // sykli knows it now: the entry differs from what adopt would make it.
    fs::write(sandbox.dir.join("worktree-cache"), "").unwrap();
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "DRIFT  acme/rusty: fmt, doc, test run cargo without ${worktree_cache}: gate runs \
             may share a target dir across worktrees (ohjaa adopt --force <its clone>)"
        ),
        "{stdout}"
    );
    assert!(
        !stdout
            .lines()
            .any(|line| line.contains("acme/erl") && line.contains("cargo")),
        "an Erlang gate has no cargo task to flag:\n{stdout}"
    );

    sandbox.ohjaa(&["adopt", &rusty.to_string_lossy(), "--force"]);
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(
            "ok     acme/rusty: every cargo task declares where it builds (${worktree_cache} or \
             its own)"
        ),
        "{stdout}"
    );
}

#[test]
fn an_unproven_push_to_main_lands_with_a_flag_and_a_proven_one_is_quiet() {
    let mut sandbox = Sandbox::new("observe");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);

    // No receipt: sykli does not accept it, and the push lands all the same.
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", "main"]),
        head,
        "it landed"
    );
    let flags: Vec<&str> = stderr.lines().filter(|l| l.starts_with("ohjaa")).collect();
    assert_eq!(
        flags,
        [
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree). \
          It is pushed and flagged; prove it with: ohjaa gate"
        ],
        "one flag, in sykli's words: {stderr}"
    );
    assert!(
        !stderr.contains("sykli hook pre-push"),
        "sykli's own lines are not repeated: {stderr}"
    );
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        sandbox.read("sykli-calls").contains(&format!(
            "{}|hook pre-push --contract {} --branch main origin {}",
            repo.display(),
            contract.display(),
            bare.display()
        )),
        "{}",
        sandbox.read("sykli-calls")
    );
    assert!(sandbox
        .read("sykli-stdin")
        .starts_with(&format!("refs/heads/main {head} refs/heads/main {landed}")));

    // Proven (a receipt sykli accepts): quiet, sykli's lines and all.
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    let head = sandbox.commit(&repo, "proven");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        !stderr.contains("ohjaa:") && !stderr.contains("sykli hook"),
        "a proven push is quiet: {stderr}"
    );

    // A push that does not move the entry's branch is not judged at all.
    let calls = sandbox.read("sykli-calls").lines().count();
    sandbox.commit(&repo, "side");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/side"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
    assert_eq!(sandbox.read("sykli-calls").lines().count(), calls);
    assert_eq!(
        sandbox.git(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "the repository stays clean"
    );
}

#[test]
fn without_a_sykli_that_takes_a_contract_the_push_lands_flagged() {
    let sandbox = Sandbox::new("no-contract");
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    sandbox.commit(&repo, "change");

    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    let head = sandbox.commit(&repo, "more");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (this sykli cannot judge")
            && stderr.contains("names no --contract and no --branch"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);

    fs::remove_file(sandbox.dir.join("bin/sykli")).unwrap();
    let head = sandbox.commit(&repo, "again");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (the sykli `ohjaa apply` recorded (")
            && stderr.contains(") is gone). It is pushed and flagged"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

#[test]
fn an_entry_that_cannot_be_read_flags_the_push_and_lets_it_through() {
    let sandbox = Sandbox::new("broken-entry");
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    fs::create_dir_all(sandbox.config().join("repos/acme/widget")).unwrap();
    fs::write(
        sandbox.config().join("repos/acme/widget/sykli.json"),
        "{}\n",
    )
    .unwrap();
    sandbox.apply();
    assert!(
        sandbox.gated(&repo),
        "a broken entry still has its repository observed"
    );
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/feature"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNOBSERVED push to acme/widget: its entry cannot be read")
            && stderr.contains("no repo.toml"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "feature"]), head);
}

#[test]
fn a_repo_without_an_entry_pushes_freely() {
    let sandbox = Sandbox::new("free");
    let (repo, bare) = sandbox.repo("acme", "free", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "other", Some("{}\n"));
    sandbox.apply();
    let head = sandbox.commit(&repo, "change");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(!sandbox.read("sykli-calls").contains("hook pre-push"));
}

#[test]
fn the_repositorys_own_hooks_still_run_through_the_dispatcher() {
    let mut sandbox = Sandbox::new("chain");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let hooks = repo.join(".git/hooks");
    write_executable(
        &hooks.join("pre-commit"),
        "#!/bin/sh\necho \"pre-commit $#\" >> \"$SANDBOX/own-calls\"\n[ ! -f \"$SANDBOX/fail\" ]\n",
    );
    write_executable(
        &hooks.join("commit-msg"),
        "#!/bin/sh\necho \"commit-msg $1\" >> \"$SANDBOX/own-calls\"\n",
    );
    // No #! line: git runs such a hook through the shell, and so does ohjaa.
    write_executable(
        &hooks.join("post-commit"),
        "echo post-commit >> \"$SANDBOX/own-calls\"\n",
    );
    write_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\necho \"pre-push $*\" >> \"$SANDBOX/own-calls\"\ncat > \"$SANDBOX/own-stdin\"\n",
    );
    sandbox.apply();
    sandbox.adopt(&repo);
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "own")),
        ["commit-msg", "post-commit", "pre-commit", "pre-push"]
    );

    let first = sandbox.commit(&repo, "one");
    let calls = sandbox.read("own-calls");
    assert!(calls.contains("pre-commit 0\n"), "{calls}");
    assert!(
        calls.contains("commit-msg .git/COMMIT_EDITMSG\n"),
        "{calls}"
    );
    assert!(calls.contains("post-commit\n"), "{calls}");

    // The repository's own refusal still refuses.
    fs::write(sandbox.dir.join("fail"), "").unwrap();
    fs::write(repo.join("two"), "two").unwrap();
    sandbox.git(&repo, &["add", "two"]);
    let output = sandbox.git_out(&repo, &["commit", "-q", "-m", "two"]);
    assert!(!output.status.success(), "the own pre-commit refused");
    assert_eq!(sandbox.git(&repo, &["rev-parse", "HEAD"]), first);
    fs::remove_file(sandbox.dir.join("fail")).unwrap();

    // A linked worktree shares the common directory's hooks.
    let linked = sandbox.dir.join("linked");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            &linked.to_string_lossy(),
        ],
    );
    fs::write(sandbox.dir.join("own-calls"), "").unwrap();
    sandbox.commit(&linked, "side");
    assert!(sandbox.read("own-calls").contains("pre-commit 0\n"));

    // pre-push: sykli and the own hook read the same stdin.
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(sandbox
        .read("own-calls")
        .contains(&format!("pre-push origin {}\n", bare.display())));
    let own = sandbox.read("own-stdin");
    assert!(
        own.starts_with(&format!("refs/heads/main {first} refs/heads/main ")),
        "{own}"
    );
    assert_eq!(own, sandbox.read("sykli-stdin"));

    // The repository's own pre-push still refuses, unproven or not, and an
    // unproven push it refuses is not said to be pushed.
    write_executable(
        &hooks.join("pre-push"),
        "#!/bin/sh\ncat > /dev/null\necho 'own pre-push: no' >&2\nexit 3\n",
    );
    fs::remove_file(repo.join(".sykli/receipt.json")).unwrap();
    let landed = sandbox.git(&bare, &["rev-parse", "main"]);
    sandbox.commit(&repo, "refused-by-own");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(
        !output.status.success(),
        "the own pre-push refused: {stderr}"
    );
    assert!(stderr.contains("own pre-push: no"), "{stderr}");
    assert!(!stderr.contains("It is pushed"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), landed);
    fs::remove_file(hooks.join("pre-push")).unwrap();

    // A hook that is not executable is skipped, with git's own hint.
    fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(sandbox.dir.join("fail"), "").unwrap();
    fs::write(repo.join("three"), "three").unwrap();
    sandbox.git(&repo, &["add", "three"]);
    let output = sandbox.git_out(&repo, &["commit", "-q", "-m", "three"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("hook was ignored because it's not set as executable"),
        "{stderr}"
    );
}

#[test]
fn hook_arguments_reach_the_own_hook_unchanged() {
    let sandbox = Sandbox::new("args");
    let (repo, _) = sandbox.repo("acme", "args", &[]);
    write_executable(
        &repo.join(".git/hooks/pre-rebase"),
        "#!/bin/sh\nfor a in \"$@\"; do echo \"arg[$a]\"; done\nexit 7\n",
    );
    // Arguments that look like ohjaa's own flags are the hook's, not ohjaa's.
    let output = sandbox.ohjaa_in(
        &repo,
        &["hook", "pre-rebase", "--root", "-x", "two words", "--"],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(
        output.status.code(),
        Some(7),
        "the own hook's status: {stderr}"
    );
    assert_eq!(stdout, "arg[--root]\narg[-x]\narg[two words]\narg[--]\n");

    let output = sandbox.ohjaa_in(&repo, &["hook", "not-a-hook"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).1.contains("is not a git hook"));
}

#[test]
fn doctor_checks_every_gated_clone_and_ohjaa_run_warns_in_one() {
    let sandbox = Sandbox::new("doctor");
    let root = sandbox.projects();
    let (gated, _) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own-hooks", &[]);
    sandbox.git(&own, &["config", "core.hooksPath", ".githooks"]);
    sandbox.repo("acme", "older", &[("sykli.json", "{}\n")]);
    sandbox.repo("acme", "plain", &[]);

    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}{stderr}");
    for said in [
        "ok     git 2.",
        "DRIFT  git's global config does not include",
        "ok     git's global core.hooksPath is unset",
        "/git/config is missing: nothing is observed (ohjaa apply)",
        "lacks the ohjaa block (.sykli/ .teko/)",
        "does not load the launcher",
        &format!("roots: {root} (given with --root)"),
        "note   older: an older in-repo sykli.json and no entry",
        "note   gated: an older in-repo sykli.json and no entry",
        "not observed (no entry; git never runs ohjaa there): own-hooks, plain",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    sandbox.apply();
    sandbox.adopt(&gated);
    // No --root: the roots file apply wrote.
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let hooks = sandbox.entry_hooks("acme", "gated");
    for said in [
        "ok     git's global config includes",
        "/git/config is current",
        "/repos/acme/gated/gated.config is current",
        &format!(
            "ok     acme/gated: dispatchers (pre-push) exec {}",
            sandbox.dir.join("bin/ohjaa").display()
        ),
        &format!(
            "roots: {root} (from {})",
            sandbox.config().join("roots").display()
        ),
        &format!(
            "ok     gated (acme/gated): observed; git runs {} here",
            hooks.display()
        ),
        "ohjaa doctor: healthy",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    // (a) An entry for a clone whose own core.hooksPath wins.
    sandbox.entry("acme", "own-hooks", Some("{}\n"));
    sandbox.apply();
    // (b) A hook of its own that has no dispatcher: git would skip it.
    write_executable(&gated.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    let output = sandbox.ohjaa(&["doctor", "--root", &root]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    for said in [
        "DRIFT  own-hooks (acme/own-hooks): its own core.hooksPath (.githooks) overrides \
         ohjaa's: its pushes are not observed here: the repo's own hooks run",
        &format!(
            "DRIFT  gated (acme/gated): its own pre-commit hook has no dispatcher in {}, so git \
             skips it (ohjaa apply)",
            hooks.display()
        ),
        &format!(
            "DRIFT  acme/gated: dispatchers missing in {}: pre-commit (ohjaa apply)",
            hooks.display()
        ),
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }

    // ohjaa run says the same at launch, in the repository it starts in.
    let output = sandbox.ohjaa_in(&gated, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains("ohjaa: warning: ")
            && stderr.contains("its own pre-commit hook has no dispatcher"),
        "{stderr}"
    );

    // The dispatchers exec an ohjaa that is gone, and hand the gate to a
    // sykli that is gone.
    sandbox.apply();
    fs::remove_file(sandbox.dir.join("bin/ohjaa")).unwrap();
    fs::remove_file(sandbox.dir.join("bin/sykli")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ohjaa"))
        .env_clear()
        .envs(
            sandbox
                .command("x", &sandbox.dir)
                .get_envs()
                .filter_map(|(k, v)| Some((k, v?))),
        )
        .current_dir(&sandbox.dir)
        .args(["doctor", "--root", &root])
        .output()
        .unwrap();
    let (stdout, _) = text(&output);
    for said in [
        "which is gone: pushes in its clones fail, and so do commits in clones that chain a \
         hook of their own",
        "the pre-push dispatcher hands pushes to",
        "every push to the entry's branch is flagged unproven, never judged",
    ] {
        assert!(stdout.contains(said), "missing {said:?} in:\n{stdout}");
    }
}

/// B2: a hook whose mere presence changes what git does is never dispatched,
/// so a gated checkout that takes pushes with updateInstead still updates.
#[test]
fn a_gated_checkout_with_update_instead_still_updates_its_worktree() {
    let sandbox = Sandbox::new("update-instead");
    let (target, _) = sandbox.repo("acme", "target", &[("f", "v1\n")]);
    let (deploy, _) = sandbox.repo("acme", "deploy", &[]);
    sandbox.git(
        &target,
        &["config", "receive.denyCurrentBranch", "updateInstead"],
    );
    // Another gated clone has its own push-to-checkout: it gets no dispatcher.
    write_executable(
        &deploy.join(".git/hooks/push-to-checkout"),
        "#!/bin/sh\nexit 0\n",
    );
    sandbox.entry("acme", "target", Some("{}\n"));
    sandbox.entry("acme", "deploy", Some("{}\n"));
    sandbox.apply();
    assert!(sandbox.gated(&target));
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "target")),
        ["pre-push"]
    );
    assert_eq!(
        names_in(&sandbox.entry_hooks("acme", "deploy")),
        ["pre-push"]
    );

    let work = sandbox.dir.join("work");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &target.to_string_lossy(), "work"],
    );
    fs::write(work.join("f"), "v2\n").unwrap();
    sandbox.git(&work, &["commit", "-q", "-am", "v2"]);
    sandbox.git(&work, &["push", "-q", "origin", "main"]);
    assert_eq!(
        fs::read_to_string(target.join("f")).unwrap(),
        "v2\n",
        "worktree updated"
    );
    assert_eq!(sandbox.git(&target, &["status", "--porcelain"]), "");

    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert!(
        stdout.contains(
            "deploy (acme/deploy): its own push-to-checkout cannot run under ohjaa's hooks folder"
        ),
        "{stdout}"
    );
}

/// B3: the entry's branch is the observed one, whatever origin/HEAD says.
#[test]
fn the_entry_branch_is_observed_whatever_origin_head_says() {
    let mut sandbox = Sandbox::new("branch");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    // origin/HEAD at develop, entry main.
    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    sandbox.git(&repo, &["remote", "set-head", "origin", "develop"]);
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main ("),
        "{stderr}"
    );
    assert!(sandbox.read("sykli-calls").contains("--branch main origin"));
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/develop"]);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);

    // Entry trunk2: trunk2 is observed, and main no longer is.
    fs::write(
        sandbox.config().join("repos/acme/widget/repo.toml"),
        "branch = \"trunk2\"\n",
    )
    .unwrap();
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/trunk2"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to trunk2 ("),
        "{stderr}"
    );
    assert!(sandbox
        .read("sykli-calls")
        .contains("--branch trunk2 origin"));
    sandbox.commit(&repo, "b");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success());
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
}

/// B3: a sykli that takes --contract but not --branch cannot judge the
/// entry's branch, so a push to it is flagged, never said to be proven.
#[test]
fn a_sykli_without_branch_flags_the_entry_branch_and_passes_others_quietly() {
    let mut sandbox = Sandbox::new("nobranch");
    sandbox.stub_contract = "nobranch";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "HEAD:refs/heads/feature"]);
    assert!(output.status.success());
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (") && stderr.contains("names no --branch"),
        "{stderr}"
    );
    assert!(!stderr.contains("no --contract"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// Should-fix 4: an entry means the repository is meant to be observed.
#[test]
fn an_entry_without_a_gate_flags_its_branch() {
    let mut sandbox = Sandbox::new("no-gate");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", None);
    sandbox.apply();
    let head = sandbox.commit(&repo, "a");
    sandbox.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (its entry has no gate: "),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// Should-fix 1: `ohjaa gate` runs the gate a push is judged by, and says so.
#[test]
fn ohjaa_gate_runs_the_entry_gate_and_prints_it() {
    let mut sandbox = Sandbox::new("gate-command");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    fs::create_dir_all(repo.join("src")).unwrap();

    let output = sandbox.ohjaa_in(&repo.join("src"), &["gate", "--print"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "ohjaa gate: the gate is {} (the entry for acme/widget (branch main))",
            contract.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "cd '{}' && mkdir -p .sykli && sykli run '{c}' --json > .sykli/receipt.json && \
         sykli verify .sykli/receipt.json --contract '{c}' --commit HEAD",
            repo.display(),
            c = contract.display()
        )),
        "{stdout}"
    );
    assert!(!repo.join(".sykli").exists(), "--print runs nothing");

    let head = sandbox.commit(&repo, "a");
    let output = sandbox.ohjaa_in(&repo.join("src"), &["gate"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains(&format!(
        "$ sykli run '{}' --json > .sykli/receipt.json",
        contract.display()
    )));
    assert!(
        stdout.contains(
            "ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven"
        ),
        "{stdout}"
    );
    assert!(sandbox
        .read("projects/widget/.sykli/receipt.json")
        .contains("passed"));
    assert!(sandbox.read("sykli-calls").contains(&format!(
        "{}|run {} --json",
        repo.display(),
        contract.display()
    )));
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);

    fs::write(sandbox.dir.join("fail-gate"), "").unwrap();
    let output = sandbox.ohjaa_in(&repo, &["gate"]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("ohjaa gate: failed: sykli run exited 1"),
        "{stdout}"
    );

    // No entry: the repository's own sykli.json; neither: no gate.
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    let output = sandbox.ohjaa_in(&own, &["gate", "--print"]);
    assert!(text(&output).0.contains(&format!(
        "the gate is {} (the repository's own sykli.json (no entry))",
        own.join("sykli.json").display()
    )));
    let (none, _) = sandbox.repo("acme", "none", &[]);
    let output = sandbox.ohjaa_in(&none, &["gate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output).1.contains("has no gate"));
}

/// A receipt of `tree` that says `outcome`, written as sykli writes one.
fn receipt(path: &Path, tree: &str, outcome: &str, at_ms: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{{\"schema\":\"sykli-receipt.v1\",\"outcome\":\"{outcome}\",\"started_at_ms\":{at_ms},\
             \"subject\":{{\"tree_oid\":\"{tree}\",\"head_tree_oid\":\"{tree}\",\"dirty\":false}}}}\n"
        ),
    )
    .unwrap();
}

/// A receipt of a run that passed under another contract than the gate's.
fn receipt_under_another_contract(path: &Path, tree: &str, at_ms: u64) {
    receipt(path, tree, "passed", at_ms);
    let text = fs::read_to_string(path).unwrap();
    let text = text.replacen(
        "\"schema\"",
        "\"contract_hash\":\"other-contract\",\"schema\"",
        1,
    );
    fs::write(path, text).unwrap();
}

fn short(commit: &str) -> &str {
    &commit[..7]
}

/// `text` with every `YYYY-MM-DD HH:MM UTC` as `<time>`.
fn blank_times(text: &str) -> String {
    let shape: Vec<char> = "dddd-dd-dd dd:dd UTC".chars().collect();
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        let fits = at + shape.len() <= chars.len()
            && shape.iter().zip(&chars[at..]).all(|(want, got)| {
                if *want == 'd' {
                    got.is_ascii_digit()
                } else {
                    want == got
                }
            });
        if fits {
            out.push_str("<time>");
            at += shape.len();
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

impl Sandbox {
    fn tree(&self, repo: &Path, rev: &str) -> String {
        self.git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
    }

    /// `ohjaa report` over the projects folder, as text, its times blanked.
    fn report(&self, cwd: &Path) -> String {
        let output = self.ohjaa_in(cwd, &["report", "--root", &self.projects()]);
        blank_times(&text(&output).0)
    }

    /// `ohjaa brief` here, its times blanked.
    fn brief(&self, cwd: &Path) -> String {
        let output = self.ohjaa_in(cwd, &["brief"]);
        blank_times(&text(&output).0)
    }

    /// `ohjaa report --json` over the projects folder.
    fn report_json(&self, cwd: &Path) -> serde_json::Value {
        let output = self.ohjaa_in(cwd, &["report", "--json", "--root", &self.projects()]);
        let (stdout, stderr) = text(&output);
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"))
    }

    /// The `sykli verify … --commit` calls so far.
    fn verifies(&self) -> usize {
        self.read("sykli-calls")
            .lines()
            .filter(|line| line.contains("|verify ") && line.contains("--commit"))
            .count()
    }
}

/// The review of #45 against sykli main: what `sykli verify --help` says is
/// read for its flags, never its sentences (sykli#79 reworded the one ohjaa
/// matched, and every proven commit became "sykli could not say"). A sykli
/// that names `--commit` judges the commit alone, whatever else it says; one
/// that also names `--json` is asked for its `sykli-verify.v1` verdict, and
/// a commit is proven only where that verified the commit and established
/// the receipt's identity: the schema, the contract, the source (the
/// commit's tree and inputs) and the records' consistency. Its outcome is
/// the receipt's claim (receipts are unsigned), read from the verdict as
/// claimed, whether sykli lists the outcome as established (sykli main
/// today) or not (as sykli is moving to).
#[test]
fn a_commit_is_proven_by_sykli_verifys_flags_and_its_json_verdict() {
    let mut sandbox = Sandbox::new("verify-flags");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let c1 = sandbox.commit(&repo, "one");
    let t1 = sandbox.tree(&repo, "HEAD");
    receipt(
        &repo.join(".git/sykli/receipts/rcpt_1.json"),
        &t1,
        "passed",
        1_000,
    );
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    let proven = format!(
        "- acme/widget main (origin/main as of a push at <time>): ✓ {}\n",
        short(&c1)
    );
    let verify_calls = |sandbox: &Sandbox| -> Vec<String> {
        sandbox
            .read("sykli-calls")
            .lines()
            .filter(|line| line.contains("|verify ") && line.contains("--commit"))
            .map(str::to_owned)
            .collect()
    };

    // Its exit code says: no --json.
    let report = sandbox.report(&repo);
    assert!(report.contains(&proven), "{report}");
    assert!(
        verify_calls(&sandbox)
            .iter()
            .all(|call| !call.contains("--json")),
        "{:?}",
        verify_calls(&sandbox)
    );

    // Its verdict says: asked with --json.
    fs::write(sandbox.dir.join("verify-json"), "").unwrap();
    let report = sandbox.report(&repo);
    assert!(report.contains(&proven), "{report}");
    let last = verify_calls(&sandbox).pop().unwrap();
    assert!(last.ends_with(&format!("--commit {c1} --json")), "{last}");

    // sykli as it is moving to: the outcome is not established, it is the
    // receipt's claim. Proven all the same, as claimed.
    fs::write(
        sandbox.dir.join("verify-established"),
        r#""schema","contract","source","consistency""#,
    )
    .unwrap();
    fs::write(
        sandbox.dir.join("verify-not-established"),
        r#""outcome","provenance","producer","execution""#,
    )
    .unwrap();
    let report = sandbox.report(&repo);
    assert!(report.contains(&proven), "{report}");

    // Up to here the claim was read from the outcome check's words (no
    // `claimed_outcome`: sykli up to 5acdf91). A sykli with the field is
    // read by it alone, whatever the check's words say.
    fs::write(sandbox.dir.join("verify-claimed-outcome"), "passed").unwrap();
    let report = sandbox.report(&repo);
    assert!(report.contains(&proven), "{report}");
    fs::write(sandbox.dir.join("verify-claimed-outcome"), "failed").unwrap();
    let json = sandbox.report_json(&repo);
    let commit = &json["entries"][0]["commits"][0];
    assert_eq!(commit["code"], "could_not_say", "{json:#}");
    assert!(
        commit["why"].as_str().unwrap().contains("claims failed"),
        "{json:#}"
    );
    fs::remove_file(sandbox.dir.join("verify-claimed-outcome")).unwrap();

    // Without the contract, either way: not proven, and said why.
    fs::write(
        sandbox.dir.join("verify-established"),
        r#""schema","source","consistency""#,
    )
    .unwrap();
    let json = sandbox.report_json(&repo);
    let commit = &json["entries"][0]["commits"][0];
    assert_eq!(commit["code"], "could_not_say", "{json:#}");
    assert!(
        commit["why"].as_str().unwrap().contains("contract"),
        "{json:#}"
    );
    fs::remove_file(sandbox.dir.join("verify-not-established")).unwrap();

    // Verified, but without the source: not proven, and said why.
    fs::write(
        sandbox.dir.join("verify-established"),
        r#""schema","contract","consistency","outcome""#,
    )
    .unwrap();
    let json = sandbox.report_json(&repo);
    let commit = &json["entries"][0]["commits"][0];
    assert_eq!(commit["code"], "could_not_say", "{json:#}");
    assert!(
        commit["why"].as_str().unwrap().contains("source"),
        "{json:#}"
    );

    // A verdict in a schema this ohjaa does not read proves nothing.
    fs::remove_file(sandbox.dir.join("verify-established")).unwrap();
    fs::write(sandbox.dir.join("verify-schema"), "sykli-verify.v9").unwrap();
    let json = sandbox.report_json(&repo);
    let commit = &json["entries"][0]["commits"][0];
    assert_eq!(commit["code"], "could_not_say", "{json:#}");
    assert!(
        commit["why"].as_str().unwrap().contains("sykli-verify.v9"),
        "{json:#}"
    );
}

/// The marks: ✓ where a receipt of the commit's tree verifies, ✗ where one is
/// there and did not pass, ? where none is; tip first, down to the first
/// proven commit; sykli asked only where a receipt says it passed. A merge
/// the forge made, with the tree of a proven pull request head, is proven.
#[test]
fn marks_are_derived_from_receipts_tip_first() {
    let mut sandbox = Sandbox::new("marks");
    sandbox.stub_contract = "fast";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let common = repo.join(".git");
    // Proven: the receipt sykli kept for an accepted push.
    let c1 = sandbox.commit(&repo, "one");
    let t1 = sandbox.tree(&repo, "HEAD");
    receipt(
        &common.join("sykli/receipts/rcpt_1.json"),
        &t1,
        "passed",
        1_000,
    );
    // Failed: its run in this worktree.
    let c2 = sandbox.commit(&repo, "two");
    let t2 = sandbox.tree(&repo, "HEAD");
    receipt(
        &repo.join(".sykli/receipts/rcpt_2.json"),
        &t2,
        "failed",
        2_000,
    );
    // Never run.
    let c3 = sandbox.commit(&repo, "three");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    assert!(output.status.success(), "{}", text(&output).1);
    let before = sandbox.verifies();

    let report = sandbox.report(&repo);
    let line = format!(
        "- acme/widget main (origin/main as of a push at <time>): ? {} ✗ {} ✓ {}\n",
        short(&c3),
        short(&c2),
        short(&c1)
    );
    assert!(
        report.contains(
            "Marks on each entry's branch, tip first (✓ proven · ✗ failed · ? unproven):\n"
        ) && report.contains(&line),
        "{report}"
    );
    assert_eq!(
        sandbox.verifies() - before,
        1,
        "sykli asked only about the one receipt that says it passed"
    );
    let calls = sandbox.read("sykli-calls");
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        calls.contains(&format!(
            "|verify {} --contract {} --repo-root {} --git-common-dir {} --commit {c1}",
            common.join("sykli/receipts/rcpt_1.json").display(),
            contract.display(),
            repo.display(),
            common.display()
        )),
        "{calls}"
    );

    let json = sandbox.report_json(&repo);
    assert_eq!(json["schema"], "ohjaa-marks.v1");
    let entry = &json["entries"][0];
    assert_eq!(entry["repo"], "acme/widget", "{json:#}");
    assert_eq!(entry["branch"], "main");
    assert_eq!(entry["ended"], "proven");
    assert_eq!(entry["fetched"], serde_json::Value::Null);
    let marks: Vec<(&str, &str)> = entry["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["sha"].as_str().unwrap(), c["mark"].as_str().unwrap()))
        .collect();
    assert_eq!(
        marks,
        [
            (c3.as_str(), "unproven"),
            (c2.as_str(), "failed"),
            (c1.as_str(), "proven")
        ]
    );
    assert_eq!(entry["commits"][1]["tree"], t2.as_str());
    assert_eq!(entry["commits"][1]["author"], "Ohjaa Test");
    assert_eq!(entry["commits"][1]["subject"], "two");

    // A pull request proven in a linked worktree (`ohjaa gate`'s receipt),
    // merged on the forge.
    let linked = sandbox.dir.join("pr");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.commit(&linked, "four");
    receipt(
        &linked.join(".sykli/receipt.json"),
        &sandbox.tree(&linked, "HEAD"),
        "passed",
        4_000,
    );
    sandbox.git(&linked, &["push", "-q", "origin", "pr"]);
    let forge = sandbox.dir.join("forge");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "forge"],
    );
    sandbox.git(
        &forge,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "origin/pr",
        ],
    );
    sandbox.git(&forge, &["push", "-q", "origin", "main"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    let merge = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    assert_ne!(merge, head);
    assert_eq!(
        sandbox.tree(&repo, &merge),
        sandbox.tree(&repo, &head),
        "the forge's merge has its head's tree"
    );
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a fetch at <time>): ✓ {}\n",
            short(&merge)
        )),
        "{report}"
    );
}

/// The briefing opens with the flag while the entry's branch is not proven
/// at its tip; `ohjaa gate` there proves it, and the flag is gone.
#[test]
fn the_briefing_opens_with_the_flag_and_ohjaa_gate_flips_the_tip() {
    let mut sandbox = Sandbox::new("flag");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let c1 = sandbox.commit(&repo, "one");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c2 = sandbox.commit(&repo, "two");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let stdout = sandbox.brief(&repo);
    let never = format!(
        "FLAG: main of acme/widget has never been proven (3 commits: ? {}, ? {}, ? {}; \
         origin/main as of a push at <time>). Start by running `ohjaa gate` on main and fix \
         forward what fails.\n",
        short(&c2),
        short(&c1),
        short(&init)
    );
    assert!(stdout.starts_with(&never), "{stdout}");

    // Proven below: unproven since the commit above it, one sykli verify.
    receipt(
        &repo.join(".git/sykli/receipts/rcpt_1.json"),
        &sandbox.tree(&repo, &c1),
        "passed",
        1_000,
    );
    let before = sandbox.verifies();
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "x", "--dry-run"]);
    let stderr = blank_times(&text(&output).1);
    let since = format!(
        "FLAG: main of acme/widget is unproven since {c} (1 commit: ? {c}; origin/main as of \
         a push at <time>). Start by running `ohjaa gate` on main and fix forward what fails.",
        c = short(&c2)
    );
    assert!(stderr.contains(&format!("ohjaa: {since}\n")), "{stderr}");
    let would = stderr
        .lines()
        .find(|line| line.starts_with("ohjaa: dry-run; would run: "))
        .unwrap_or_else(|| panic!("{stderr}"));
    let briefing = would
        .split_once("--briefing ")
        .map(|(_, rest)| rest.trim_start_matches('\''))
        .unwrap_or_else(|| panic!("{would}"));
    assert!(
        briefing.starts_with(&since),
        "the flag comes first: {would}"
    );
    assert_eq!(sandbox.verifies() - before, 1, "one sykli verify");

    // `ohjaa gate` on main, at origin/main's tip: proven, and no flag.
    fs::write(sandbox.dir.join("tree"), sandbox.tree(&repo, "HEAD")).unwrap();
    let output = sandbox.ohjaa_in(&repo, &["gate"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).0);
    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    let (stdout, _) = text(&output);
    assert!(!stdout.contains("FLAG:"), "{stdout}");
    assert!(
        sandbox.report(&repo).contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ✓ {}\n",
            short(&c2)
        )),
        "{}",
        sandbox.report(&repo)
    );

    // No entry, no flag, and no git asked for one.
    let (plain, _) = sandbox.repo("acme", "plain", &[]);
    let output = sandbox.ohjaa_in(&plain, &["brief"]);
    assert!(!text(&output).0.contains("FLAG:"), "{}", text(&output).0);
}

/// B1: a run that passed under another contract than the gate's now (the
/// gate was edited and re-locked, or the run used the repository's own
/// sykli.json) is not proven against the gate, and never "failed": ✗ is
/// only for a run that failed.
#[test]
fn a_run_that_passed_under_another_contract_is_unproven_not_failed() {
    let mut sandbox = Sandbox::new("other-contract");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let c1 = sandbox.commit(&repo, "one");
    receipt_under_another_contract(
        &repo.join(".git/sykli/receipts/rcpt_1.json"),
        &sandbox.tree(&repo, "HEAD"),
        1_000,
    );
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ? {} ? {} (nothing on it \
             is proven)\n",
            short(&c1),
            short(&init)
        )),
        "{report}"
    );
    assert!(
        !report
            .lines()
            .any(|line| line.starts_with("- acme/widget main") && line.contains('✗')),
        "{report}"
    );
    let json = sandbox.report_json(&repo);
    let tip = &json["entries"][0]["commits"][0];
    assert_eq!(tip["mark"], "unproven", "{json:#}");
    assert_eq!(tip["code"], "other_contract", "{json:#}");
    assert_eq!(
        tip["why"],
        "passed under another contract than the gate's (refused: contract expected 1111 but \
         got 2222)"
    );
    assert_eq!(json["entries"][0]["commits"][1]["code"], "no_receipt");
    assert_eq!(
        json["entries"][0]["commits"][1]["why"],
        "no receipt of its tree here"
    );
    let stdout = sandbox.brief(&repo);
    assert!(
        stdout.starts_with(&format!(
            "FLAG: main of acme/widget has never been proven against its current gate (2 \
             commits: ? {}, ? {}; 1 passed under another contract; origin/main as of a push at \
             <time>). Start by running `ohjaa gate` on main and fix forward what fails.\n",
            short(&c1),
            short(&init)
        )),
        "{stdout}"
    );

    // A run that failed is ✗, still.
    let c2 = sandbox.commit(&repo, "two");
    receipt(
        &repo.join(".sykli/receipts/rcpt_2.json"),
        &sandbox.tree(&repo, "HEAD"),
        "failed",
        2_000,
    );
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            ": ✗ {} ? {} ? {} (nothing on it is proven)\n",
            short(&c2),
            short(&c1),
            short(&init)
        )),
        "{report}"
    );
}

/// Should-fix 1: nothing the workflow does refuses a push, not even a
/// workflow home that cannot be found (no HOME, no XDG_CONFIG_HOME).
#[test]
fn a_push_without_a_home_lands_flagged_unobserved() {
    let mut sandbox = Sandbox::new("no-home");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let head = sandbox.commit(&repo, "a");
    let output = sandbox
        .command("git", &repo)
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args(["push", "origin", "main"])
        .output()
        .unwrap();
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr
            .contains("ohjaa: UNOBSERVED push to acme/widget: the workflow home cannot be found (")
            && stderr.contains("). It is pushed"),
        "{stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// Should-fix 2: the push and the marks agree. A commit proven in another
/// worktree of the clone (its receipt there) is a quiet push from this one,
/// asked of the sykli `apply` recorded, not of PATH's; one proven nowhere is
/// flagged.
#[test]
fn a_commit_proven_in_another_worktree_is_a_quiet_push() {
    let mut sandbox = Sandbox::new("other-worktree");
    sandbox.stub_contract = "fast";
    let (repo, bare) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let linked = sandbox.dir.join("linked");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "work",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.commit(&linked, "proven-there");
    receipt(
        &linked.join(".sykli/receipt.json"),
        &sandbox.tree(&linked, "HEAD"),
        "passed",
        1_000,
    );
    sandbox.git(&repo, &["merge", "-q", "--ff-only", "work"]);
    // A client without your PATH: only the sykli the dispatcher names.
    let push = |sandbox: &Sandbox| {
        sandbox
            .command("/usr/bin/git", &repo)
            .env("PATH", "/usr/bin:/bin")
            .args(["push", "origin", "main"])
            .output()
            .unwrap()
    };
    let output = push(&sandbox);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        !stderr.contains("ohjaa:"),
        "proven in another worktree: quiet: {stderr}"
    );
    let calls = sandbox.read("sykli-calls");
    assert!(
        calls.contains(&format!(
            "|verify {} --contract ",
            linked.join(".sykli/receipt.json").display()
        )) && calls.contains(&format!("--commit {head}")),
        "{calls}"
    );
    assert!(
        sandbox
            .report(&repo)
            .contains(&format!(": ✓ {}\n", short(&head))),
        "the marks say the same"
    );

    let next = sandbox.commit(&repo, "nowhere");
    let output = push(&sandbox);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), next);
    assert!(
        stderr.contains(
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree)"
        ),
        "proven nowhere: flagged: {stderr}"
    );
}

// ---- KATANA D4: validation references ----------------------------------------

/// A receipt of `tree` whose tasks ended as `tasks` say (`passed`, `cached`,
/// `failed`), the run as `outcome`, written as sykli writes one.
fn receipt_with_tasks(path: &Path, tree: &str, outcome: &str, tasks: &[&str], at_ms: u64) {
    let tasks: Vec<String> = tasks
        .iter()
        .enumerate()
        .map(|(n, outcome)| format!("{{\"name\":\"t{n}\",\"outcome\":\"{outcome}\"}}"))
        .collect();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{{\"schema\":\"sykli-receipt.v1\",\"outcome\":\"{outcome}\",\"started_at_ms\":{at_ms},\
             \"subject\":{{\"tree_oid\":\"{tree}\",\"head_tree_oid\":\"{tree}\",\"dirty\":false}},\
             \"tasks\":[{}]}}\n",
            tasks.join(",")
        ),
    )
    .unwrap();
}

impl Sandbox {
    /// `ohjaa usage export` (ohjaa-sessions.v1), read from `cwd`.
    fn export(&self, cwd: &Path) -> serde_json::Value {
        let output = self.ohjaa_in(cwd, &["usage", "export"]);
        let (stdout, stderr) = text(&output);
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"))
    }

    /// One `ohjaa run` session in `repo` whose agent runs `script`, with
    /// sykli saving the record it finds at dispatch.
    fn session_running(&self, repo: &Path, script: &str) -> (String, String) {
        let output = self
            .command("ohjaa", repo)
            .args([
                "run",
                "sh",
                "--intent",
                "prove it",
                "--no-briefing",
                "--",
                "-c",
            ])
            .arg(script)
            .env("STUB_SYKLI_CAPTURE", "1")
            .output()
            .unwrap();
        let (stdout, stderr) = text(&output);
        assert!(output.status.success(), "{stdout}{stderr}");
        (stdout, blank_times(&stderr))
    }

    /// How many times `caller` (its SYKLI_CALLER) asked sykli about `run`
    /// (`status --run <run>`), or about any run with `run` empty.
    fn status_asks(&self, caller: &str, run: &str) -> usize {
        self.read("status-calls")
            .lines()
            .filter(|line| {
                line.strip_prefix(&format!("{caller}|status --run {run}"))
                    .is_some()
            })
            .count()
    }

    /// An entry whose gate's lock pins it as `pin`.
    fn locked_entry(&self, owner: &str, name: &str, pin: &str) -> PathBuf {
        self.entry(owner, name, Some("{}\n"));
        let entry = self.config().join("repos").join(owner).join(name);
        fs::write(
            entry.join("sykli.lock"),
            format!(
                "{{\"schema\":\"sykli-lock.v2\",\"contracts\":{{\"sykli.json\":\"{pin}\"}}}}\n"
            ),
        )
        .unwrap();
        entry
    }
}

/// The validations a session's record holds, from an export.
fn validations(export: &serde_json::Value, session: usize) -> Vec<serde_json::Value> {
    export["sessions"][session]["validations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// D4: `ohjaa gate` inside a session writes its request into the session's
/// record before it asks sykli, then what sykli answered: the receipt it
/// wrote, by path and digest. A reference, never an outcome.
#[test]
fn ohjaa_gate_records_its_request_before_asking_sykli() {
    let mut sandbox = Sandbox::new("gate-ref");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    let entry = sandbox.locked_entry("acme", "widget", "c0ffee");
    sandbox.apply();
    // A commit not on origin/main yet: a candidate.
    let head = sandbox.commit(&repo, "change");
    let tree = sandbox.tree(&repo, "HEAD");
    fs::write(sandbox.dir.join("tree"), &tree).unwrap();

    let (stdout, _) = sandbox.session_running(&repo, "ohjaa gate");
    assert!(stdout.contains("ohjaa gate: passed"), "{stdout}");
    let export = sandbox.export(&repo);
    let id = export["sessions"][0]["id"].as_str().unwrap().to_owned();
    let refs = validations(&export, 0);
    assert_eq!(refs.len(), 1, "{export:#}");
    let found = &refs[0];
    let request = found["request"].as_str().unwrap().to_owned();
    let request = request.as_str();
    assert!(request.starts_with("rq-"), "{found:#}");
    assert!(
        stdout.contains(&format!(
            "ohjaa gate: request {request} recorded in session {id} before asking sykli"
        )),
        "{stdout}"
    );
    assert_eq!(found["kind"], "candidate");
    assert_eq!(found["commit"], head.as_str());
    assert_eq!(found["tree"], tree.as_str());
    assert_eq!(
        found["contract"],
        entry.join("sykli.json").to_str().unwrap()
    );
    assert_eq!(
        found["contract_digest"], "c0ffee",
        "the pin in the gate's lock"
    );
    assert_eq!(
        found["lock_digest"], "10c4",
        "sykli's word, from its receipt"
    );
    assert!(
        found.get("run").is_none(),
        "this sykli names no run: {found:#}"
    );
    let receipt = found["receipt"]["path"].as_str().unwrap();
    let digest = found["receipt"]["digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64, "{found:#}");
    assert!(
        receipt.ends_with(&format!(".sykli/receipts/rcpt_{digest}.json")),
        "{found:#}"
    );
    for key in ["outcome", "state", "verdict", "check"] {
        assert!(
            found.get(key).is_none(),
            "never an outcome: {key}: {found:#}"
        );
    }
    // At dispatch the record already held the request, and nothing sykli said.
    let at: serde_json::Value = serde_json::from_str(&sandbox.read("at-dispatch.json"))
        .unwrap_or_else(|e| panic!("{e}: {}", sandbox.read("at-dispatch.json")));
    let then = &validations(&at, 0)[0];
    assert_eq!(then["request"], request);
    assert!(
        then.get("receipt").is_none() && then.get("lock_digest").is_none(),
        "{then:#}"
    );

    // The next briefing names it in the session's observed part, before
    // anything the agent testified.
    let brief = sandbox.brief(&repo);
    let line = format!(
        "- validation {request} (candidate, {} tree {}): no sykli run id; receipt {receipt} \
         (sha256 {})",
        short(&head),
        short(&tree),
        &digest[..12]
    );
    let at_line = brief
        .find(&line)
        .unwrap_or_else(|| panic!("{line}\n{brief}"));
    assert!(
        at_line < brief.find("- it left no handover").unwrap(),
        "{brief}"
    );

    // Outside a session nothing is recorded, and that is said.
    let output = sandbox.ohjaa_in(&repo, &["gate"]);
    assert!(
        text(&output).0.contains(
            "ohjaa gate: not inside an ohjaa session (OHJAA_SESSION): this request is not \
             recorded"
        ),
        "{}",
        text(&output).0
    );

    // Once it is origin/main's tip, as this clone saw it: integrated.
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    sandbox.session_running(&repo, "ohjaa gate");
    let export = sandbox.export(&repo);
    let integrated: Vec<serde_json::Value> = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["validations"].as_array().cloned().unwrap_or_default())
        .filter(|v| v["request"] != request)
        .collect();
    assert_eq!(integrated.len(), 1, "{export:#}");
    assert_eq!(integrated[0]["kind"], "integrated", "{export:#}");
    assert_eq!(integrated[0]["commit"], head.as_str());
    assert_eq!(
        integrated[0]["ran"],
        serde_json::json!({"tree": tree, "dirty": false}),
        "what sykli says ran: {export:#}"
    );
    assert_eq!(integrated[0]["observed_branch"]["name"], "main");
    assert_eq!(integrated[0]["observed_branch"]["tip"], head.as_str());
    assert!(
        integrated[0]["observed_branch"]["updated_ms"].is_i64(),
        "when origin/main last moved here: {export:#}"
    );

    // Should-fix 3 of the review of #45: at the tip, but a run of
    // uncommitted changes: a candidate, never integrated, and what ran is
    // said.
    fs::write(sandbox.dir.join("dirty-tree"), "d1rty7ree").unwrap();
    let (_, stderr) = sandbox.session_running(&repo, "ohjaa gate");
    fs::remove_file(sandbox.dir.join("dirty-tree")).unwrap();
    let export = sandbox.export(&repo);
    let dirty = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["validations"].as_array().cloned().unwrap_or_default())
        .find(|v| v["ran"]["dirty"] == true)
        .unwrap_or_else(|| panic!("{export:#}"));
    assert_eq!(dirty["kind"], "candidate", "{dirty:#}");
    assert_eq!(dirty["commit"], head.as_str());
    assert_eq!(dirty["tree"], tree.as_str());
    assert_eq!(dirty["ran"]["tree"], "d1rty7ree");
    assert!(
        stderr.contains(&format!(
            "(candidate, {} tree {}; sykli ran tree d1rty7r, dirty)",
            short(&head),
            short(&tree)
        )),
        "{stderr}"
    );
}

/// D4 against the durable runs of D3: the gate records sykli's run id as
/// soon as sykli has the run, waits for it, and takes the receipt from
/// sykli's status. A run still going when its session ends is asked about at
/// leaving and in the next briefing, and said as sykli's answer: never
/// stored.
#[test]
fn a_durable_run_is_recorded_by_id_and_sykli_is_asked_about_it() {
    let mut sandbox = Sandbox::new("gate-run");
    sandbox.stub_contract = "fast";
    sandbox.stub_d3 = true;
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.locked_entry("acme", "widget", "c0ffee");
    sandbox.apply();
    let head = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let tree = sandbox.tree(&repo, "HEAD");
    fs::write(sandbox.dir.join("tree"), &tree).unwrap();
    fs::write(sandbox.dir.join("with-cached"), "").unwrap();

    let (stdout, stderr) = sandbox.session_running(&repo, "ohjaa gate");
    let found = validations(&sandbox.export(&repo), 0)[0].clone();
    let request = found["request"].as_str().unwrap().to_owned();
    let run = found["run"].as_str().expect("sykli's run id").to_owned();
    assert!(run.starts_with("run-"), "{found:#}");
    assert!(
        stdout.contains(&format!("ohjaa gate: sykli run {run} requested"))
            && stdout.contains("ohjaa gate: passed"),
        "{stdout}"
    );
    let receipt = found["receipt"]["path"].as_str().unwrap().to_owned();
    let digest = found["receipt"]["digest"].as_str().unwrap().to_owned();
    assert!(receipt.ends_with(&format!(".sykli/receipts/rcpt_{digest}.json")));
    let at: serde_json::Value = serde_json::from_str(&sandbox.read("at-dispatch.json")).unwrap();
    let then = &validations(&at, 0)[0];
    assert_eq!(then["request"], request.as_str());
    assert!(
        then.get("run").is_none(),
        "before sykli was asked: {then:#}"
    );
    // The gate said what sykli said of the run, as sykli's, cached apart.
    assert!(
        stdout.contains(&format!(
            "ohjaa gate: sykli says run {run} completed: 1 passed, 1 cached"
        )),
        "{stdout}"
    );
    // The record names the run and its receipt: nothing pending, so sykli
    // is not asked again at leaving.
    let recorded = format!(
        "validation {request} (integrated, {} tree {}): sykli run {run}; receipt {receipt} \
         (sha256 {})",
        short(&head),
        short(&tree),
        &digest[..12]
    );
    assert!(stderr.contains(&recorded), "{recorded}\n{stderr}");
    assert_eq!(
        sandbox.status_asks("", &run),
        1,
        "asked by the gate until the run ended, once here, and never at leaving"
    );

    // A run still going when its session ends.
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    let (_, stderr) =
        sandbox.session_running(&repo, "ohjaa gate & g=$!; sleep 2; kill -9 $g; exit 0");
    let export = sandbox.export(&repo);
    let pending = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["validations"].as_array().cloned().unwrap_or_default())
        .find(|v| v["request"] != request.as_str())
        .expect("the second request");
    let pending_run = pending["run"].as_str().expect("its run id").to_owned();
    assert!(pending.get("receipt").is_none(), "{pending:#}");
    assert!(
        stderr.contains(&format!(
            "sykli says run {pending_run} is running: 1 running, 1 pending"
        )),
        "{stderr}"
    );
    // The run finishes; the next briefing asks again, once.
    fs::remove_file(sandbox.dir.join("hold-runs")).unwrap();
    let (starts, output) = sandbox.git_starts(
        sandbox
            .command("ohjaa", &repo)
            .arg("brief")
            .env("SYKLI_CALLER", "brief"),
    );
    let brief = text(&output).0;
    assert!(
        brief.contains(&format!(
            "sykli says run {pending_run} completed: 1 passed, 1 cached"
        )),
        "{brief}"
    );
    assert_eq!(
        sandbox.status_asks("brief", &pending_run),
        1,
        "sykli is asked once per pending run"
    );
    // What sykli said is never stored.
    let stored = serde_json::to_string(&sandbox.export(&repo)).unwrap();
    assert!(
        !stored.contains("completed") && !stored.contains("\"running\""),
        "{stored}"
    );
    // Asking sykli adds no git process to the briefing.
    sandbox.stub_d3 = false;
    let (without, output) = sandbox.git_starts(sandbox.command("ohjaa", &repo).arg("brief"));
    assert!(
        text(&output)
            .0
            .contains("sykli cannot be asked about runs here (no `sykli status --run`)"),
        "{}",
        text(&output).0
    );
    assert_eq!(starts.len(), without.len(), "the same git processes");
}

impl Sandbox {
    /// A sandbox with a durable-run sykli (D3), `acme/widget` cloned with an
    /// entry whose lock pins its gate, and HEAD's tree as the runs' tree.
    fn durable(tag: &str) -> (Sandbox, PathBuf) {
        let mut sandbox = Sandbox::new(tag);
        sandbox.stub_contract = "fast";
        sandbox.stub_d3 = true;
        let (repo, _) = sandbox.repo("acme", "widget", &[]);
        sandbox.locked_entry("acme", "widget", "c0ffee");
        sandbox.apply();
        let tree = sandbox.tree(&repo, "HEAD");
        fs::write(sandbox.dir.join("tree"), &tree).unwrap();
        (sandbox, repo)
    }

    /// Every validation in the log, oldest session first.
    fn all_validations(&self, cwd: &Path) -> Vec<serde_json::Value> {
        self.export(cwd)["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["validations"].as_array().cloned().unwrap_or_default())
            .collect()
    }
}

/// Nit 5 and the D3 field analysis of the review of #45: a durable run still
/// going past `--wait`, or one sykli says ended in a state other than
/// completed, is not proven and not failed: exit 75, said as sykli says it,
/// and the run stays recorded for whoever asks next.
#[test]
fn a_durable_run_not_done_is_neither_proven_nor_failed() {
    let (sandbox, repo) = Sandbox::durable("not-done");
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    let (stdout, _) = sandbox.session_running(&repo, "ohjaa gate --wait 1; echo GATE-EXIT=$?");
    assert!(stdout.contains("GATE-EXIT=75"), "{stdout}");
    assert!(
        stdout.contains("is running: 1 running, 1 pending; still running after 1s, so not failed"),
        "{stdout}"
    );
    assert!(!stdout.contains("ohjaa gate: failed"), "{stdout}");
    let held = sandbox.all_validations(&repo);
    assert!(
        held[0]["run"].is_string() && held[0].get("receipt").is_none(),
        "{held:#?}"
    );

    // Ended, but not completed (D3 says interrupted; audit-sykli.md names
    // indeterminate): unknown, never failed.
    for state in ["interrupted", "indeterminate"] {
        fs::write(sandbox.dir.join("run-state"), state).unwrap();
        let (stdout, _) = sandbox.session_running(&repo, "ohjaa gate; echo GATE-EXIT=$?");
        assert!(stdout.contains("GATE-EXIT=75"), "{state}: {stdout}");
        assert!(
            stdout.contains(&format!(
                " {state}: 1 interrupted (not proven, not failed: unknown)"
            )),
            "{state}: {stdout}"
        );
        assert!(!stdout.contains("ohjaa gate: failed"), "{state}: {stdout}");
    }
}

/// Should-fix 4 of the review of #45: sykli keeps its runs in the clone's
/// common dir, so a pending run is asked about from any checkout of the
/// clone once the worktree it was requested in is removed.
#[test]
fn a_pending_run_is_asked_about_from_the_clone_once_its_worktree_is_gone() {
    let (sandbox, repo) = Sandbox::durable("wt-gone");
    let worktree = sandbox.dir.join("projects/widget-wt");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            &worktree.to_string_lossy(),
            "main",
        ],
    );
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    sandbox.session_running(&worktree, "ohjaa gate & g=$!; sleep 2; kill -9 $g; exit 0");
    let pending = sandbox.all_validations(&repo)[0].clone();
    let run = pending["run"].as_str().expect("a run id").to_owned();
    let session = sandbox.export(&repo)["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sandbox.git(
        &repo,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    assert!(!worktree.exists());
    let output = sandbox.ohjaa_in(&repo, &["session", &session]);
    let (stdout, stderr) = text(&output);
    assert!(
        stdout.contains(&format!("sykli says run {run} is running")),
        "{stdout}{stderr}"
    );
    // Asked from the main clone, handed the clone's facts.
    let asked = format!(
        "{}|status --run {run} --repo-root {} --git-common-dir {} --json",
        repo.display(),
        repo.display(),
        repo.join(".git").display()
    );
    assert!(
        sandbox.read("sykli-calls").contains(&asked),
        "{asked}\n{}",
        sandbox.read("sykli-calls")
    );
}

/// Should-fix 5 of the review of #45 (BRIEF §11): a pending validation
/// outlives the session that asked for it. Every briefing here names it, as
/// sykli says it now, after more sessions came and went, until sykli says it
/// ended.
#[test]
fn a_pending_run_is_named_in_briefings_until_sykli_says_it_ended() {
    let (sandbox, repo) = Sandbox::durable("pending-on");
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    sandbox.session_running(&repo, "ohjaa gate & g=$!; sleep 2; kill -9 $g; exit 0");
    let run = sandbox.all_validations(&repo)[0]["run"]
        .as_str()
        .expect("a run id")
        .to_owned();
    // Another session here, which asked for nothing.
    sandbox.session_running(&repo, "exit 0");
    let brief = sandbox.brief(&repo);
    assert!(
        brief.contains("Validations earlier sessions here left pending:")
            && brief.contains(&format!("sykli says run {run} is running")),
        "{brief}"
    );
    // sykli says it ended: no longer named.
    fs::remove_file(sandbox.dir.join("hold-runs")).unwrap();
    let brief = sandbox.brief(&repo);
    assert!(!brief.contains(&run), "{brief}");
}

/// D4, audit finding 3: a proof that reused tasks from sykli's cache says so,
/// in the marks, the report and its JSON: cached is never folded into passed.
#[test]
fn a_proof_that_reused_cached_tasks_says_so() {
    let mut sandbox = Sandbox::new("cached");
    sandbox.stub_contract = "fast";
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    let kept = repo.join(".git/sykli/receipts");
    let c1 = sandbox.commit(&repo, "one");
    receipt_with_tasks(
        &kept.join("rcpt_1.json"),
        &sandbox.tree(&repo, "HEAD"),
        "passed",
        &["passed", "cached"],
        1_000,
    );
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ✓ {} (1 of 2 tasks \
             cached)\n",
            short(&c1)
        )),
        "{report}"
    );
    let c2 = sandbox.commit(&repo, "two");
    receipt_with_tasks(
        &kept.join("rcpt_2.json"),
        &sandbox.tree(&repo, "HEAD"),
        "cached",
        &["cached", "cached"],
        2_000,
    );
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ✓ {} (all 2 tasks \
             cached)\n",
            short(&c2)
        )),
        "{report}"
    );
    assert!(
        report.contains("The rest: 2 verified, 2 of them reusing cached tasks"),
        "{report}"
    );
    let json = sandbox.report_json(&repo);
    let tip = &json["entries"][0]["commits"][0];
    assert_eq!(tip["mark"], "proven");
    assert_eq!(
        (tip["tasks"].as_u64(), tip["cached"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(
        json["report"]["default_branch_commits"]["verified_reusing_cache"], 2,
        "{json:#}"
    );
}

// ---- against the real sykli --------------------------------------------------
//
// What a flag says and what a mark is depend on sykli's own judgement, so
// these run the real one: built from sykli's main and named by
// OHJAA_TEST_SYKLI. CI's runners have no sykli, so they are ignored there:
//
//     OHJAA_TEST_SYKLI=/path/to/sykli cargo test --test config_home -- --ignored

const NEEDS_SYKLI: &str = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored";

/// A gate that fails while `state.txt` says broken.
const REAL_GATE: &str = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"ok","run":"! grep -q broken state.txt","inputs":["state.txt"]}]}"#;

impl Sandbox {
    /// A sandbox whose sykli is the real one, with `acme/widget` cloned and
    /// given an entry whose gate the real sykli locked.
    fn with_real_sykli(tag: &str) -> (Sandbox, PathBuf, PathBuf) {
        let sykli = std::env::var_os("OHJAA_TEST_SYKLI")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .unwrap_or_else(|| panic!("{NEEDS_SYKLI}"));
        let sandbox = Sandbox::new(tag);
        let bin = sandbox.dir.join("bin/sykli");
        fs::remove_file(&bin).unwrap();
        std::os::unix::fs::symlink(&sykli, &bin).unwrap();
        let (repo, bare) = sandbox.repo("acme", "widget", &[("state.txt", "ok\n")]);
        sandbox.entry("acme", "widget", Some(REAL_GATE));
        let contract = sandbox.config().join("repos/acme/widget/sykli.json");
        let output = sandbox
            .command("sykli", &repo)
            .arg("lock")
            .arg(&contract)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output).1);
        sandbox.apply();
        (sandbox, repo, bare)
    }

    /// Write `state.txt` and commit it.
    fn state(&self, repo: &Path, state: &str, message: &str) -> String {
        fs::write(repo.join("state.txt"), format!("{state}\n")).unwrap();
        self.git(repo, &["commit", "-q", "-am", message]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    /// `ohjaa gate` in `repo`, with its exit code.
    fn gate(&self, repo: &Path) -> (Option<i32>, String) {
        let output = self.ohjaa_in(repo, &["gate"]);
        let (stdout, stderr) = text(&output);
        (output.status.code(), format!("{stdout}{stderr}"))
    }
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_an_unproven_push_lands_flagged_and_a_proven_one_is_quiet() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-push");

    let head = sandbox.state(&repo, "ok again", "unproven");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        stderr.contains(
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree). \
             It is pushed and flagged; prove it with: ohjaa gate\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("sykli hook"), "{stderr}");

    // Proven with `ohjaa gate`: quiet, and sykli keeps the receipt.
    let head = sandbox.state(&repo, "ok, proven", "proven");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        !stderr.contains("ohjaa:") && !stderr.contains("sykli hook"),
        "a proven push is quiet: {stderr}"
    );
    assert_eq!(
        names_in(&repo.join(".git/sykli/receipts")).len(),
        1,
        "sykli kept the receipt that proved it"
    );

    // A failing run: the push lands, flagged in sykli's words.
    let head = sandbox.state(&repo, "broken", "broken");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(1), "{said}");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (no receipt of its tree verifies; the newest says .sykli/receipt.json: outcome expected passed or cached"),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "nothing but sykli's own .sykli/, which every repository ignores"
    );
}

/// The review of #45: sykli main (5acdf91 and on) reworded `verify --help`
/// and answers `verify --json`. A commit `ohjaa gate` proved is ✓, never
/// "sykli could not say".
#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_commit_ohjaa_gate_proved_is_marked_proven() {
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-proven");
    let head = sandbox.state(&repo, "ok, proven", "proven");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let json = sandbox.report_json(&repo);
    let tip = &json["entries"][0]["commits"][0];
    assert_eq!(tip["sha"], head.as_str(), "{json:#}");
    assert_eq!(tip["mark"], "proven", "{json:#}");
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!("): ✓ {}\n", short(&head))) && !report.contains("could not say"),
        "{report}"
    );
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_marks_proven_failed_unproven_and_a_forge_merge() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-marks");
    let c1 = sandbox.state(&repo, "ok, one", "one");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c2 = sandbox.state(&repo, "broken", "two");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(1), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let c3 = sandbox.state(&repo, "ok, three", "three");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ? {} ✗ {} ✓ {}\n",
            short(&c3),
            short(&c2),
            short(&c1)
        )),
        "{report}"
    );
    let json = sandbox.report_json(&repo);
    let commits = json["entries"][0]["commits"].as_array().unwrap();
    assert_eq!(commits[1]["mark"], "failed");
    assert!(
        commits[1]["why"]
            .as_str()
            .unwrap()
            .contains("its receipt says the run failed"),
        "{json:#}"
    );

    // A pull request proven in a linked worktree, merged on the forge.
    let linked = sandbox.dir.join("pr");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pr",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.state(&linked, "ok, four", "four");
    let (code, said) = sandbox.gate(&linked);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&linked, &["push", "-q", "origin", "pr"]);
    let forge = sandbox.dir.join("forge");
    sandbox.git(
        &sandbox.dir,
        &["clone", "-q", &bare.to_string_lossy(), "forge"],
    );
    sandbox.git(
        &forge,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1",
            "origin/pr",
        ],
    );
    sandbox.git(&forge, &["push", "-q", "origin", "main"]);
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    let merge = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    assert_ne!(merge, head);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a fetch at <time>): ✓ {}\n",
            short(&merge)
        )),
        "the merge has its proven head's tree: {report}"
    );
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_the_briefing_flags_first_and_ohjaa_gate_flips_the_tip() {
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-flag");
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let tip = sandbox.state(&repo, "ok, unproven", "unproven");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);

    let stdout = sandbox.brief(&repo);
    assert!(
        stdout.starts_with(&format!(
            "FLAG: main of acme/widget has never been proven (2 commits: ? {}, ? {}; \
             origin/main as of a push at <time>). Start by running `ohjaa gate` on main and \
             fix forward what fails.\n",
            short(&tip),
            short(&init)
        )),
        "{stdout}"
    );
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains("--briefing 'FLAG: main of acme/widget has never been proven"),
        "{stderr}"
    );

    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    let output = sandbox.ohjaa_in(&repo, &["brief"]);
    assert!(!text(&output).0.contains("FLAG:"), "{}", text(&output).0);
    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            "- acme/widget main (origin/main as of a push at <time>): ✓ {}\n",
            short(&tip)
        )),
        "{report}"
    );
}

/// D4 with a real sykli: `ohjaa gate` inside a session records its request,
/// then the receipt sykli kept of the run, under the name that is its
/// sha256, and the lock sykli read; and the run's id exactly when this sykli
/// has durable runs (`sykli status --help` lists `--run`, KATANA D3).
#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_gate_in_a_session_records_the_receipt_sykli_kept() {
    use sha2::Digest as _;
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-ref");
    let head = sandbox.state(&repo, "ok, a candidate", "candidate");
    let (stdout, stderr) = sandbox.session_running(&repo, "ohjaa gate");
    assert!(stdout.contains("ohjaa gate: passed"), "{stdout}{stderr}");
    let refs = validations(&sandbox.export(&repo), 0);
    assert_eq!(refs.len(), 1, "{refs:#?}");
    let found = &refs[0];
    assert_eq!(found["kind"], "candidate");
    assert_eq!(found["commit"], head.as_str());
    let status_help = sandbox
        .command("sykli", &repo)
        .args(["status", "--help"])
        .output()
        .unwrap();
    let durable = text(&status_help)
        .0
        .lines()
        .any(|line| line.trim_start().starts_with("--run "));
    assert_eq!(
        found["run"].as_str().is_some_and(|run| !run.is_empty()),
        durable,
        "{found:#}"
    );
    let lock: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(sandbox.config().join("repos/acme/widget/sykli.lock")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        found["contract_digest"], lock["contracts"]["sykli.json"],
        "the pin in the gate's lock: {found:#}"
    );
    assert_eq!(
        found["lock_digest"].as_str().map(str::len),
        Some(64),
        "{found:#}"
    );
    let path = found["receipt"]["path"].as_str().unwrap();
    let digest = found["receipt"]["digest"].as_str().unwrap();
    assert!(
        path.ends_with(&format!(".sykli/receipts/rcpt_{digest}.json")),
        "{found:#}"
    );
    let bytes = fs::read(path).unwrap();
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(&bytes)),
        digest,
        "the name sykli keeps it under is its sha256"
    );

    // Should-fix 3 of the review of #45, as the review found it: HEAD is
    // origin/main's tip, with an uncommitted edit. sykli runs the edited
    // tree and says so: a candidate, never integrated.
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    let tree = sandbox.tree(&repo, "HEAD");
    fs::write(repo.join("state.txt"), "ok, edited\n").unwrap();
    // (sykli verify refuses a dirty run for HEAD: the gate fails, as it should.)
    sandbox.session_running(&repo, "ohjaa gate; exit 0");
    let refs = sandbox.all_validations(&repo);
    let dirty = refs.last().unwrap();
    assert_eq!(dirty["commit"], head.as_str(), "{dirty:#}");
    assert_eq!(dirty["tree"], tree.as_str(), "{dirty:#}");
    assert_eq!(dirty["kind"], "candidate", "{dirty:#}");
    assert_eq!(dirty["ran"]["dirty"], true, "{dirty:#}");
    assert_ne!(dirty["ran"]["tree"], tree.as_str(), "{dirty:#}");
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_new_gate_leaves_old_proofs_unproven_not_failed() {
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-regate");
    let init = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let c1 = sandbox.state(&repo, "ok, one", "one");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    assert!(
        sandbox
            .report(&repo)
            .contains(&format!(": ✓ {}\n", short(&c1))),
        "{}",
        sandbox.report(&repo)
    );

    // The gate gains a task, and is locked again.
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    fs::write(
        &contract,
        format!(
            "{},{}]}}",
            REAL_GATE.strip_suffix("]}").unwrap(),
            r#"{"name":"more","run":"true","inputs":["state.txt"]}"#
        ),
    )
    .unwrap();
    let output = sandbox
        .command("sykli", &repo)
        .arg("lock")
        .arg(&contract)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output).1);

    let report = sandbox.report(&repo);
    assert!(
        report.contains(&format!(
            ": ? {} ? {} (nothing on it is proven)\n",
            short(&c1),
            short(&init)
        )) && !report
            .lines()
            .any(|line| line.starts_with("- acme/widget main") && line.contains('✗')),
        "{report}"
    );
    let json = sandbox.report_json(&repo);
    assert_eq!(
        json["entries"][0]["commits"][0]["code"], "other_contract",
        "{json:#}"
    );
    let stdout = sandbox.brief(&repo);
    assert!(
        stdout.starts_with(&format!(
            "FLAG: main of acme/widget has never been proven against its current gate (2 \
             commits: ? {}, ? {}; 1 passed under another contract; ",
            short(&c1),
            short(&init)
        )),
        "{stdout}"
    );
}

#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_commit_proven_in_another_worktree_is_a_quiet_push() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-elsewhere");
    let linked = sandbox.dir.join("linked");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "work",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    let head = sandbox.state(&linked, "ok, there", "proven there");
    let (code, said) = sandbox.gate(&linked);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["merge", "-q", "--ff-only", "work"]);
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    assert!(!stderr.contains("ohjaa:"), "{stderr}");

    let next = sandbox.state(&repo, "ok, nowhere", "proven nowhere");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), next);
    assert!(
        stderr.contains(
            "ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree)"
        ),
        "{stderr}"
    );
}

/// KATANA D10 against sykli main: adopt declares `${worktree_cache}` for a
/// cargo task, the real `sykli lock` pins the entry's gate (so `sykli
/// validate` finds no drift), and each worktree's gate then builds in a
/// directory of its own, outside every work tree. The `cargo` here is a stub
/// that writes down where it was told to build.
#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_adopt_gives_each_worktree_its_own_cargo_build_directories() {
    let sykli = std::env::var_os("OHJAA_TEST_SYKLI")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| panic!("{NEEDS_SYKLI}"));
    let sandbox = Sandbox::new("real-d10");
    let bin = sandbox.dir.join("bin");
    fs::remove_file(bin.join("sykli")).unwrap();
    std::os::unix::fs::symlink(&sykli, bin.join("sykli")).unwrap();
    // A task sees PATH, HOME and TMPDIR, and what its contract declares.
    write_executable(
        &bin.join("cargo"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$PWD\" \"$CARGO_TARGET_DIR\" \
         \"$CARGO_BUILD_BUILD_DIR\" >> \"$HOME/cargo-env\"\n",
    );
    let gate = r#"{"schema":"sykli-contract.v1","tasks":[{"name":"test","run":"cargo test","inputs":["state.txt"]},{"name":"ok","run":"! grep -q broken state.txt","inputs":["state.txt"]}]}
"#;
    let (repo, _) = sandbox.repo(
        "acme",
        "widget",
        &[("state.txt", "ok\n"), ("sykli.json", gate)],
    );
    let output = sandbox
        .command("sykli", &repo)
        .args(["lock", "sykli.json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output).1);
    sandbox.git(&repo, &["add", "sykli.lock"]);
    sandbox.git(&repo, &["commit", "-q", "-m", "lock"]);
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    sandbox.apply();

    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("${worktree_cache}: CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR declared for test, and sykli.lock locked again"),
        "{stdout}"
    );
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    let output = sandbox
        .command("sykli", &repo)
        .arg("validate")
        .arg(&contract)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output).0);

    let linked = sandbox.dir.join("linked");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "work",
            &linked.to_string_lossy(),
            "main",
        ],
    );
    for checkout in [&repo, &linked] {
        let (code, said) = sandbox.gate(checkout);
        assert_eq!(code, Some(0), "{said}");
    }
    let seen = fs::read_to_string(sandbox.home().join("cargo-env")).unwrap();
    let lines: Vec<Vec<&str>> = seen.lines().map(|line| line.split('|').collect()).collect();
    assert_eq!(lines.len(), 2, "{seen}");
    let common = format!("{}/", repo.join(".git/sykli/worktrees").display());
    for (line, checkout) in lines.iter().zip([&repo, &linked]) {
        let [pwd, target, build] = line[..] else {
            panic!("{seen}")
        };
        assert_eq!(Path::new(pwd), checkout.as_path(), "{seen}");
        assert!(
            target.starts_with(&common) && target.ends_with("/cache/cargo-target"),
            "{seen}"
        );
        assert!(
            build.starts_with(&common) && build.ends_with("/cache/cargo-build"),
            "{seen}"
        );
    }
    assert_ne!(
        lines[0][1], lines[1][1],
        "each worktree has its own: {seen}"
    );
    for checkout in [&repo, &linked] {
        assert!(!checkout.join("${worktree_cache}").exists());
    }
}

/// Should-fix 3: settings in a file git's global config includes count.
#[test]
fn settings_in_an_included_global_file_count() {
    let sandbox = Sandbox::new("includes");
    let extra = sandbox.dir.join("team.gitconfig");
    fs::write(
        sandbox.dir.join("gitconfig"),
        format!("[include]\n\tpath = {}\n", extra.display()),
    )
    .unwrap();
    fs::write(&extra, "[core]\n\thooksPath = /team/hooks\n").unwrap();
    for args in [&["apply"][..], &["apply", "--force"][..]] {
        let output = sandbox.ohjaa(&[args, &["--root", &sandbox.projects()]].concat());
        let (_, stderr) = text(&output);
        assert_eq!(output.status.code(), Some(3), "{args:?}: {stderr}");
        assert!(stderr.contains("/team/hooks"), "{stderr}");
    }
    assert!(
        text(&sandbox.ohjaa(&["apply", "--force", "--root", &sandbox.projects()]))
            .1
            .contains("a file git's global config includes")
    );

    fs::write(&extra, "[core]\n\texcludesFile = ~/team-ignore\n").unwrap();
    sandbox.apply();
    assert!(sandbox.read("home/team-ignore").contains(".sykli/"));
    assert!(!sandbox.dir.join("config/git/ignore").exists());
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);
}

/// The nit: entries are found whatever the case of the remote.
#[test]
fn an_entry_is_found_whatever_the_case_of_the_remote() {
    let mut sandbox = Sandbox::new("case");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("Acme", "Widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    assert!(sandbox
        .config()
        .join("repos/acme/widget/repo.toml")
        .is_file());
    assert!(sandbox.gated(&repo));
    let head = sandbox.commit(&repo, "a");
    let output = sandbox.git_out(&repo, &["push", "origin", "main"]);
    let (_, stderr) = text(&output);
    assert!(
        output.status.success() && stderr.contains("ohjaa: UNPROVEN push to main ("),
        "the entry was found: {stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
}

/// A clone that names an entry only through its push URL is not gated by
/// git's patterns (they see remote.*.url); doctor and `ohjaa run` say so.
#[test]
fn doctor_flags_a_clone_named_only_by_its_push_url() {
    let sandbox = Sandbox::new("pushurl");
    let (repo, bare) = sandbox.repo("acme", "pushy", &[]);
    sandbox.git(
        &repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/other/thing",
        ],
    );
    sandbox.git(
        &repo,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            &bare.to_string_lossy(),
        ],
    );
    sandbox.entry("acme", "pushy", Some("{}\n"));
    sandbox.apply();
    assert!(!sandbox.gated(&repo));
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("DRIFT  pushy (acme/pushy): git does not include"),
        "{stdout}"
    );
}

/// A generated file git cannot read breaks every git command: doctor names
/// it, and apply puts it right.
#[test]
fn an_unreadable_generated_file_is_named_and_apply_repairs_it() {
    let sandbox = Sandbox::new("corrupt");
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    sandbox.apply();
    fs::write(
        sandbox.config().join("git/config"),
        "[includeIf \"hasconfig:remote.*.url:x\n",
    )
    .unwrap();
    assert!(
        !sandbox.git_out(&repo, &["status"]).status.success(),
        "git is broken"
    );
    let output = sandbox.ohjaa(&["doctor", "--root", &sandbox.projects()]);
    let (stdout, _) = text(&output);
    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(
        stdout.contains("cannot be read by git, which then fails in every repository"),
        "{stdout}"
    );
    sandbox.apply();
    assert!(sandbox.git_out(&repo, &["status"]).status.success());
    assert!(sandbox.gated(&repo));
}

#[test]
fn shell_init_prints_the_launcher_and_its_rules_hold() {
    let sandbox = Sandbox::new("shell");
    let output = sandbox.ohjaa(&["shell-init", "bash"]);
    assert_eq!(output.status.code(), Some(2));
    let output = sandbox.ohjaa(&["shell-init", "zsh"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output).0.contains("_ohjaa_should_wrap() {"));

    if !Path::new("/bin/zsh").exists() && !Path::new("/usr/bin/zsh").exists() {
        eprintln!("skipping the launcher's rules: zsh is not installed here");
        return;
    }
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    // The launcher wraps only at a terminal; the test says it is at one, and
    // ohjaa is a function that prints how it was called.
    let script = r#"
eval "$(ohjaa shell-init zsh)"
_ohjaa_at_terminal() { true; }
ohjaa() { print -r -- "ohjaa $*"; }
claude
claude "fix the parser"
claude --model opus "do x"
claude -p hi
claude --resume
claude a b
claude mcp list
OHJAA_WRAP=0 claude
OHJAA_SESSION=s claude
codex "write it"
codex exec hi
cd /
claude
"#;
    let output = sandbox
        .command("zsh", &repo)
        .args(["-f", "-c", script])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&output);
    assert_eq!(
        stdout,
        "ohjaa run claude --intent interactive --\n\
         ohjaa run claude --intent interactive --task fix the parser --\n\
         ohjaa run claude --intent interactive --task do x -- --model opus\n\
         direct -p hi\n\
         direct --resume\n\
         direct a b\n\
         direct mcp list\n\
         direct \n\
         direct \n\
         ohjaa run codex --intent interactive --task write it --\n\
         direct-codex exec hi\n\
         direct \n",
        "{stderr}"
    );
    assert!(
        stderr.contains("not wrapped (more than one prompt-like argument)"),
        "{stderr}"
    );
}

/// "We don't want to run git git git": each command's own git processes, as
/// git's trace2 counts them, within its budget. Every case first proves it
/// measured something real (a positive control): the trace holds the git
/// command that ran the hook, and what the command does visibly happened.
/// A hook is measured as the same git command run through ohjaa's
/// dispatcher, minus the same command with ohjaa out of the way. The stub
/// sykli runs no git, so the rest is ohjaa's own.
#[test]
fn every_command_stays_within_its_git_budget() {
    let mut sandbox = Sandbox::new("budget");
    // A sykli that takes the repository's facts: ohjaa hands them over.
    sandbox.stub_contract = "fast";
    // A vessa that lists nothing, and says when it is asked.
    write_executable(
        &sandbox.dir.join("bin/vessa"),
        "#!/bin/sh\necho \"$*\" >> \"$SANDBOX/vessa-calls\"\n\
         echo '{\"schema\":\"vessa-usage.v1\",\"warnings\":[],\"sessions\":[]}'\n",
    );
    let (gated, bare) = sandbox.repo("acme", "gated", &[("sykli.json", "{}\n")]);
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    write_executable(
        &own.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho ran >> \"$SANDBOX/own-pre-commit\"\n",
    );
    sandbox.repo("acme", "free", &[]);
    let (local, _) = sandbox.repo("acme", "local-hooks", &[("sykli.json", "{}\n")]);
    sandbox.git(&local, &["config", "core.hooksPath", ".githooks"]);
    let mut report = Vec::new();
    let mut budget = |what: &str, starts: usize, at_most: usize| {
        let unit = if what.ends_with("sykli verify") {
            "sykli verify calls"
        } else {
            "git processes"
        };
        eprintln!("{what:<44} {starts} {unit} (budget {at_most})");
        report.push((what.to_owned(), starts, at_most));
    };
    let root = sandbox.projects();
    let ohjaa_here = |args: &[&str]| {
        let mut command = sandbox.command("ohjaa", &sandbox.dir);
        command.args(args);
        command
    };

    // apply before any entry: git's version and global config, no clone read
    // (nothing can be gated yet), the include added and git/config checked
    // before it is written.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply", "--root", &root]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("ohjaa apply: wrote"),
        "{}",
        text(&output).0
    );
    assert!(
        started(&starts, "--version") && started(&starts, "--global"),
        "{starts:#?}"
    );
    budget(
        "ohjaa apply (no entries yet, first run)",
        starts.len(),
        2 + 2,
    );
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["apply"]));
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    assert!(started(&starts, "--global"), "{starts:#?}");
    budget("ohjaa apply (no entries yet, no change)", starts.len(), 2);

    // adopt: the repository's config and objects, git's global config, its
    // config again once gated, and a check per file it writes (2).
    let (starts, output) =
        sandbox.git_starts(&mut ohjaa_here(&["adopt", &gated.to_string_lossy()]));
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("  observed: git runs"),
        "{}",
        text(&output).0
    );
    assert!(sandbox
        .config()
        .join("repos/acme/gated/repo.toml")
        .is_file());
    assert!(started(&starts, "cat-file"), "{starts:#?}");
    budget("ohjaa adopt", starts.len(), 4 + 2);
    sandbox.adopt(&own);

    // apply with two entries, nothing to change: now it reads the clones.
    // Exactly git's version and global config, then one process per clone
    // under the roots (4, gated or not): each clone is read exactly once.
    let (events, output) = sandbox.git_events(&mut ohjaa_here(&["apply"]));
    assert!(
        text(&output).0.contains("nothing to change"),
        "{}",
        text(&output).0
    );
    for clone in ["gated", "own", "free", "local-hooks"] {
        let worktree = format!(
            "\"worktree\":\"{}\"",
            sandbox.dir.join("projects").join(clone).display()
        );
        let reads = events
            .iter()
            .filter(|line| line.contains("\"event\":\"def_repo\"") && line.contains(&worktree))
            .count();
        assert_eq!(reads, 1, "apply read {clone} {reads} times, not once");
    }
    let starts = events
        .iter()
        .filter(|line| line.contains("\"event\":\"start\""))
        .count();
    assert_eq!(
        starts,
        2 + 4,
        "apply with entries: 2 + 1 per clone, exactly"
    );
    budget(
        "ohjaa apply (2 entries, 4 clones, no change)",
        starts,
        2 + 4,
    );

    // pre-commit, a hook that only chains: `git commit` through the
    // dispatcher, against the same commit with git running the own hook.
    let own_ran = || sandbox.read("own-pre-commit").lines().count();
    let commit = |direct: bool| {
        let mut command = sandbox.command("git", &own);
        if direct {
            command.args([
                "-c",
                &format!("core.hooksPath={}", own.join(".git/hooks").display()),
            ]);
        }
        command.args(["commit", "-q", "--allow-empty", "-m", "measure"]);
        let before = own_ran();
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(output.status.success(), "{}", text(&output).1);
        assert!(
            started(&starts, "commit"),
            "no git commit in the trace: {starts:#?}"
        );
        assert_eq!(own_ran(), before + 1, "the own pre-commit did not run");
        starts.len()
    };
    let through = commit(false);
    let direct = commit(true);
    budget(
        "ohjaa hook pre-commit (chains only)",
        through.saturating_sub(direct),
        0,
    );

    // pre-push: `git push` through the dispatcher, against the same push
    // with no hooks at all. Each pushes one new commit to the observed main.
    let calls = || sandbox.read("sykli-calls").lines().count();
    let push = |what: &str, hooks: bool| {
        sandbox.commit(&gated, what);
        let mut command = sandbox.command("git", &gated);
        if !hooks {
            command.args(["-c", "core.hooksPath=/dev/null"]);
        }
        command.args(["push", "-q", "origin", "HEAD:refs/heads/main"]);
        let before = calls();
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(output.status.success(), "{}", text(&output).1);
        assert!(
            started(&starts, "push"),
            "no git push in the trace: {starts:#?}"
        );
        assert_eq!(
            sandbox.git(&bare, &["rev-parse", "main"]),
            sandbox.git(&gated, &["rev-parse", "HEAD"]),
            "it landed"
        );
        (starts.len(), calls() - before, text(&output).1)
    };
    let (through, observe_calls, said) = push("measured", true);
    let (direct, no_calls, _) = push("plain", false);
    assert_eq!(no_calls, 0, "sykli ran without the dispatcher");
    assert!(observe_calls >= 1, "the push was never handed to sykli");
    assert!(said.contains("ohjaa: UNPROVEN push to main ("), "{said}");
    let facts = format!(
        "--repo-root {} --git-common-dir {}",
        gated.display(),
        gated.join(".git").display()
    );
    assert!(
        sandbox
            .read("sykli-calls")
            .contains(&format!("--branch main {facts} origin")),
        "{}",
        sandbox.read("sykli-calls")
    );
    budget(
        "ohjaa hook pre-push (observed)",
        through.saturating_sub(direct),
        1,
    );

    // pre-push of a commit sykli does not see proven from this worktree but
    // the marks do (its receipt in another one): the commit's tree, asked
    // once, and one sykli verify.
    let elsewhere = sandbox.dir.join("elsewhere");
    sandbox.git(
        &gated,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "elsewhere",
            &elsewhere.to_string_lossy(),
            "main",
        ],
    );
    let proven = sandbox.commit(&elsewhere, "proven-elsewhere");
    receipt(
        &elsewhere.join(".sykli/receipt.json"),
        &sandbox.tree(&elsewhere, "HEAD"),
        "passed",
        500,
    );
    sandbox.git(&gated, &["merge", "-q", "--ff-only", "elsewhere"]);
    let verifies_before = sandbox
        .read("sykli-calls")
        .lines()
        .filter(|line| line.contains("|verify ") && line.contains("--commit"))
        .count();
    let (starts, output) = sandbox.git_starts(sandbox.command("git", &gated).args([
        "push",
        "-q",
        "origin",
        "HEAD:refs/heads/main",
    ]));
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), proven);
    assert!(started(&starts, "rev-parse"), "the commit's tree was asked");
    let verifies = sandbox
        .read("sykli-calls")
        .lines()
        .filter(|line| line.contains("|verify ") && line.contains("--commit"))
        .count()
        - verifies_before;
    budget(
        "ohjaa hook pre-push (proven elsewhere)",
        starts.len().saturating_sub(direct),
        1,
    );
    budget(
        "ohjaa hook pre-push (proven elsewhere): sykli verify",
        verifies,
        1,
    );
    // One more on top, proven nowhere: main is unproven at its tip again,
    // above the commit proven elsewhere.
    let (_, _, said) = push("on-top", true);
    assert!(said.contains("ohjaa: UNPROVEN push to main ("), "{said}");

    // ohjaa gate: one git process, then sykli's own, with the facts.
    let (starts, output) = sandbox.git_starts(sandbox.command("ohjaa", &gated).arg("gate"));
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(
        text(&output).0.contains("ohjaa gate: passed"),
        "{}",
        text(&output).0
    );
    assert!(sandbox
        .read("projects/gated/.sykli/receipt.json")
        .contains("passed"));
    let contract = sandbox.config().join("repos/acme/gated/sykli.json");
    let calls = sandbox.read("sykli-calls");
    assert!(
        calls.contains(&format!("|run {} {facts} --json", contract.display())),
        "{calls}"
    );
    assert!(calls.contains(&format!("{facts} --commit HEAD")), "{calls}");
    budget("ohjaa gate", starts.len(), 1);

    // ohjaa gate inside a session (D4): the same, and one git process for
    // the commit and tree its request names, written before sykli is asked.
    let output = sandbox
        .command("ohjaa", &gated)
        .args([
            "run",
            "sh",
            "--intent",
            "measure",
            "--no-briefing",
            "--",
            "-c",
            "true",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output).1);
    let session = sandbox.export(&gated)["sessions"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (starts, output) = sandbox.git_starts(
        sandbox
            .command("ohjaa", &gated)
            .arg("gate")
            .env("OHJAA_SESSION", &session),
    );
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(
        text(&output)
            .0
            .contains(&format!("recorded in session {session}")),
        "{}",
        text(&output).0
    );
    assert_eq!(
        validations(&sandbox.export(&gated), 0).len(),
        1,
        "the request was recorded"
    );
    assert!(started(&starts, "cat-file"), "{starts:#?}");
    budget("ohjaa gate (inside a session)", starts.len(), 2);

    // The same against a sykli with durable runs (D3): the run handed over
    // detached and asked about by its id, with the same git processes.
    let (starts, output) = sandbox.git_starts(
        sandbox
            .command("ohjaa", &gated)
            .arg("gate")
            .env("OHJAA_SESSION", &session)
            .env("STUB_SYKLI_D3", "1")
            .env("SYKLI_CALLER", "gate"),
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains("ohjaa gate: sykli run run-") && stdout.contains("ohjaa gate: passed"),
        "{stdout}"
    );
    let refs = validations(&sandbox.export(&gated), 0);
    assert_eq!(refs.len(), 2, "{refs:#?}");
    assert!(
        refs[1]["run"].is_string(),
        "the run id was recorded: {refs:#?}"
    );
    assert!(
        sandbox.status_asks("gate", "") > 0,
        "sykli was asked about the run"
    );
    assert!(started(&starts, "cat-file"), "{starts:#?}");
    budget(
        "ohjaa gate (inside a session, durable sykli)",
        starts.len(),
        2,
    );

    // A run still going: the gate is stopped once the run's id is recorded,
    // and the run is left pending in the session's record.
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    // In a group of its own, so it is stopped with what it started; its
    // asks are its own (SYKLI_CALLER), never counted as another's.
    let mut held = std::os::unix::process::CommandExt::process_group(
        sandbox
            .command("ohjaa", &gated)
            .arg("gate")
            .env("OHJAA_SESSION", &session)
            .env("STUB_SYKLI_D3", "1")
            .env("SYKLI_CALLER", "held")
            .stdout(std::process::Stdio::null()),
        0,
    )
    .spawn()
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let pending = loop {
        let refs = validations(&sandbox.export(&gated), 0);
        if let Some(run) = refs.get(2).and_then(|v| v["run"].as_str()) {
            break run.to_owned();
        }
        assert!(std::time::Instant::now() < deadline, "no run id: {refs:#?}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", held.id())])
        .status()
        .unwrap();
    held.wait().unwrap();

    // ohjaa brief asks sykli about the pending run (and only it), beyond the
    // same briefing from a sykli that cannot be asked: no git process more.
    let (with, output) = sandbox.git_starts(
        sandbox
            .command("ohjaa", &gated)
            .arg("brief")
            .env("STUB_SYKLI_D3", "1")
            .env("SYKLI_CALLER", "brief"),
    );
    assert!(
        text(&output)
            .0
            .contains(&format!("sykli says run {pending} is running")),
        "{}",
        text(&output).0
    );
    assert_eq!(
        sandbox.status_asks("brief", &pending),
        1,
        "sykli was asked about the pending run, once"
    );
    assert_eq!(
        sandbox.status_asks("brief", ""),
        1,
        "and about nothing else"
    );
    let (without, output) = sandbox.git_starts(sandbox.command("ohjaa", &gated).arg("brief"));
    assert!(
        text(&output)
            .0
            .contains("sykli cannot be asked about runs here (no `sykli status --run`)"),
        "{}",
        text(&output).0
    );
    budget(
        "ohjaa brief: sykli asked about a pending run",
        with.len().saturating_sub(without.len()),
        0,
    );

    // The hook path never asks about runs: a push through the dispatcher
    // with a run pending is measured against the plain push above.
    let calls_before = sandbox.read("sykli-calls").lines().count();
    sandbox.commit(&gated, "with-a-run-pending");
    let (starts, output) = sandbox.git_starts(
        sandbox
            .command("git", &gated)
            .args(["push", "-q", "origin", "HEAD:refs/heads/main"])
            .env("STUB_SYKLI_D3", "1")
            .env("SYKLI_CALLER", "push"),
    );
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(started(&starts, "push"), "no git push in the trace");
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", "main"]),
        sandbox.git(&gated, &["rev-parse", "HEAD"]),
        "it landed"
    );
    assert!(
        sandbox.read("sykli-calls").lines().count() > calls_before,
        "the push was handed to sykli"
    );
    assert_eq!(
        sandbox.status_asks("push", ""),
        0,
        "the hook asks about no run"
    );
    budget(
        "ohjaa hook pre-push (a run pending)",
        starts.len().saturating_sub(direct),
        1,
    );
    fs::remove_file(sandbox.dir.join("hold-runs")).unwrap();

    // doctor: two for the machine, at most two per clone.
    let (starts, output) = sandbox.git_starts(&mut ohjaa_here(&["doctor", "--root", &root]));
    let (stdout, _) = text(&output);
    assert!(stdout.contains("ohjaa doctor: "), "{stdout}");
    for clone in [
        "gated (acme/gated)",
        "own (acme/own)",
        "local-hooks: ",
        "free",
    ] {
        assert!(
            stdout.contains(clone),
            "doctor did not look at {clone}: {stdout}"
        );
    }
    assert!(
        started(&starts, "--version") && started(&starts, "--global"),
        "{starts:#?}"
    );
    budget("ohjaa doctor (4 clones)", starts.len(), 2 + 2 * 4);

    // The marks need sykli once: the commit proven elsewhere; the one pushed
    // above it is not proven, and nothing below is asked about.
    let verified = || {
        sandbox
            .read("sykli-calls")
            .lines()
            .filter(|line| line.contains("|verify ") && line.contains("--commit"))
            .count()
    };

    // ohjaa run's briefing for a clone that has drifted (a hook of its own
    // with no dispatcher) and whose main is unproven at its tip: a launch
    // with entries, beyond the same launch with no entries at all, which
    // reads no clone.
    write_executable(&gated.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    let launch = |sandbox: &Sandbox| {
        let before = verified();
        let (starts, output) = sandbox.git_starts(sandbox.command("ohjaa", &gated).args([
            "run",
            "claude",
            "--intent",
            "x",
            "--dry-run",
        ]));
        let (_, stderr) = text(&output);
        assert!(
            stderr.contains("ohjaa: dry-run; would run:"),
            "the launch stopped short: {stderr}"
        );
        let logs = starts
            .iter()
            .filter(|line| line.contains("\"log\"") && line.contains("refs/remotes/origin/main"))
            .count();
        (starts.len(), stderr, verified() - before, logs)
    };
    let (with, said, verifies, logs) = launch(&sandbox);
    assert!(
        said.contains("its own pre-commit hook has no dispatcher"),
        "no warning for the drifted clone: {said}"
    );
    assert!(
        said.contains("would run: ")
            && said.contains("--briefing 'FLAG: main of acme/gated is unproven since "),
        "no flag first in the briefing: {said}"
    );
    assert_eq!(logs, 1, "one walk, in one git log");
    assert_eq!(verifies, 1, "sykli verify, once");
    let repos = sandbox.config().join("repos");
    fs::rename(&repos, sandbox.config().join("repos.aside")).unwrap();
    let (without, said, none, _) = launch(&sandbox);
    fs::rename(sandbox.config().join("repos.aside"), &repos).unwrap();
    assert!(
        !said.contains("has no dispatcher") && !said.contains("FLAG"),
        "{said}"
    );
    assert_eq!(none, 0);
    budget(
        "ohjaa run's briefing (drift and marks)",
        with.saturating_sub(without),
        3,
    );
    budget("ohjaa run's briefing: sykli verify", verifies, 1);

    // ohjaa report: the marks of both entries, beyond the same report with
    // no entries; sykli asked only where a receipt says it passed (one).
    let reported = |sandbox: &Sandbox| {
        let before = verified();
        let (starts, output) = sandbox.git_starts(
            sandbox
                .command("ohjaa", &gated)
                .args(["report", "--root", &root]),
        );
        let stdout = blank_times(&text(&output).0);
        assert!(stdout.starts_with("ohjaa report, "), "{stdout}");
        let logs = starts
            .iter()
            .filter(|line| {
                line.contains("\"log\"")
                    && line.contains("refs/remotes/origin/main")
                    && line.contains("--max-count=50")
            })
            .count();
        // The clones whose config was read to find their entry.
        let listings = starts
            .iter()
            .filter(|line| line.contains("\"config\",\"-z\",\"--list\""))
            .count();
        (starts.len(), stdout, verified() - before, logs, listings)
    };
    let (with, stdout, verifies, logs, listings) = reported(&sandbox);
    for marked in [
        "- acme/gated main (origin/main as of a push at <time>): ? ",
        "- acme/own main (origin/main as of a push at <time>): ? ",
    ] {
        assert!(stdout.contains(marked), "no marks {marked:?}: {stdout}");
    }
    assert_eq!(logs, 2, "one walk per entry");
    assert_eq!(listings, 2, "the config of each entry's clone, read once");
    assert_eq!(verifies, 1, "one receipt says it passed");
    fs::rename(&repos, sandbox.config().join("repos.aside")).unwrap();
    let (without, stdout, _, no_logs, no_listings) = reported(&sandbox);
    fs::rename(sandbox.config().join("repos.aside"), &repos).unwrap();
    assert!(!stdout.contains("Marks on each"), "{stdout}");
    assert_eq!(
        (no_logs, no_listings),
        (0, 0),
        "no entry: no clone read for one"
    );
    // The marks' own processes: each entry's clone config, and its walk.
    budget("ohjaa report's marks (2 entries)", logs + listings, 2 * 2);
    // The whole report, beyond the same one with no entries: the gate also
    // spares the window the default-branch lookup and the contract checks.
    budget(
        "ohjaa report with 2 entries, beyond none",
        with.saturating_sub(without),
        2 * 2,
    );
    budget("ohjaa report's marks: sykli verify", verifies, 1);

    // ohjaa report --json --marks: the marks, the proposals and the sessions
    // only, for other tools to read often: no window, no vessa. Beyond the
    // same with no entries, which reads no clone at all.
    let vessa = || sandbox.read("vessa-calls").lines().count();
    assert!(vessa() > 0, "the full report asks vessa");
    let marks_only = |sandbox: &Sandbox| {
        let before = verified();
        let asked = vessa();
        let (starts, output) = sandbox.git_starts(
            sandbox
                .command("ohjaa", &gated)
                .args(["report", "--json", "--marks", "--root", &root]),
        );
        let (stdout, stderr) = text(&output);
        let json: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"));
        let walks = starts
            .iter()
            .filter(|line| line.contains("\"log\"") && line.contains("--max-count=50"))
            .count();
        let window = started(&starts, "worktree") || started(&starts, "--walk-reflogs");
        assert_eq!(vessa(), asked, "vessa is not asked");
        (starts.len(), json, verified() - before, walks, window)
    };
    let (with, json, verifies, walks, window) = marks_only(&sandbox);
    assert_eq!(json["schema"], "ohjaa-marks.v1");
    assert_eq!(json["report"], serde_json::Value::Null, "no window");
    let marked: Vec<&str> = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["repo"].as_str().unwrap())
        .collect();
    assert_eq!(marked, ["acme/gated", "acme/own"]);
    assert!(
        json["entries"][0]["commits"].as_array().unwrap().len() > 1,
        "{json:#}"
    );
    assert_eq!(walks, 2, "one walk per entry");
    assert!(!window, "the window is not read");
    fs::rename(&repos, sandbox.config().join("repos.aside")).unwrap();
    let (without, json, _, none, _) = marks_only(&sandbox);
    fs::rename(sandbox.config().join("repos.aside"), &repos).unwrap();
    assert_eq!(json["entries"], serde_json::json!([]));
    assert_eq!(none, 0);
    budget(
        "ohjaa report --json --marks (2 entries)",
        with.saturating_sub(without),
        2 * 2,
    );
    budget("ohjaa report --json --marks: sykli verify", verifies, 1);
    budget("ohjaa report --json --marks, no entries", without, 1);

    let over: Vec<_> = report
        .iter()
        .filter(|(_, starts, at_most)| starts > at_most)
        .collect();
    assert!(over.is_empty(), "over budget: {over:?}");
}

/// N1: what apply finds does not depend on where it runs. With no roots file
/// it does not guess (a home folder whose `.oh-my-zsh` is a repository would
/// pass for the projects folder): it refuses until --root names the folder,
/// writes that down, never removes a dispatcher, and --prune needs the roots
/// written down.
#[test]
fn apply_only_adds_dispatchers_and_writes_the_roots_it_scanned() {
    let sandbox = Sandbox::new("roots");
    let (own, _) = sandbox.repo("acme", "own", &[("sykli.json", "{}\n")]);
    write_executable(&own.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n");
    // A home folder with a repository in it, as ~/.oh-my-zsh is.
    let zsh = sandbox.home().join(".oh-my-zsh");
    fs::create_dir_all(&zsh).unwrap();
    sandbox.git(&zsh, &["init", "-q"]);

    // No roots file and no --root: refused, from anywhere, dry run or not,
    // naming the fix and what it would have used.
    for (cwd, args) in [
        (sandbox.home(), &["apply"][..]),
        (sandbox.home(), &["apply", "--dry-run"][..]),
        (sandbox.dir.join("projects"), &["apply"][..]),
    ] {
        let output = sandbox.ohjaa_in(&cwd, args);
        let (stdout, stderr) = text(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} in {}: {stdout}{stderr}",
            cwd.display()
        );
        assert!(stderr.contains("ohjaa apply --root <dir>"), "{stderr}");
        assert!(
            stderr.contains(&format!("would have used {}", cwd.display())),
            "{stderr}"
        );
    }
    assert!(!sandbox.config().exists(), "nothing written");
    assert!(!sandbox.dir.join("gitconfig").exists(), "nothing written");

    // With --root: the roots are written down.
    let output = sandbox.ohjaa_in(&sandbox.home(), &["apply", "--root", &sandbox.projects()]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "wrote {} with {}",
            sandbox.config().join("roots").display(),
            sandbox.projects()
        )),
        "{stdout}"
    );
    sandbox.adopt(&own);
    let hooks = sandbox.entry_hooks("acme", "own");
    assert_eq!(names_in(&hooks), ["pre-commit", "pre-push"]);

    // From the home folder: the roots file decides, and nothing is removed.
    let output = sandbox.ohjaa_in(&sandbox.home(), &["apply"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(names_in(&hooks), ["pre-commit", "pre-push"]);
    let output = sandbox.ohjaa_in(&sandbox.home(), &["doctor"]);
    assert!(
        text(&output).0.contains(&format!(
            "roots: {} (from {})",
            sandbox.projects(),
            sandbox.config().join("roots").display()
        )),
        "{}",
        text(&output).0
    );

    // --prune refuses without a roots file; with one, it removes spares.
    fs::rename(
        sandbox.config().join("roots"),
        sandbox.dir.join("roots.aside"),
    )
    .unwrap();
    let output = sandbox.ohjaa(&["apply", "--prune", "--root", &sandbox.projects()]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output).1);
    assert!(text(&output).1.contains("refusing --prune"));
    fs::rename(
        sandbox.dir.join("roots.aside"),
        sandbox.config().join("roots"),
    )
    .unwrap();
    fs::remove_file(own.join(".git/hooks/pre-commit")).unwrap();
    let output = sandbox.ohjaa(&["apply"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        names_in(&hooks),
        ["pre-commit", "pre-push"],
        "a spare is kept"
    );
    let output = sandbox.ohjaa(&["doctor"]);
    assert!(
        text(&output).0.contains("spare dispatchers in"),
        "{}",
        text(&output).0
    );
    let output = sandbox.ohjaa(&["apply", "--prune"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(names_in(&hooks), ["pre-push"]);
}

/// The ohjaa the hooks run is the one on PATH, unless --bin names another.
#[test]
fn apply_runs_the_ohjaa_on_path_unless_bin_names_another() {
    let sandbox = Sandbox::new("bin");
    let (repo, _) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.entry("acme", "widget", Some("{}\n"));
    let other = sandbox.dir.join("other");
    fs::create_dir_all(&other).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_ohjaa"), other.join("ohjaa")).unwrap();
    let elsewhere = other.join("ohjaa").to_string_lossy().into_owned();
    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply", "--root", &sandbox.projects()])
        .output()
        .unwrap();
    let (_, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("is not the one on PATH"), "{stderr}");
    assert!(
        !sandbox.config().join("git").exists(),
        "nothing half applied"
    );

    let output = sandbox
        .command(&elsewhere, &sandbox.dir)
        .args(["apply", "--root", &sandbox.projects(), "--bin", &elsewhere])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let dispatcher =
        fs::read_to_string(sandbox.entry_hooks("acme", "widget").join("pre-push")).unwrap();
    assert!(
        dispatcher.contains(&format!("exec '{elsewhere}' hook pre-push")),
        "{dispatcher}"
    );
    assert!(fs::read_to_string(sandbox.home().join(".zshrc"))
        .unwrap()
        .contains(&format!("[[ -x '{elsewhere}' ]]")));
    assert!(sandbox.gated(&repo));
}

/// A git client without the shell's PATH (a GUI) still has its push observed:
/// the dispatcher names ohjaa and sykli by the paths apply found.
#[test]
fn a_push_from_a_client_without_the_path_is_still_observed() {
    let mut sandbox = Sandbox::new("gui");
    sandbox.stub_contract = "1";
    let (repo, bare) = sandbox.repo("acme", "widget", &[("sykli.json", "{}\n")]);
    sandbox.apply();
    sandbox.adopt(&repo);
    let head = sandbox.commit(&repo, "change");
    let push = |sandbox: &Sandbox| {
        sandbox
            .command("/usr/bin/git", &repo)
            .env("PATH", "/usr/bin:/bin")
            .args(["push", "origin", "main"])
            .output()
            .unwrap()
    };
    let output = push(&sandbox);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("ohjaa: UNPROVEN push to main (no receipt in this worktree"),
        "sykli was asked: {stderr}"
    );
    assert_eq!(sandbox.git(&bare, &["rev-parse", "main"]), head);
    fs::create_dir_all(repo.join(".sykli")).unwrap();
    fs::write(repo.join(".sykli/receipt.json"), "{}").unwrap();
    sandbox.commit(&repo, "proven");
    let output = push(&sandbox);
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(!text(&output).1.contains("ohjaa:"), "{}", text(&output).1);
}

/// The ignore block holds `.sykli/` and `.teko/`. A block an earlier apply
/// wrote with `.sykli/` only is brought up to date in place, and undo still
/// puts back exactly what was there before the first apply.
#[test]
fn an_older_ignore_block_is_upgraded_in_place_and_undo_is_exact() {
    let sandbox = Sandbox::new("teko");
    let (repo, _) = sandbox.repo("acme", "any", &[]);
    fs::create_dir_all(sandbox.dir.join("config/git")).unwrap();
    fs::write(sandbox.dir.join("config/git/ignore"), "*.swp\n").unwrap();
    sandbox.apply();
    let ignore = sandbox.read("config/git/ignore");
    assert!(ignore.contains("\n.sykli/\n.teko/\n"), "{ignore}");
    sandbox.git(&repo, &["check-ignore", "-q", ".teko/state.json"]);
    sandbox.git(&repo, &["check-ignore", "-q", ".sykli/receipt.json"]);

    // As an earlier apply left it: .sykli/ only.
    let older = ignore.replace(".sykli/\n.teko/\n", ".sykli/\n");
    fs::write(sandbox.dir.join("config/git/ignore"), &older).unwrap();
    let output = sandbox.ohjaa(&["doctor"]);
    let (stdout, _) = text(&output);
    assert!(
        stdout.contains("holds an older ohjaa block; it should ignore .sykli/ .teko/"),
        "{stdout}"
    );
    let output = sandbox.ohjaa(&["apply"]);
    let (stdout, _) = text(&output);
    assert!(stdout.contains("added .sykli/ .teko/ to"), "{stdout}");
    assert_eq!(
        sandbox.read("config/git/ignore"),
        ignore,
        "upgraded in place"
    );

    let output = sandbox.ohjaa(&["apply", "--undo"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.read("config/git/ignore"), "*.swp\n");
}

// ---- KATANA Stage 5: Judgement, integrated-state validation, stale base, ----
// ---- and the sessions of every worktree of a clone ------------------------

/// The ref Judgements are shared under, as git notes on the head they judge.
const JUDGEMENTS_REF: &str = "refs/notes/ohjaa/judgements";

/// Ann, who wrote the Change, and Rae, who reviews it: each as a person's git
/// identity for every git command they run.
const ANN: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Ann Author"),
    ("GIT_AUTHOR_EMAIL", "ann@example.invalid"),
    ("GIT_COMMITTER_NAME", "Ann Author"),
    ("GIT_COMMITTER_EMAIL", "ann@example.invalid"),
];
const RAE: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Rae Reviewer"),
    ("GIT_AUTHOR_EMAIL", "rae@example.invalid"),
    ("GIT_COMMITTER_NAME", "Rae Reviewer"),
    ("GIT_COMMITTER_EMAIL", "rae@example.invalid"),
];

impl Sandbox {
    /// A sandbox for Stage 5: a durable-run sykli (D3) that also answers
    /// `status --commit`, and `acme/widget` with an entry whose lock pins its
    /// gate and whose `visibility` is as given (none: the entry does not say).
    fn stage5(tag: &str, visibility: Option<&str>) -> (Sandbox, PathBuf, PathBuf) {
        let mut sandbox = Sandbox::new(tag);
        sandbox.stub_contract = "fast";
        sandbox.stub_d3 = true;
        sandbox.stub_status_commit = true;
        let (repo, bare) = sandbox.repo("acme", "widget", &[]);
        sandbox.locked_entry("acme", "widget", "c0ffee");
        sandbox.entry_says(
            "acme",
            "widget",
            visibility.map(|v| format!("visibility = \"{v}\"")),
        );
        sandbox.apply();
        let tree = sandbox.tree(&repo, "HEAD");
        fs::write(sandbox.dir.join("tree"), &tree).unwrap();
        (sandbox, repo, bare)
    }

    /// What an entry says about sharing its Judgements: its `sharing.toml`
    /// holds the lines given, or is removed (`None`). `repo.toml` holds only
    /// the branch, so an older ohjaa still reads it.
    fn entry_says(&self, owner: &str, name: &str, line: Option<String>) {
        let toml = self
            .config()
            .join("repos")
            .join(owner)
            .join(name)
            .join("sharing.toml");
        match line {
            Some(line) => fs::write(toml, format!("{line}\n")).unwrap(),
            None => {
                let _ = fs::remove_file(toml);
            }
        }
    }

    /// `ohjaa run sh` in `cwd` whose agent runs `script`, briefed (the
    /// briefing toimija was handed is in `$SANDBOX/toimija-argv`), with
    /// `env` beside the sandbox's: its output.
    fn session_with(&self, cwd: &Path, intent: &str, script: &str, env: &[(&str, &str)]) -> Output {
        let mut command = self.command("ohjaa", cwd);
        command
            .args(["run", "sh", "--intent", intent, "--", "-c", script])
            .env("STUB_SYKLI_CAPTURE", "1");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    /// The briefing the last `ohjaa run` handed toimija.
    fn last_briefing(&self) -> String {
        let argv = self.read("toimija-argv");
        let lines: Vec<&str> = argv.lines().collect();
        let at = lines
            .iter()
            .position(|line| *line == "--briefing")
            .unwrap_or_else(|| panic!("no --briefing in {argv}"));
        // The briefing is one argument; toimija-argv holds one per line, so
        // it runs until the next flag toimija was handed.
        lines[at + 1..]
            .iter()
            .take_while(|line| !line.starts_with("--"))
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The id of the session whose intent is `intent`.
    fn session_id(&self, cwd: &Path, intent: &str) -> String {
        self.export(cwd)["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["intent"] == intent)
            .and_then(|s| s["id"].as_str())
            .unwrap_or_else(|| panic!("no session {intent:?}"))
            .to_owned()
    }

    /// The record of the session whose intent is `intent`.
    fn session_record(&self, cwd: &Path, intent: &str) -> serde_json::Value {
        self.export(cwd)["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["intent"] == intent)
            .cloned()
            .unwrap_or_else(|| panic!("no session {intent:?}"))
    }

    /// Ann's work, made in an ohjaa session on `branch` (two commits, then
    /// back to main): the session's id and the branch's head.
    fn authored(&self, repo: &Path, branch: &str) -> (String, String) {
        let intent = format!("write {branch}");
        let script = format!(
            "git checkout -q -b {branch} && echo a > {0}-a && git add {0}-a && git commit -qm 'parse a' \
             && echo b > {0}-b && git add {0}-b && git commit -qm 'parse b' && git checkout -q main",
            branch.replace('/', "-")
        );
        let output = self.session_with(repo, &intent, &script, &ANN);
        assert!(output.status.success(), "{}", text(&output).1);
        let head = self.git(repo, &["rev-parse", branch]);
        (self.session_id(repo, &intent), head)
    }

    /// `ohjaa judge <args>` in `cwd` as a person with identity `who`,
    /// outside any session, and with no controlling terminal (a session of
    /// its own, setsid(2)), whether or not the test runner has one: a person
    /// claimed, not shown.
    fn judge_as(&self, cwd: &Path, who: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = self.command("ohjaa", cwd);
        command.arg("judge").args(args);
        for (key, value) in who {
            command.env(key, value);
        }
        without_terminal(&mut command);
        command.output().unwrap()
    }

    /// The same, at a terminal of its own: a person shown as far as ohjaa
    /// can show one.
    fn judge_at_terminal(&self, cwd: &Path, who: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = self.command("ohjaa", cwd);
        command.arg("judge").args(args);
        for (key, value) in who {
            command.env(key, value);
        }
        with_terminal(&mut command)
    }

    /// `ohjaa judgements <commit> --json <extra>` in `cwd`.
    fn judgements(&self, cwd: &Path, commit: &str, extra: &[&str]) -> serde_json::Value {
        let output = self
            .command("ohjaa", cwd)
            .args(["judgements", commit, "--json"])
            .args(extra)
            .output()
            .unwrap();
        let (stdout, stderr) = text(&output);
        assert!(output.status.success(), "{stdout}{stderr}");
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"))
    }

    /// The Judgements note on `commit` in `repo`, its lines (empty: none).
    fn note_lines(&self, repo: &Path, commit: &str) -> Vec<String> {
        let output = self.git_out(repo, &["notes", "--ref", JUDGEMENTS_REF, "show", commit]);
        if !output.status.success() {
            return Vec::new();
        }
        text(&output)
            .0
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// What sykli says of `commit` (`status --commit`): `verdict`, with one
    /// current receipt of `tree` made by run `run` when `run` is given.
    fn commit_status(
        &self,
        commit: &str,
        tree: &str,
        verdict: &str,
        run: Option<(&str, &str, &str)>,
    ) {
        let dir = self.dir.join("commit-status");
        fs::create_dir_all(&dir).unwrap();
        let (current, runs) = match run {
            Some((run, path, digest)) => (
                format!(
                    "[{{\"receipt\":\"{path}\",\"digest\":\"{digest}\",\"run\":\"{run}\",\
                     \"started_at_ms\":1,\"outcome\":\"{verdict}\",\"judgement\":\"verified\",\
                     \"stage\":null,\"refusal\":null}}]"
                ),
                format!(
                    "[{{\"run\":\"{run}\",\"state\":\"completed\",\"outcome\":\"{verdict}\",\
                     \"dirty\":false,\"receipt\":{{\"path\":\"{path}\",\"digest\":\"{digest}\"}},\
                     \"tasks\":[]}}]"
                ),
            ),
            None => ("[]".to_owned(), "[]".to_owned()),
        };
        fs::write(
            dir.join(commit),
            format!(
                "{{\"schema\":\"sykli-commit-status.v1\",\"commit\":\"{commit}\",\"tree\":\"{tree}\",\
                 \"contract\":{{\"path\":\"x\",\"hash\":\"c0ffee\"}},\"verdict\":\"{verdict}\",\
                 \"current\":{current},\"historical\":[],\"runs\":{runs}}}\n"
            ),
        )
        .unwrap();
    }

    /// Another clone of `bare` (`elsewhere/<name>`, outside the roots: the
    /// marks are walked in the clone here) that pushes one commit to main:
    /// origin/main moves on elsewhere. Its new tip.
    fn pushed_elsewhere(&self, bare: &Path, name: &str, file: &str) -> String {
        let other = self.dir.join("elsewhere").join(name);
        self.git(
            &self.dir,
            &[
                "clone",
                "-q",
                &bare.to_string_lossy(),
                &other.to_string_lossy(),
            ],
        );
        let tip = self.commit(&other, file);
        self.git(&other, &["push", "-q", "origin", "main"]);
        tip
    }
}

/// BRIEF §12 and D5: `ohjaa judge` records a Judgement of a Change and its
/// exact State in ohjaa's log: who made the Change (its commits' authors, and
/// the ohjaa sessions the log attributes them to), the reviewer, the Intent,
/// the evidence considered as sykli's references (never a verdict copied
/// from sykli), the verdict, findings, severity, uncertainty, time and
/// provenance; and two independence facts, kept apart: the judge's
/// (procedural) and the evidence's (not established).
#[test]
fn ohjaa_judge_records_the_change_its_authors_the_reviewer_and_evidence_references() {
    let (sandbox, repo, _) = Sandbox::stage5("judge", Some("private"));
    let base = sandbox.git(&repo, &["rev-parse", "main"]);
    let (author, head) = sandbox.authored(&repo, "feat/parser");
    let tree = sandbox.tree(&repo, &head);
    let digest = "ab".repeat(32);
    sandbox.commit_status(
        &head,
        &tree,
        "passed",
        Some(("run-77", "/kept/rcpt_ab.json", &digest)),
    );
    let output = sandbox.session_with(
        &repo,
        "review the parser",
        "ohjaa judge main..feat/parser --verdict request-changes \
         --finding 'the parser drops the last line' --severity major --blocking \
         --uncertainty 'the fuzz corpus was not run'",
        &RAE,
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}{stderr}");
    let reviewer = sandbox.session_id(&repo, "review the parser");
    let author_intent = sandbox.session_record(&repo, "write feat/parser")["intent_id"].clone();

    let json = sandbox.judgements(&repo, &head, &[]);
    assert_eq!(json["schema"], "ohjaa-judgements.v1", "{json:#}");
    let judgements = json["judgements"].as_array().unwrap();
    assert_eq!(judgements.len(), 1, "{json:#}");
    let judgement = &judgements[0];
    assert!(
        judgement["id"].as_str().unwrap().starts_with("jd-"),
        "{json:#}"
    );
    // The Change and its exact State.
    assert_eq!(judgement["change"]["base"], base.as_str(), "{json:#}");
    assert_eq!(judgement["change"]["head"], head.as_str(), "{json:#}");
    assert_eq!(judgement["change"]["state"], tree.as_str(), "{json:#}");
    let commits = judgement["change"]["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2, "{json:#}");
    assert_eq!(commits[0]["commit"], head.as_str());
    assert_eq!(
        commits[0]["session"],
        author.as_str(),
        "attributed by the log"
    );
    // Who made it: the commits' authors, and the session that made them.
    assert_eq!(
        judgement["authors"]["people"],
        serde_json::json!(["Ann Author <ann@example.invalid>"]),
        "{json:#}"
    );
    assert_eq!(
        judgement["authors"]["sessions"][0]["session"],
        author.as_str()
    );
    assert_eq!(judgement["authors"]["sessions"][0]["agent"], "sh");
    assert_eq!(judgement["authors"]["unattributed"], 0);
    // The reviewer, the Intent.
    assert_eq!(
        judgement["reviewer"],
        serde_json::json!({"kind": "agent", "session": reviewer, "agent": "sh"}),
        "{json:#}"
    );
    assert_eq!(judgement["intent_id"], author_intent, "{json:#}");
    // The evidence considered: sykli's references, asked of sykli; never its
    // verdict.
    let evidence = &judgement["evidence"];
    assert_eq!(
        evidence["refs"],
        serde_json::json!([{"run": "run-77", "receipt": {"path": "/kept/rcpt_ab.json", "digest": digest}}]),
        "{json:#}"
    );
    for word in ["passed", "verified", "verdict", "outcome"] {
        assert!(
            !evidence.to_string().contains(word),
            "a copied verdict ({word}) in {evidence:#}"
        );
    }
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    assert!(
        sandbox.read("commit-asks").contains(&format!(
            "status --commit {head} --contract {}",
            contract.display()
        )),
        "{}",
        sandbox.read("commit-asks")
    );
    // The Judgement itself.
    assert_eq!(judgement["verdict"], "request_changes");
    assert_eq!(
        judgement["findings"],
        serde_json::json!(["the parser drops the last line"])
    );
    assert_eq!(judgement["severity"], "major");
    assert_eq!(judgement["blocking"], true);
    assert_eq!(judgement["uncertainty"], "the fuzz corpus was not run");
    assert!(judgement["at_ms"].as_i64().unwrap() > 0);
    assert_eq!(
        judgement["provenance"]["recorded_by"],
        format!("ohjaa {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(judgement["provenance"]["session"], reviewer.as_str());
    // Two independence facts, apart: the judge's, procedural and
    // established; the evidence's, not established, and said so.
    assert_eq!(
        judgement["independence"]["judge"]["established"], true,
        "{json:#}"
    );
    assert_eq!(
        judgement["independence"]["evidence"]["established"], false,
        "{json:#}"
    );
    assert!(
        judgement["independence"]["evidence"]["why"]
            .as_str()
            .unwrap()
            .contains("not established"),
        "{json:#}"
    );
    // Where it was read from: the log, and the note on its head.
    assert_eq!(
        judgement["sources"],
        serde_json::json!(["log", "note"]),
        "{json:#}"
    );
    // And for a person.
    let shown = sandbox.ohjaa_in(&repo, &["judgements", &head]);
    let (stdout, _) = text(&shown);
    assert!(stdout.contains("request changes"), "{stdout}");
    assert!(
        stdout.contains(&format!("by agent session {reviewer} (sh)")),
        "{stdout}"
    );
    assert!(
        stdout.contains("evidence not independently produced (not established"),
        "{stdout}"
    );
}

/// Law 2: the Agent never judges its own Change. A reviewer who made any
/// commit of the Change is refused, and nothing is recorded: an agent
/// session that made one (seen in its checkout's reflog), and a person whose
/// identity is on a commit no agent session made. A person judging an agent
/// session's work made under their identity is recorded, and never claimed
/// independent: ohjaa cannot tell the person from an agent with that identity.
#[test]
fn law_two_no_reviewer_judges_a_change_it_made() {
    let (sandbox, repo, _) = Sandbox::stage5("law2", Some("private"));

    // An agent session that made a commit of the Change, judging it.
    let output = sandbox.session_with(
        &repo,
        "judge my own",
        "git checkout -q -b feat/own && echo x > own && git add own && git commit -qm own \
         && ohjaa judge main..feat/own --verdict approve; echo JUDGE-EXIT=$?; git checkout -q main",
        &ANN,
    );
    let (stdout, stderr) = text(&output);
    assert!(stdout.contains("JUDGE-EXIT=3"), "{stdout}{stderr}");
    let said = format!("{stdout}{stderr}");
    assert!(said.contains("refused (law 2"), "{said}");
    assert!(said.contains("made by this session"), "{said}");
    let own = sandbox.git(&repo, &["rev-parse", "feat/own"]);
    assert_eq!(
        sandbox.judgements(&repo, &own, &[])["judgements"],
        serde_json::json!([])
    );
    assert!(
        sandbox.note_lines(&repo, &own).is_empty(),
        "no note for a refusal"
    );

    // A person whose identity is on a commit no session made.
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat/rae"]);
    let mut commit = sandbox.command("git", &repo);
    commit.args(["commit", "-q", "--allow-empty", "-m", "rae's own"]);
    for (key, value) in RAE {
        commit.env(key, value);
    }
    assert!(commit.output().unwrap().status.success());
    sandbox.git(&repo, &["checkout", "-q", "main"]);
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/rae", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(
        stderr.contains("refused (law 2") && stderr.contains("Rae Reviewer <rae@example.invalid>"),
        "{stderr}"
    );
    // The same, with Rae's commit the oldest of a Change longer than the
    // record keeps (200 commits): every commit is checked, not those kept.
    let mut more = sandbox.command("sh", &repo);
    more.args([
        "-c",
        "git checkout -q feat/rae && i=0; while [ $i -lt 205 ]; do \
         git commit -q --allow-empty -m \"ann $i\"; i=$((i+1)); done; git checkout -q main",
    ]);
    for (key, value) in ANN {
        more.env(key, value);
    }
    assert!(more.output().unwrap().status.success());
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/rae", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(stderr.contains("refused (law 2"), "{stderr}");

    // Rae judging Ann's session work: recorded, Rae the reviewer.
    let (_, head) = sandbox.authored(&repo, "feat/ann");
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/ann", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let json = sandbox.judgements(&repo, &head, &[]);
    assert_eq!(
        json["judgements"][0]["reviewer"],
        serde_json::json!({"kind": "person", "name": "Rae Reviewer", "email": "rae@example.invalid"}),
        "{json:#}"
    );

    // Ann judging the work an agent session made under her identity: the
    // log says the session made it; recorded, and never claimed
    // independent: ohjaa cannot tell Ann from an agent with her identity.
    let output = sandbox.judge_as(&repo, &ANN, &["main..feat/ann", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let json = sandbox.judgements(&repo, &head, &[]);
    let ann = json["judgements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["reviewer"]["name"] == "Ann Author")
        .cloned()
        .unwrap_or_else(|| panic!("{json:#}"));
    assert_eq!(
        ann["independence"]["judge"]["established"], false,
        "{ann:#}"
    );
    let why = ann["independence"]["judge"]["not_established"].to_string();
    assert!(
        why.contains("carry the reviewer's git identity, Ann Author <ann@example.invalid> (made by agent session"),
        "{why}"
    );
}

/// D5: authors may answer a Judgement, never change or remove it. A
/// response is its own record, linked to the Judgement; a second Judgement
/// is a new one beside the first.
#[test]
fn an_author_answers_beside_a_judgement_and_never_changes_it() {
    let (sandbox, repo, _) = Sandbox::stage5("respond", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/x",
            "--verdict",
            "reject",
            "--finding",
            "no tests",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let before = sandbox.judgements(&repo, &head, &[])["judgements"][0].clone();
    let id = before["id"].as_str().unwrap().to_owned();

    let mut respond = sandbox.command("ohjaa", &repo);
    respond.args(["judge", "respond", &id, "tests are in the next commit"]);
    for (key, value) in ANN {
        respond.env(key, value);
    }
    let output = respond.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);

    let json = sandbox.judgements(&repo, &head, &[]);
    assert_eq!(json["judgements"][0], before, "the Judgement is unchanged");
    let responses = json["responses"].as_array().unwrap();
    assert_eq!(responses.len(), 1, "{json:#}");
    assert!(responses[0]["id"].as_str().unwrap().starts_with("rs-"));
    assert_eq!(responses[0]["judgement"], id.as_str());
    assert_eq!(responses[0]["text"], "tests are in the next commit");
    assert_eq!(
        responses[0]["author"], true,
        "Ann's identity is on its commits"
    );
    assert_eq!(responses[0]["by"]["kind"], "person");
    // The note carries the answer too, as a line of its own.
    let lines = sandbox.note_lines(&repo, &head);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines
        .iter()
        .any(|line| line.contains("ohjaa-judgement-response-note.v1")));

    // A second Judgement is a new record; the first stays as it was.
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let json = sandbox.judgements(&repo, &head, &[]);
    assert_eq!(json["judgements"].as_array().unwrap().len(), 2, "{json:#}");
    assert!(json["judgements"].as_array().unwrap().contains(&before));

    // An answer to a Judgement that does not exist is refused.
    let output = sandbox.ohjaa_in(&repo, &["judge", "respond", "jd-1-1", "hello"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output).1);
    assert!(
        text(&output).1.contains("no Judgement jd-1-1"),
        "{}",
        text(&output).1
    );
}

/// D8 point 3: a Judgement is attached to its head as a git note under
/// refs/notes/ohjaa/judgements, one compact canonical JSON line, and nothing
/// else in the repository changes: no file, hook or config. It is pushed
/// only when asked (`--push`), with the notes ref alone; another clone on
/// another machine fetches it (`--fetch`) and reads it as shared. Notes made
/// in two clones merge, line by line, and none is lost.
#[test]
fn a_judgement_is_a_git_note_on_its_head_and_nothing_in_the_repository_changes() {
    let (sandbox, repo, bare) = Sandbox::stage5("notes", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let tree = sandbox.tree(&repo, &head);
    let refs = |dir: &Path| {
        sandbox
            .git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .filter(|line| !line.starts_with("refs/notes/"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let config = fs::read(repo.join(".git/config")).unwrap();
    let (status, hooks, before) = (
        sandbox.status(&repo),
        names_in(&repo.join(".git/hooks")),
        refs(&repo),
    );

    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("not pushed"), "{stdout}");

    assert_eq!(
        sandbox.status(&repo),
        status,
        "no file in the repository changed"
    );
    assert_eq!(
        fs::read(repo.join(".git/config")).unwrap(),
        config,
        "nor its config"
    );
    assert_eq!(names_in(&repo.join(".git/hooks")), hooks, "nor its hooks");
    assert_eq!(refs(&repo), before, "no ref but the notes ref");
    let lines = sandbox.note_lines(&repo, &head);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let note: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(
        serde_json::to_string(&note).unwrap(),
        lines[0],
        "compact and canonical"
    );
    assert_eq!(note["schema"], "ohjaa-judgement-note.v1");
    assert_eq!(note["head"], head.as_str());
    assert_eq!(note["state"], tree.as_str());
    assert_eq!(note["verdict"], "approve");
    assert_eq!(
        note["reviewer"]["name"], "Rae Reviewer",
        "a private repository names people"
    );
    // Not pushed unless asked.
    assert_eq!(sandbox.git(&bare, &["for-each-ref", "refs/notes/"]), "");

    // Pushed when asked: the notes ref alone.
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("pushed {JUDGEMENTS_REF} to origin")),
        "{stdout}"
    );
    assert_eq!(sandbox.note_lines(&bare, &head).len(), 2);
    assert_eq!(
        sandbox.git(
            &bare,
            &["for-each-ref", "--format=%(refname)", "refs/notes/"]
        ),
        JUDGEMENTS_REF
    );

    // Another clone, on another machine (a log of its own), once the branch
    // is pushed for review: fetched, and read as shared, not as its own.
    sandbox.git(&repo, &["push", "-q", "origin", "feat/x"]);
    let other = sandbox.dir.join("projects/widget-b");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    let state_b = sandbox.dir.join("state-b");
    let read_b = |extra: &[&str]| {
        let output = sandbox
            .command("ohjaa", &other)
            .args(["judgements", &head, "--json"])
            .args(extra)
            .env("XDG_STATE_HOME", &state_b)
            .output()
            .unwrap();
        let (stdout, stderr) = text(&output);
        assert!(output.status.success(), "{stdout}{stderr}");
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap()
    };
    let json = read_b(&["--fetch"]);
    let shared = json["judgements"].as_array().unwrap();
    assert_eq!(shared.len(), 2, "{json:#}");
    for judgement in shared {
        assert_eq!(
            judgement["sources"],
            serde_json::json!(["fetched"]),
            "{json:#}"
        );
    }

    // The other clone judges too and pushes; this one pushes again: the
    // notes merge line by line, and none is lost.
    let mut judge_b = sandbox.command("ohjaa", &other);
    judge_b
        .args([
            "judge",
            &format!("{}..{head}", sandbox.git(&repo, &["rev-parse", "main"])),
            "--verdict",
            "reject",
            "--push",
        ])
        .env("XDG_STATE_HOME", &state_b)
        .env("GIT_COMMITTER_NAME", "Bo Reviewer")
        .env("GIT_COMMITTER_EMAIL", "bo@example.invalid")
        .env("GIT_AUTHOR_NAME", "Bo Reviewer")
        .env("GIT_AUTHOR_EMAIL", "bo@example.invalid");
    let output = judge_b.output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        sandbox.note_lines(&bare, &head).len(),
        4,
        "{:#?}",
        sandbox.note_lines(&bare, &head)
    );
}

/// D8 point 3: a public repository's Judgements stay on this machine unless
/// its entry opts in (`share_judgements = true`), and even then its note
/// holds only ids, hashes and the verdict: no person's name or email, no
/// finding's words, no path. An entry that does not say whether it is
/// public is treated as public.
#[test]
fn a_public_repository_shares_no_judgement_unless_its_entry_opts_in_and_never_a_name() {
    let (sandbox, repo, bare) = Sandbox::stage5("public", Some("public"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/x",
            "--verdict",
            "approve",
            "--finding",
            "secret-project is slow",
        ],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("local only"), "{stdout}");
    let private_words = [
        "Rae",
        "rae@",
        "Ann",
        "ann@",
        "secret-project",
        &sandbox.dir.to_string_lossy(),
    ];
    let check = |lines: &[String]| {
        assert!(!lines.is_empty());
        for line in lines {
            for word in &private_words {
                assert!(!line.contains(word), "{word:?} in a public note: {line}");
            }
            let note: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(note["head"], head.as_str(), "{line}");
            let id = note["id"].as_str().unwrap();
            assert!(id.starts_with("jd-") || id.starts_with("rs-"), "{line}");
            assert_eq!(note["redacted"], true, "{line}");
        }
    };
    check(&sandbox.note_lines(&repo, &head));

    // Asked to push: refused, and nothing recorded.
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3), "{stdout}{stderr}");
    assert!(stderr.contains("share_judgements = true"), "{stderr}");
    assert_eq!(sandbox.git(&bare, &["for-each-ref", "refs/notes/"]), "");
    assert_eq!(
        sandbox.judgements(&repo, &head, &[])["judgements"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Its entry opts in: pushed, and still names nothing private.
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let pushed = sandbox.note_lines(&bare, &head);
    assert_eq!(pushed.len(), 2, "{pushed:#?}");
    check(&pushed);

    // An entry that does not say: public, as far as sharing goes.
    sandbox.entry_says("acme", "widget", None);
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    assert_eq!(output.status.code(), Some(3), "{}", text(&output).1);
    assert!(
        text(&output).1.contains("visibility = \"private\""),
        "{}",
        text(&output).1
    );

    // An answer in a public repository names no one either.
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let id = sandbox.judgements(&repo, &head, &[])["judgements"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut respond = sandbox.command("ohjaa", &repo);
    respond.args(["judge", "respond", &id, "secret-project is fine"]);
    for (key, value) in ANN {
        respond.env(key, value);
    }
    assert_eq!(respond.output().unwrap().status.code(), Some(0));
    check(&sandbox.note_lines(&repo, &head));
    // An Intent id is an id: free text is refused, never published.
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/x",
            "--verdict",
            "approve",
            "--intent-id",
            "secret-project",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{}", text(&output).1);
    assert!(
        text(&output).1.contains("not an intent id"),
        "{}",
        text(&output).1
    );
}

/// D8 point 3: an entry that was private and is public now never publishes
/// the notes written while it was private, which name people: the push is
/// refused while the notes ref holds any note that is not redacted.
#[test]
fn notes_written_while_private_are_never_pushed_once_the_entry_is_public() {
    let (sandbox, repo, bare) = Sandbox::stage5("went-public", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert!(sandbox.note_lines(&repo, &head)[0].contains("Rae Reviewer"));
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let output = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("not shared") && stderr.contains("not redacted"),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(&bare, &["for-each-ref", "refs/notes/"]),
        "",
        "nothing pushed"
    );
}

/// D5, BRIEF §5: when a landing reaches the observed branch, ohjaa asks sykli
/// to validate the integrated State, the landed commit's own tree: a durable
/// run of kind `integrated`, recorded before it is dispatched (invariant A),
/// in a checkout of exactly that commit; never when sykli already has a
/// current receipt of it. A candidate's pass is never the integrated
/// State's unless the trees are the same.
#[test]
fn a_landing_on_the_observed_branch_is_validated_as_the_integrated_state() {
    let (sandbox, repo, bare) = Sandbox::stage5("integrated", None);
    // A candidate, proven on its branch by a receipt of its tree.
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let candidate = sandbox.commit(&repo, "feature");
    let candidate_tree = sandbox.tree(&repo, "HEAD");
    receipt(
        &repo.join(".sykli/receipts/rcpt_feat.json"),
        &candidate_tree,
        "passed",
        1_000,
    );
    sandbox.git(&repo, &["checkout", "-q", "main"]);
    // Meanwhile origin/main moves on elsewhere.
    sandbox.pushed_elsewhere(&bare, "widget-elsewhere", "elsewhere");
    // The session merges the candidate onto it and pushes: a State no run
    // has seen. The run is held, so sykli has not said yet. (sykli says the
    // tip this clone knows at launch passed: the launch asks for nothing,
    // and the clone's one validation checkout is free at leaving.)
    let known = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    sandbox.commit_status(&known, &sandbox.tree(&repo, &known), "passed", None);
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    let output = sandbox.session_with(
        &repo,
        "land the feature",
        "git fetch -q origin && git merge -q --ff-only origin/main && \
         git merge -q --no-ff feat -m 'land the feature' && git push -q origin main",
        &[],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}{stderr}");
    let landed = sandbox.git(&repo, &["rev-parse", "main"]);
    let landed_tree = sandbox.tree(&repo, "main");
    assert_ne!(landed_tree, candidate_tree);
    assert_ne!(landed, candidate);

    // Recorded in the leaving session, as an integrated validation.
    let record = sandbox.session_record(&repo, "land the feature");
    let refs = record["validations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let of_landed: Vec<&serde_json::Value> = refs
        .iter()
        .filter(|v| v["commit"] == landed.as_str())
        .collect();
    assert_eq!(of_landed.len(), 1, "{record:#}");
    let integrated = of_landed[0];
    assert_eq!(integrated["kind"], "integrated", "{record:#}");
    assert_eq!(integrated["commit"], landed.as_str());
    assert_eq!(integrated["tree"], landed_tree.as_str());
    let run = integrated["run"]
        .as_str()
        .unwrap_or_else(|| panic!("{record:#}"));
    // Before it was dispatched (invariant A): the record sykli found.
    let at_dispatch: serde_json::Value =
        serde_json::from_str(&sandbox.read("at-dispatch-all.json")).unwrap();
    let then = at_dispatch["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["intent"] == "land the feature")
        .unwrap_or_else(|| panic!("{at_dispatch:#}"));
    let recorded = then["validations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["request"] == integrated["request"])
        .unwrap_or_else(|| panic!("not recorded before dispatch: {then:#}"));
    assert_eq!(recorded["run"], serde_json::Value::Null, "{then:#}");
    // Run in a checkout of exactly that commit, outside the repository.
    let checkout = PathBuf::from(integrated["repo"].as_str().unwrap());
    assert!(
        checkout.starts_with(sandbox.dir.join("state/ohjaa/integrated")),
        "{}",
        checkout.display()
    );
    assert_eq!(sandbox.git(&checkout, &["rev-parse", "HEAD"]), landed);
    assert_eq!(
        sandbox.git(
            &checkout,
            &["status", "--porcelain", "--", ".", ":(exclude).sykli"]
        ),
        "",
        "a clean checkout of the landed commit"
    );
    assert_eq!(
        sandbox.read(&format!("runs/{run}/cwd")).trim(),
        checkout.to_string_lossy()
    );
    assert!(
        sandbox
            .read("commit-asks")
            .contains(&format!("status --commit {landed}")),
        "sykli was asked first: {}",
        sandbox.read("commit-asks")
    );
    // The leaving record names it.
    assert!(
        stderr.contains(&format!(
            "validation {} (integrated, {} tree {}",
            integrated["request"].as_str().unwrap(),
            short(&landed),
            short(&landed_tree)
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "ohjaa: integrated unverified: {} on origin/main has no current sykli receipt; \
             ohjaa asked sykli to validate it",
            short(&landed)
        )),
        "{stderr}"
    );

    // The candidate's pass is not the landing's: integrated unverified,
    // with the request named, until sykli says.
    let marks = |sandbox: &Sandbox| {
        let output = sandbox.ohjaa_in(
            &repo,
            &["report", "--json", "--marks", "--root", &sandbox.projects()],
        );
        let (stdout, stderr) = text(&output);
        serde_json::from_str::<serde_json::Value>(&stdout)
            .unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"))
    };
    let json = marks(&sandbox);
    let entry = &json["entries"][0];
    assert_eq!(entry["tip"], landed.as_str(), "{json:#}");
    assert_eq!(entry["commits"][0]["integrated"], "unverified", "{json:#}");
    assert_eq!(entry["integrated"]["status"], "unverified", "{json:#}");
    let requested = entry["integrated"]["requested"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["commit"] == landed.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("{json:#}"));
    assert_eq!(requested["request"], integrated["request"], "{json:#}");
    assert_eq!(requested["run"], run, "{json:#}");

    // sykli ran the landed tree and it passed: integrated passed.
    fs::remove_file(sandbox.dir.join("hold-runs")).unwrap();
    fs::write(sandbox.dir.join("tree"), &landed_tree).unwrap();
    let _ = sandbox.brief(&repo); // asks sykli about the pending run
    let json = marks(&sandbox);
    assert_eq!(
        json["entries"][0]["commits"][0]["integrated"], "passed",
        "{json:#}"
    );
    assert_eq!(
        json["entries"][0]["integrated"]["status"], "passed",
        "{json:#}"
    );

    // A candidate landed as it is (a fast-forward) has the same tree: sykli
    // has a current receipt of it, so no run is asked for, and it is said.
    // (sykli now says the landing above passed: no launch asks for it.)
    sandbox.commit_status(&landed, &landed_tree, "passed", None);
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat2"]);
    let same = sandbox.commit(&repo, "feature-two");
    sandbox.commit_status(&same, &sandbox.tree(&repo, "HEAD"), "passed", None);
    sandbox.git(&repo, &["checkout", "-q", "main"]);
    let output = sandbox.session_with(
        &repo,
        "land as it is",
        "git merge -q --ff-only feat2 && git push -q origin main",
        &[],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}{stderr}");
    let record = sandbox.session_record(&repo, "land as it is");
    assert_eq!(
        record["validations"],
        serde_json::Value::Null,
        "no run asked for: {record:#}"
    );
    assert!(
        stderr.contains(&format!(
            "integrated passed: {} on origin/main",
            short(&same)
        )),
        "{stderr}"
    );
}

/// D5: the briefing shows what sykli says of the integrated State (the
/// observed branch's tip) and of the candidate here (HEAD): passed, failed
/// or unverified, each named. A launch asks sykli to validate an unverified
/// tip once, recorded before it is dispatched; a dry run only says it would.
#[test]
fn the_briefing_shows_candidate_and_integrated_states_and_asks_once_for_an_unverified_tip() {
    let (sandbox, repo, _) = Sandbox::stage5("brief5", None);
    let tip = sandbox.commit(&repo, "unproven");
    sandbox.git(&repo, &["push", "-q", "origin", "main"]);
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();

    // A dry run says it would ask, and dispatches nothing.
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "look", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains(&format!(
            "integrated unverified: main of acme/widget at {}",
            short(&tip)
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("would ask sykli to validate it"),
        "{stderr}"
    );
    assert!(
        names_in(&sandbox.dir.join("runs")).is_empty(),
        "nothing dispatched"
    );

    // A launch asks, once: recorded, then dispatched.
    let output = sandbox.session_with(&repo, "first look", "true", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let briefing = sandbox.last_briefing();
    assert!(
        briefing.contains(&format!(
            "integrated unverified: main of acme/widget at {}",
            short(&tip)
        )),
        "{briefing}"
    );
    let record = sandbox.session_record(&repo, "first look");
    let refs = record["validations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(refs.len(), 1, "{record:#}");
    assert_eq!(refs[0]["kind"], "integrated");
    assert_eq!(refs[0]["commit"], tip.as_str());
    assert!(refs[0]["run"].is_string(), "{record:#}");
    assert!(
        briefing.contains(&format!(
            "ohjaa asks sykli to validate it: request {}",
            refs[0]["request"].as_str().unwrap()
        )),
        "{briefing}"
    );
    let at_dispatch: serde_json::Value =
        serde_json::from_str(&sandbox.read("at-dispatch-all.json")).unwrap();
    let then = at_dispatch["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["intent"] == "first look")
        .unwrap()
        .clone();
    assert_eq!(
        then["validations"][0]["run"],
        serde_json::Value::Null,
        "recorded before dispatch: {then:#}"
    );

    // While it runs, sykli says pending: not asked again.
    sandbox.commit_status(&tip, "", "pending", None);
    let output = sandbox.session_with(&repo, "second look", "true", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let briefing = sandbox.last_briefing();
    assert!(
        briefing.contains("integrated unverified") && briefing.contains("a run of it is pending"),
        "{briefing}"
    );
    assert_eq!(
        sandbox.session_record(&repo, "second look")["validations"],
        serde_json::Value::Null
    );

    // Passed, then failed.
    sandbox.commit_status(&tip, "", "passed", None);
    assert!(sandbox.brief(&repo).contains(&format!(
        "integrated passed: main of acme/widget at {}",
        short(&tip)
    )));
    sandbox.commit_status(&tip, "", "failed", None);
    assert!(sandbox.brief(&repo).contains(&format!(
        "integrated failed: main of acme/widget at {}",
        short(&tip)
    )));

    // The candidate here: HEAD on a branch ahead of the tip.
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let head = sandbox.commit(&repo, "candidate");
    sandbox.commit_status(&head, "", "passed", None);
    let brief = sandbox.brief(&repo);
    assert!(
        brief.contains(&format!("candidate passed: HEAD {} (feat)", short(&head))),
        "{brief}"
    );
    sandbox.commit_status(&head, "", "failed", None);
    let brief = sandbox.brief(&repo);
    assert!(
        brief.contains(&format!("candidate failed: HEAD {} (feat)", short(&head))),
        "{brief}"
    );
}

/// BRIEF §5 and D5, observe-mark-flag: a branch validated when the observed
/// branch was at B, pushed after it moved on, is pushed and flagged "base
/// moved since validation", and the briefing in its checkout says so too.
/// Never refused. A branch validated on the current base is not flagged.
#[test]
fn a_base_that_moved_since_validation_is_flagged_at_push_and_in_the_briefing_never_refused() {
    let (sandbox, repo, bare) = Sandbox::stage5("stale", None);
    let base = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let candidate = sandbox.commit(&repo, "feature");
    fs::write(sandbox.dir.join("tree"), sandbox.tree(&repo, "HEAD")).unwrap();
    let output = sandbox.session_with(&repo, "validate the feature", "ohjaa gate", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    // origin/main moves on, and this clone sees it.
    let moved = sandbox.pushed_elsewhere(&bare, "widget-elsewhere", "elsewhere");
    sandbox.git(&repo, &["fetch", "-q", "origin"]);

    let output = sandbox.git_out(&repo, &["push", "-q", "origin", "feat"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "never refused: {stderr}");
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", "feat"]),
        candidate,
        "pushed"
    );
    assert!(
        stderr.contains(&format!(
            "ohjaa: base moved since validation: {} (feat) was validated when origin/main was at {}; it is at {} now",
            short(&candidate),
            short(&base),
            short(&moved)
        )),
        "{stderr}"
    );
    let brief = sandbox.brief(&repo);
    assert!(
        brief.contains(&format!(
            "base moved since validation: HEAD {} was validated when origin/main was at {}; it is at {} now",
            short(&candidate),
            short(&base),
            short(&moved)
        )),
        "{brief}"
    );

    // Validated on the current base: no flag.
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat2", "origin/main"]);
    sandbox.commit(&repo, "feature-two");
    fs::write(sandbox.dir.join("tree"), sandbox.tree(&repo, "HEAD")).unwrap();
    let output = sandbox.session_with(&repo, "validate feature two", "ohjaa gate", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    let output = sandbox.git_out(&repo, &["push", "-q", "origin", "feat2"]);
    let (_, stderr) = text(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("base moved"), "{stderr}");
    assert!(!sandbox.brief(&repo).contains("base moved"));
}

/// D5, audit finding 9: concurrent sessions are counted across every
/// worktree of a clone, not only the checkout a session is launched in, and
/// the briefing names them.
#[test]
fn sessions_in_every_worktree_of_the_clone_are_counted_and_named_at_launch() {
    let sandbox = Sandbox::new("clone-sessions");
    let (repo, _) = sandbox.repo("acme", "widget", &[]);
    let sibling = sandbox.dir.join("projects/widget-lexer");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lexer",
            &sibling.to_string_lossy(),
        ],
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let listed = sandbox.dir.join("toimija-sessions");
    fs::create_dir_all(&listed).unwrap();
    fs::write(
        listed.join("widget-lexer"),
        format!(
            "{{\"sessions\":[{{\"id\":\"1790-42\",\"agent\":[\"codex\"],\"worktree\":\"{}\",\
             \"branch\":\"lexer\",\"intent\":\"rework the lexer\",\"scope\":[],\
             \"liveness\":\"active\",\"pid_check\":\"alive\",\"last_seen_at\":{now}}}],\
             \"warnings\":[],\"coverage\":{{\"status\":\"available\"}}}}",
            sibling.display()
        ),
    )
    .unwrap();
    let output = sandbox.ohjaa_in(&repo, &["run", "claude", "--intent", "x", "--dry-run"]);
    let (_, stderr) = text(&output);
    assert!(
        stderr.contains("ohjaa: 0 sibling session(s) in this checkout at launch"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ohjaa: 1 more session(s) in other worktrees of this clone at launch: 1790-42 (codex, widget-lexer)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Sessions in the other worktrees of this clone at launch")
            && stderr.contains("1790-42 (codex) in ")
            && stderr.contains("on lexer")
            && stderr.contains("rework the lexer"),
        "named in the briefing: {stderr}"
    );
}

/// The git processes ohjaa itself started: trace2's top-level sessions. A
/// git that git started (a fetch's `upload-pack`, a push's `receive-pack`,
/// an automatic `maintenance`) carries its parent's session id before a `/`.
fn own(starts: &[String]) -> usize {
    starts
        .iter()
        .filter(|line| {
            line.split_once("\"sid\":\"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .is_some_and(|(sid, _)| !sid.contains('/'))
        })
        .count()
}

/// "We don't want to run git git git", Stage 5: the Judgement and its notes,
/// and the stale-base check on the hook path, each measured as git's trace2
/// counts it (ohjaa's own processes; the transport's are git's), with a
/// positive control that it did what it says.
#[test]
fn judgements_and_the_stale_base_check_stay_within_their_git_budget() {
    let (sandbox, repo, bare) = Sandbox::stage5("budget5", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let mut report = Vec::new();
    let mut budget = |what: &str, starts: usize, at_most: usize| {
        eprintln!("{what:<52} {starts} git processes (budget {at_most})");
        report.push((what.to_owned(), starts, at_most));
    };
    let with = |command: &mut Command, who: &[(&str, &str)]| {
        for (key, value) in who {
            command.env(key, value);
        }
    };

    // A person's Judgement: the clone's config, the identity, the Change
    // (its ends, its commits) and the note.
    let mut judge = sandbox.command("ohjaa", &repo);
    judge.args(["judge", "main..feat/x", "--verdict", "approve"]);
    with(&mut judge, &RAE);
    let (starts, output) = sandbox.git_starts(&mut judge);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        sandbox.note_lines(&repo, &head).len(),
        1,
        "the note was written"
    );
    assert!(started(&starts, "notes"), "{starts:#?}");
    budget("ohjaa judge (a person)", own(&starts), 6);

    // Inside a session: also the reviewer's own commits (its checkout's
    // reflog and the clone's branches).
    let started_flag = sandbox.dir.join("reviewer-started");
    let stop = sandbox.dir.join("reviewer-stop");
    let mut session = std::os::unix::process::CommandExt::process_group(
        sandbox
            .command("ohjaa", &repo)
            .args([
                "run",
                "sh",
                "--intent",
                "hold a review",
                "--no-briefing",
                "--",
                "-c",
                &format!(
                    "touch '{}'; while [ ! -f '{}' ]; do sleep 0.05; done",
                    started_flag.display(),
                    stop.display()
                ),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
        0,
    )
    .spawn()
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !started_flag.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the session never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let reviewer = sandbox.session_id(&repo, "hold a review");
    let mut judge = sandbox.command("ohjaa", &repo);
    judge
        .args(["judge", "main..feat/x", "--verdict", "approve"])
        .env("OHJAA_SESSION", &reviewer);
    let (starts, output) = sandbox.git_starts(&mut judge);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.note_lines(&repo, &head).len(), 2);
    assert!(
        started(&starts, "--walk-reflogs"),
        "its checkout's reflog: {starts:#?}"
    );
    budget("ohjaa judge (inside a session)", own(&starts), 6);
    fs::write(&stop, "").unwrap();
    session.wait().unwrap();

    // Pushed: the remote's notes fetched, merged, and the ref pushed.
    let mut judge = sandbox.command("ohjaa", &repo);
    judge.args(["judge", "main..feat/x", "--verdict", "approve", "--push"]);
    with(&mut judge, &RAE);
    let (starts, output) = sandbox.git_starts(&mut judge);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(sandbox.note_lines(&bare, &head).len(), 3, "pushed");
    budget("ohjaa judge --push", own(&starts), 10);

    // An answer: the identity and the note.
    let id = sandbox.judgements(&repo, &head, &[])["judgements"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut respond = sandbox.command("ohjaa", &repo);
    respond.args(["judge", "respond", &id, "thanks"]);
    with(&mut respond, &ANN);
    let (starts, output) = sandbox.git_starts(&mut respond);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(
        sandbox.note_lines(&repo, &head).len(),
        4,
        "the answer's note"
    );
    budget("ohjaa judge respond", own(&starts), 3);

    // Reading: the commit, and the notes of each source.
    let (starts, output) =
        sandbox.git_starts(sandbox.command("ohjaa", &repo).args(["judgements", &head]));
    assert!(text(&output).0.contains(&id), "{}", text(&output).0);
    budget("ohjaa judgements <commit>", own(&starts), 3);
    let (starts, output) =
        sandbox.git_starts(
            sandbox
                .command("ohjaa", &repo)
                .args(["judgements", &head, "--fetch"]),
        );
    assert!(text(&output).0.contains(&id), "{}", text(&output).0);
    assert!(started(&starts, "fetch"), "{starts:#?}");
    budget("ohjaa judgements <commit> --fetch", own(&starts), 4);

    // The hook path: a push of a branch validated on a base that moved since
    // starts no git process beyond the push itself, and says so.
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat/stale"]);
    sandbox.commit(&repo, "stale");
    fs::write(sandbox.dir.join("tree"), sandbox.tree(&repo, "HEAD")).unwrap();
    let output = sandbox.session_with(&repo, "validate stale", "ohjaa gate", &[]);
    assert!(output.status.success(), "{}", text(&output).1);
    sandbox.git(&repo, &["checkout", "-q", "main"]);
    sandbox.pushed_elsewhere(&bare, "widget-moved", "moved");
    sandbox.git(&repo, &["fetch", "-q", "origin"]);
    let (through, output) = sandbox.git_starts(sandbox.command("git", &repo).args([
        "push",
        "-q",
        "origin",
        "feat/stale",
    ]));
    assert!(output.status.success(), "{}", text(&output).1);
    assert!(
        text(&output)
            .1
            .contains("ohjaa: base moved since validation"),
        "{}",
        text(&output).1
    );
    sandbox.git(&repo, &["branch", "-q", "plain", "feat/stale"]);
    let (direct, output) = sandbox.git_starts(sandbox.command("git", &repo).args([
        "-c",
        "core.hooksPath=/dev/null",
        "push",
        "-q",
        "origin",
        "plain",
    ]));
    assert!(output.status.success(), "{}", text(&output).1);
    budget(
        "ohjaa hook pre-push (a base that moved)",
        through.len().saturating_sub(direct.len()),
        0,
    );

    // The briefing's validation lines read HEAD from git's files: no git
    // process beyond the same dry run from a sykli without `status
    // --commit`; sykli is asked, about the tip and about HEAD.
    sandbox.git(&repo, &["checkout", "-q", "feat/x"]);
    let asks = || sandbox.read("commit-asks").lines().count();
    let dry_run = |status_commit: bool| {
        let mut command = sandbox.command("ohjaa", &repo);
        command.args(["run", "claude", "--intent", "look", "--dry-run"]);
        if !status_commit {
            command.env_remove("STUB_SYKLI_STATUS_COMMIT");
        }
        let before = asks();
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(
            text(&output).1.contains("ohjaa: dry-run; would run:"),
            "{}",
            text(&output).1
        );
        (own(&starts), asks() - before, text(&output).1)
    };
    let (with, asked, said) = dry_run(true);
    assert_eq!(asked, 2, "the tip and HEAD: {said}");
    assert!(said.contains("candidate unverified: HEAD "), "{said}");
    let (without, none, _) = dry_run(false);
    assert_eq!(none, 0);
    budget(
        "ohjaa run's briefing (integrated and candidate)",
        with.saturating_sub(without),
        0,
    );
    sandbox.git(&repo, &["checkout", "-q", "main"]);

    // A launch that asks for an integrated validation of the tip: at most
    // three git processes for the clone's one validation checkout (made:
    // `worktree add` and `status`; reused: `checkout -f`, `clean` and
    // `status`, it being verified clean at the commit before every run).
    let tip = sandbox.git(&repo, &["rev-parse", "origin/main"]);
    let launch = |verdict: &str| {
        sandbox.commit_status(&tip, "", verdict, None);
        let mut command = sandbox.command("ohjaa", &repo);
        command.args(["run", "sh", "--intent", "launch", "--", "-c", "true"]);
        let (starts, output) = sandbox.git_starts(&mut command);
        assert!(output.status.success(), "{}", text(&output).1);
        (starts, text(&output).1)
    };
    let (asking, said) = launch("none");
    assert!(said.contains("ohjaa: integrated validation rq-"), "{said}");
    assert!(
        started(&asking, "worktree") || started(&asking, "clean"),
        "its checkout was made or reset: {asking:#?}"
    );
    assert!(started(&asking, "status"), "and verified: {asking:#?}");
    let (pending, said) = launch("pending");
    assert!(said.contains("a run of it is pending"), "{said}");
    // Leaving reads each checkout's reflog for the work made there, never a
    // validation checkout's: nothing is made in one, and they accumulate.
    let reads = |path: &Path| {
        let path = path.to_string_lossy().into_owned();
        pending
            .iter()
            .filter(|line| line.contains("\"--walk-reflogs\"") && line.contains(&path))
            .count()
    };
    assert!(reads(&repo) > 0, "leaving read the checkout's reflog");
    assert_eq!(
        reads(&sandbox.dir.join("state/ohjaa/integrated")),
        0,
        "nor a validation checkout's"
    );
    // The request's own git processes: those that name ohjaa's validation
    // checkouts (leaving's work varies with what the window holds, so the
    // two launches are not subtracted).
    let checkouts = sandbox
        .dir
        .join("state/ohjaa/integrated")
        .to_string_lossy()
        .into_owned();
    let naming: Vec<&String> = asking
        .iter()
        .filter(|line| line.contains(&checkouts))
        .collect();
    assert!(
        naming.iter().all(|line| [
            "\"worktree\",\"add\"",
            "\"checkout\",\"-q\",\"-f\",\"--detach\"",
            "\"clean\",\"-q\",\"-ffdx\",\"-e\",\"/.sykli\"",
            "\"status\",\"--porcelain=v1\""
        ]
        .iter()
        .any(|step| line.contains(step))),
        "{naming:#?}"
    );
    budget(
        "ohjaa run asking for an integrated validation",
        naming.len(),
        3,
    );

    let over: Vec<_> = report
        .iter()
        .filter(|(_, starts, at_most)| starts > at_most)
        .collect();
    assert!(over.is_empty(), "over budget: {over:?}");
}

/// D5 against sykli's main: a landing is validated as the integrated State,
/// in a checkout of exactly the landed commit, and the marks then prove it;
/// the candidate's own pass never did, its tree being another.
#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_landing_is_validated_as_the_integrated_state() {
    let (sandbox, repo, bare) = Sandbox::with_real_sykli("real-integrated");
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"private\"".to_owned()),
    );
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let candidate = sandbox.state(&repo, "ok, the feature", "feature");
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    sandbox.git(&repo, &["checkout", "-q", "main"]);
    sandbox.pushed_elsewhere(&bare, "widget-elsewhere", "elsewhere");
    let output = sandbox.session_with(
        &repo,
        "land the feature",
        "git fetch -q origin && git merge -q --ff-only origin/main && \
         git merge -q --no-ff feat -m 'land the feature' && git push -q origin main",
        &[],
    );
    let (stdout, stderr) = text(&output);
    assert!(output.status.success(), "{stdout}{stderr}");
    let landed = sandbox.git(&repo, &["rev-parse", "main"]);
    assert_ne!(
        sandbox.tree(&repo, &landed),
        sandbox.tree(&repo, &candidate)
    );
    let record = sandbox.session_record(&repo, "land the feature");
    let integrated = record["validations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["commit"] == landed.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("no validation of the landing: {record:#}"));
    assert_eq!(integrated["kind"], "integrated", "{record:#}");
    assert!(integrated["run"].is_string(), "{record:#}");
    // sykli's own word on the landed commit, once the run is done.
    let contract = sandbox.config().join("repos/acme/widget/sykli.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let verdict = loop {
        let output = sandbox
            .command("sykli", &repo)
            .args(["status", "--commit", &landed, "--contract"])
            .arg(&contract)
            .arg("--json")
            .output()
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
        let verdict = status["verdict"].as_str().unwrap_or("").to_owned();
        if verdict != "pending" && verdict != "none" || std::time::Instant::now() > deadline {
            break verdict;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(verdict, "passed");
    let json = sandbox.report_json(&repo);
    let tip = &json["entries"][0]["commits"][0];
    assert_eq!(tip["sha"], landed.as_str(), "{json:#}");
    assert_eq!(tip["mark"], "proven", "{json:#}");
    assert_eq!(tip["integrated"], "passed", "{json:#}");

    // Another landing: the clone's one validation checkout is rolled to it,
    // and the first run's receipt is kept in ohjaa's state first, with the
    // digest sykli gives it; sykli still proves the first landing after
    // the reset (its receipts stay in the checkout's .sykli/).
    let output = sandbox.session_with(
        &repo,
        "land more",
        "echo 'ok, more' > state.txt && git commit -q -am more && git push -q origin main",
        &[],
    );
    assert!(output.status.success(), "{}", text(&output).1);
    let again = sandbox.git(&repo, &["rev-parse", "main"]);
    let second = sandbox.session_record(&repo, "land more")["validations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["commit"] == again.as_str())
        .cloned()
        .unwrap_or_else(|| panic!("no validation of the second landing"));
    assert_eq!(second["repo"], integrated["repo"], "one rolling checkout");
    let first = sandbox.session_record(&repo, "land the feature")["validations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["commit"] == landed.as_str())
        .cloned()
        .unwrap();
    let kept = PathBuf::from(
        first["receipt"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("the first run's receipt was not kept: {first:#}")),
    );
    assert!(
        kept.starts_with(sandbox.dir.join("state/ohjaa/integrated")) && kept.is_file(),
        "{}",
        kept.display()
    );
    assert_eq!(first["receipt"]["digest"].as_str().unwrap().len(), 64);
    let output = sandbox
        .command("sykli", &repo)
        .args(["status", "--commit", &landed, "--contract"])
        .arg(&contract)
        .arg("--json")
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    assert_eq!(status["verdict"], "passed", "{status:#}");
}

/// D5 against sykli's main: the evidence a Judgement considered is sykli's
/// references (its run and receipt), read from `sykli status --commit`, and
/// sykli still answers for that run.
#[test]
#[ignore = "needs a real sykli: OHJAA_TEST_SYKLI=<path> cargo test -- --ignored"]
fn real_sykli_a_judgement_references_the_run_sykli_keeps() {
    let (sandbox, repo, _) = Sandbox::with_real_sykli("real-judge");
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"private\"".to_owned()),
    );
    sandbox.git(&repo, &["checkout", "-q", "-b", "feat"]);
    let mut commit = sandbox.command("git", &repo);
    fs::write(repo.join("state.txt"), "ok, Ann's\n").unwrap();
    commit.args(["commit", "-q", "-am", "ann's change"]);
    for (key, value) in ANN {
        commit.env(key, value);
    }
    assert!(commit.output().unwrap().status.success());
    let head = sandbox.git(&repo, &["rev-parse", "HEAD"]);
    let (code, said) = sandbox.gate(&repo);
    assert_eq!(code, Some(0), "{said}");
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat", "--verdict", "approve"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let json = sandbox.judgements(&repo, &head, &[]);
    let refs = json["judgements"][0]["evidence"]["refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let run = refs
        .iter()
        .find_map(|r| r["run"].as_str())
        .unwrap_or_else(|| panic!("no run among the evidence: {json:#}"))
        .to_owned();
    let output = sandbox
        .command("sykli", &repo)
        .args(["status", "--run", &run, "--json"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["state"], "completed", "{status:#}");
    assert_eq!(sandbox.note_lines(&repo, &head).len(), 1);
}

// ---- KATANA Stage 5, after review (#46): law 2 that never overclaims, ------
// ---- notes that are an append-only union, public notes that name no one, ---
// ---- and one rolling validation checkout per clone ---------------------------

impl Sandbox {
    /// The Judgements of `head` as this machine lists them.
    fn judgements_of(&self, repo: &Path, head: &str) -> Vec<serde_json::Value> {
        self.judgements(repo, head, &[])["judgements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The one Judgement of `head` this machine's log holds.
    fn logged_judgement(&self, repo: &Path, head: &str) -> serde_json::Value {
        let all = self.judgements_of(repo, head);
        let logged: Vec<&serde_json::Value> = all
            .iter()
            .filter(|j| {
                j["sources"]
                    .as_array()
                    .is_some_and(|sources| sources.iter().any(|s| s == "log"))
            })
            .collect();
        assert_eq!(logged.len(), 1, "{all:#?}");
        logged[0].clone()
    }
}

/// The judge's independence of a Judgement: whether it is established, and
/// why not.
fn independence_of(judgement: &serde_json::Value) -> (bool, String) {
    let judge = &judgement["independence"]["judge"];
    (
        judge["established"] == true,
        judge["not_established"].to_string(),
    )
}

/// Review of #46, finding 1 (a), (c) and (f′): the reviewer is observed by
/// its process, not its environment. Inside an agent session's process
/// group, a process that drops OHJAA_SESSION and takes a person's identity,
/// one in a session started inside it, and one that claims another live
/// session's id are each that session, which made the Change: refused, and
/// nothing recorded.
#[test]
fn law_two_a_process_inside_a_session_is_that_session_whatever_its_environment_says() {
    let (sandbox, repo, _) = Sandbox::stage5("l2-inside", Some("private"));
    // (a) OHJAA_SESSION dropped, a person's identity taken.
    let output = sandbox.session_with(
        &repo,
        "author A",
        "git checkout -q -b feat/s && echo s > s && git add s && git commit -qm 'A made this' \
         && env -u OHJAA_SESSION GIT_COMMITTER_NAME='Rae Reviewer' \
         GIT_COMMITTER_EMAIL=rae@example.invalid GIT_AUTHOR_NAME='Rae Reviewer' \
         GIT_AUTHOR_EMAIL=rae@example.invalid ohjaa judge main..feat/s --verdict approve; \
         echo SPOOF-EXIT=$?; git checkout -q main",
        &ANN,
    );
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("SPOOF-EXIT=3"), "{said}");
    assert!(
        said.contains("refused (law 2") && said.contains("made by this session"),
        "{said}"
    );
    let head = sandbox.git(&repo, &["rev-parse", "feat/s"]);
    assert!(sandbox.judgements_of(&repo, &head).is_empty());

    // (c) A session started inside the author's: the author's commits are
    // the reviewer's to answer for.
    let output = sandbox.session_with(
        &repo,
        "author N",
        "git checkout -q -b feat/n && echo n > n && git add n && git commit -qm 'A made this' \
         && sleep 1 && ohjaa run sh --intent 'nested reviewer' -- -c \
         'ohjaa judge main..feat/n --verdict approve; echo NESTED-EXIT=$?'; git checkout -q main",
        &ANN,
    );
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("NESTED-EXIT=3"), "{said}");
    assert!(
        said.contains("made by a session this process runs inside"),
        "{said}"
    );
    let head = sandbox.git(&repo, &["rev-parse", "feat/n"]);
    assert!(sandbox.judgements_of(&repo, &head).is_empty());

    // (f′) Another live session's id claimed, from inside the author's.
    let other = sandbox.dir.join("projects").join("wt-b");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            &other.to_string_lossy(),
            "-b",
            "b-work",
        ],
    );
    let ready = sandbox.dir.join("b-id");
    let stop = sandbox.dir.join("b-stop");
    let mut b = sandbox.command("ohjaa", &other);
    b.args([
        "run",
        "sh",
        "--intent",
        "other agent B",
        "--no-briefing",
        "--",
        "-c",
        &format!(
            "echo $OHJAA_SESSION > '{}'; while [ ! -f '{}' ]; do sleep 0.1; done",
            ready.display(),
            stop.display()
        ),
    ]);
    for (key, value) in RAE {
        b.env(key, value);
    }
    let b = b
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while fs::read_to_string(&ready)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        assert!(std::time::Instant::now() < deadline, "B never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = sandbox.session_with(
        &repo,
        "author I",
        &format!(
            "git checkout -q -b feat/i && echo i > i && git add i && git commit -qm 'A made this' \
             && OHJAA_SESSION=$(cat '{}') ohjaa judge main..feat/i --verdict approve; \
             echo BORROWED-EXIT=$?; git checkout -q main",
            ready.display()
        ),
        &ANN,
    );
    fs::write(&stop, "").unwrap();
    let _ = b.wait_with_output();
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("BORROWED-EXIT=3"), "{said}");
    assert!(said.contains("refused (law 2"), "{said}");
    let head = sandbox.git(&repo, &["rev-parse", "feat/i"]);
    assert!(sandbox.judgements_of(&repo, &head).is_empty());
}

/// Review of #46, finding 1 (b), (d), (e), (f) and (g): what ohjaa cannot
/// show is recorded, never claimed. An agent that drops OHJAA_SESSION after
/// its author ended is recorded as its own session, not a person; a commit
/// fetched from a scratch clone, one whose reflog was expired, one made in a
/// worktree while another session was open, and one made outside every
/// session under the reviewer's identity are recorded with the judge's
/// independence not established, and why.
#[test]
fn law_two_what_cannot_be_shown_is_recorded_and_never_claimed_independent() {
    let (sandbox, repo, _) = Sandbox::stage5("l2-unshown", Some("private"));

    // (b) After the author ended, an agent with the author's identity drops
    // OHJAA_SESSION: its own session, never "person Ann".
    let (_, head) = sandbox.authored(&repo, "feat/p");
    let output = sandbox.session_with(
        &repo,
        "reviewer B",
        "env -u OHJAA_SESSION ohjaa judge main..feat/p --verdict approve; echo AS-PERSON-EXIT=$?",
        &ANN,
    );
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("AS-PERSON-EXIT=0"), "{said}");
    let judged = sandbox.logged_judgement(&repo, &head);
    let reviewer = sandbox.session_id(&repo, "reviewer B");
    assert_eq!(judged["reviewer"]["kind"], "agent", "{judged:#}");
    assert_eq!(judged["reviewer"]["session"], reviewer.as_str());
    assert_eq!(
        judged["provenance"]["inside"],
        serde_json::json!([reviewer])
    );
    let (established, why) = independence_of(&judged);
    assert!(!established, "{judged:#}");
    assert!(why.contains("OHJAA_SESSION is not set"), "{why}");

    // (d) A commit made in a scratch clone and fetched; (e) one whose
    // reflog was expired: the log holds neither.
    let output = sandbox.session_with(
        &repo,
        "author D",
        "rm -rf ../scratch-c && git clone -q . ../scratch-c && (cd ../scratch-c && \
         git checkout -q -b feat/c && echo c > c && git add c && git commit -qm 'made elsewhere') \
         && git fetch -q ../scratch-c feat/c:feat/c && ohjaa judge main..feat/c --verdict approve; \
         echo CLONE-EXIT=$?; \
         git checkout -q -b feat/r && echo r > r && git add r && git commit -qm 'A made this' \
         && git checkout -q main && git reflog expire --expire=now --all \
         && ohjaa judge main..feat/r --verdict approve; echo EXPIRE-EXIT=$?",
        &ANN,
    );
    let said = format!("{}{}", text(&output).0, text(&output).1);
    for (branch, exit) in [("feat/c", "CLONE-EXIT"), ("feat/r", "EXPIRE-EXIT")] {
        let head = sandbox.git(&repo, &["rev-parse", branch]);
        // Refused (when what it made still shows) or recorded, never
        // claimed independent.
        if said.contains(&format!("{exit}=3")) {
            assert!(sandbox.judgements_of(&repo, &head).is_empty());
            continue;
        }
        assert!(said.contains(&format!("{exit}=0")), "{said}");
        let judged = sandbox.logged_judgement(&repo, &head);
        let (established, why) = independence_of(&judged);
        assert!(!established, "{branch}: {judged:#}");
        assert!(why.contains("no ended session's record holds"), "{why}");
        assert!(
            why.contains("carry the git identity the reviewer acts under"),
            "{why}"
        );
    }

    // (f) With another session open in the clone, a commit made in a
    // worktree the author made: contested, recorded, not established.
    let other = sandbox.dir.join("projects").join("wt-f");
    sandbox.git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            &other.to_string_lossy(),
            "-b",
            "f-work",
        ],
    );
    let ready = sandbox.dir.join("f-ready");
    let stop = sandbox.dir.join("f-stop");
    let mut b = sandbox.command("ohjaa", &other);
    b.args([
        "run",
        "sh",
        "--intent",
        "other agent F",
        "--no-briefing",
        "--",
        "-c",
        &format!(
            "touch '{}'; while [ ! -f '{}' ]; do sleep 0.1; done",
            ready.display(),
            stop.display()
        ),
    ]);
    let b = b
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline, "F never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let output = sandbox.session_with(
        &repo,
        "author W",
        "git worktree add -q ../wt-w -b feat/w && (cd ../wt-w && echo w > w && git add w && \
         git commit -qm 'A made this in its own worktree') && ohjaa judge main..feat/w \
         --verdict approve; echo WT-EXIT=$?",
        &ANN,
    );
    fs::write(&stop, "").unwrap();
    let _ = b.wait_with_output();
    let said = format!("{}{}", text(&output).0, text(&output).1);
    let head = sandbox.git(&repo, &["rev-parse", "feat/w"]);
    if !said.contains("WT-EXIT=3") {
        assert!(said.contains("WT-EXIT=0"), "{said}");
        let judged = sandbox.logged_judgement(&repo, &head);
        let (established, why) = independence_of(&judged);
        assert!(!established, "{judged:#}");
        assert!(why.contains("no ended session's record holds"), "{why}");
    }

    // (g) A commit made in the checkout outside every session, under Ann's
    // identity: Ann as a person is refused; an agent session acting under
    // her identity is recorded, not established.
    let mut outside = sandbox.command("sh", &repo);
    outside.args([
        "-c",
        "git checkout -q -b feat/o && echo o > o && git add o && \
         git commit -qm 'made outside ohjaa' && git checkout -q main",
    ]);
    for (key, value) in ANN {
        outside.env(key, value);
    }
    assert!(outside.output().unwrap().status.success());
    // A session started in the second the commit was made would own it
    // (reflogs say whole seconds): start the reviewer after.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let person = sandbox.judge_as(&repo, &ANN, &["main..feat/o", "--verdict", "approve"]);
    assert_eq!(person.status.code(), Some(3), "{}", text(&person).1);
    let output = sandbox.session_with(
        &repo,
        "reviewer as Ann",
        "ohjaa judge main..feat/o --verdict approve; echo AGENT-EXIT=$?",
        &ANN,
    );
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("AGENT-EXIT=0"), "{said}");
    let head = sandbox.git(&repo, &["rev-parse", "feat/o"]);
    let judged = sandbox.logged_judgement(&repo, &head);
    let (established, why) = independence_of(&judged);
    assert!(!established, "{judged:#}");
    assert!(why.contains("no ended session's record holds"), "{why}");
    assert!(
        why.contains("carry the git identity the reviewer acts under, Ann Author"),
        "{why}"
    );
    // And it says so where a person reads it.
    let shown = text(&sandbox.ohjaa_in(&repo, &["judgements", &head])).0;
    assert!(
        shown.contains("judge's independence not established ("),
        "{shown}"
    );
}

/// Review of #46, finding 1, the positive case: independence is established
/// when it is shown: every commit held by an ended session's record, the
/// reviewer a person outside every live session whose identity is on no
/// commit, or an agent session in its own process group as OHJAA_SESSION
/// says. A person judging their own agent's work is recorded, and not
/// claimed independent: ohjaa cannot tell them from an agent.
#[test]
fn law_two_independence_is_established_only_where_it_is_shown() {
    let (sandbox, repo, _) = Sandbox::stage5("l2-shown", Some("private"));
    let (author, head) = sandbox.authored(&repo, "feat/x");

    // Ann judging her agent's work: recorded, not established.
    let output = sandbox.judge_as(&repo, &ANN, &["main..feat/x", "--verdict", "approve"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let all = sandbox.judgements_of(&repo, &head);
    let ann = all
        .iter()
        .find(|j| j["reviewer"]["name"] == "Ann Author")
        .unwrap();
    let (established, why) = independence_of(ann);
    assert!(!established, "{ann:#}");
    assert!(why.contains("carry the reviewer's git identity"), "{why}");

    // Rae with no controlling terminal: a person claimed, not shown.
    let output = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    let all = sandbox.judgements_of(&repo, &head);
    let headless = all
        .iter()
        .find(|j| j["reviewer"]["name"] == "Rae Reviewer")
        .unwrap();
    let (established, why) = independence_of(headless);
    assert!(!established, "{headless:#}");
    assert!(why.contains("no controlling terminal"), "{why}");
    assert_eq!(headless["provenance"]["controlling_terminal"], false);

    // Rae at a terminal, outside every session, her identity on no commit.
    let output = sandbox.judge_at_terminal(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    let all = sandbox.judgements_of(&repo, &head);
    let rae = all
        .iter()
        .filter(|j| j["reviewer"]["name"] == "Rae Reviewer")
        .find(|j| j["provenance"]["controlling_terminal"] == true)
        .unwrap_or_else(|| panic!("{all:#?}"));
    let (established, why) = independence_of(rae);
    assert!(established, "{why}: {rae:#}");
    let basis = rae["independence"]["judge"]["basis"].to_string();
    assert!(
        basis.contains(&format!(
            "held by the record of an ended session ({author})"
        )),
        "{basis}"
    );
    assert!(
        basis.contains("runs inside no ohjaa session, live or ended")
            && basis.contains("no sign of an author, not proof of a person"),
        "{basis}"
    );
    assert!(
        basis.contains("cannot tell a person from an agent"),
        "{basis}"
    );
    assert_eq!(rae["provenance"]["inside"], serde_json::Value::Null);
    assert!(
        stdout.contains("the judge's independence of the authors: procedural, established"),
        "{stdout}"
    );

    // An agent session, in its own process group as OHJAA_SESSION says.
    let output = sandbox.session_with(
        &repo,
        "reviewer B",
        "ohjaa judge main..feat/x --verdict approve",
        &RAE,
    );
    assert!(output.status.success(), "{}", text(&output).1);
    let reviewer = sandbox.session_id(&repo, "reviewer B");
    let all = sandbox.judgements_of(&repo, &head);
    let b = all
        .iter()
        .find(|j| j["reviewer"]["session"] == reviewer.as_str())
        .unwrap();
    let (established, why) = independence_of(b);
    assert!(established, "{why}: {b:#}");
    assert!(
        b["independence"]["judge"]["basis"]
            .to_string()
            .contains("as OHJAA_SESSION says, and inside no other session's, live or ended"),
        "{b:#}"
    );
    assert_eq!(b["provenance"]["inside"], serde_json::json!([reviewer]));
}

/// Review of #46, finding 2: a shared Judgement cannot be replaced. Another
/// clone force-writes a line with the same id and another verdict on
/// origin; the reviewer's next push keeps the genuine line beside it (the
/// union, never git's three-way notes merge, which lets a one-sided change
/// win), and both machines see the conflict: on this one, the log's record
/// is authoritative; on one with no log, neither is.
#[test]
fn a_shared_judgement_is_never_replaced_and_a_second_line_with_its_id_is_a_conflict() {
    let (sandbox, repo, bare) = Sandbox::stage5("notes-forged", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/x",
            "--verdict",
            "request-changes",
            "--blocking",
            "--severity",
            "critical",
            "--finding",
            "it drops data",
            "--push",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let genuine = sandbox.note_lines(&repo, &head)[0].clone();
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "feat/x"]);
    // Another clone rewrites origin's note: the same id, approve.
    let other = sandbox.dir.join("elsewhere").join("attacker");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    sandbox.git(
        &other,
        &[
            "fetch",
            "-q",
            "origin",
            "refs/notes/ohjaa/judgements:refs/notes/ohjaa/judgements",
        ],
    );
    let forged = genuine
        .replace("\"blocking\":true", "\"blocking\":false")
        .replace("\"request_changes\"", "\"approve\"")
        .replace("\"critical\"", "null")
        .replace("[\"it drops data\"]", "[]");
    assert_ne!(forged, genuine);
    sandbox.git(
        &other,
        &[
            "notes",
            "--ref",
            JUDGEMENTS_REF,
            "add",
            "-f",
            "-m",
            &forged,
            &head,
        ],
    );
    sandbox.git(&other, &["push", "-q", "origin", JUDGEMENTS_REF]);

    // The reviewer pushes a later Judgement.
    let (_, _) = sandbox.authored(&repo, "feat/y");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/y", "--verdict", "approve", "--push"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let mut on_origin = sandbox.command("git", &bare);
    on_origin.args(["notes", "--ref", JUDGEMENTS_REF, "show", &head]);
    let on_origin = text(&on_origin.output().unwrap()).0;
    assert!(
        on_origin.contains(&genuine),
        "the genuine line kept: {on_origin}"
    );
    assert!(on_origin.contains(&forged), "{on_origin}");
    let here = sandbox.note_lines(&repo, &head);
    assert!(
        here.contains(&genuine) && here.contains(&forged),
        "{here:#?}"
    );

    // This machine: the log's record is authoritative, and in conflict.
    let json = sandbox.judgements(&repo, &head, &["--fetch"]);
    let listed = json["judgements"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{json:#}");
    let mine = listed.iter().find(|j| j["trust"] == "log").unwrap();
    assert_eq!(mine["verdict"], "request_changes", "{json:#}");
    assert_eq!(
        mine["sources"],
        serde_json::json!(["log", "note", "fetched"]),
        "{json:#}"
    );
    assert_eq!(mine["conflict"], true, "{json:#}");
    let theirs = listed.iter().find(|j| j["trust"] == "foreign").unwrap();
    assert_eq!(theirs["verdict"], "approve", "{json:#}");
    assert_eq!(theirs["id"], mine["id"], "{json:#}");
    assert_eq!(
        theirs["sources"],
        serde_json::json!(["merged", "fetched"]),
        "never this clone's own note: {json:#}"
    );
    assert_eq!(theirs["conflict"], true, "{json:#}");
    let shown = text(&sandbox.ohjaa_in(&repo, &["judgements", &head])).0;
    assert!(
        shown.contains("CONFLICT") && shown.contains("authoritative here"),
        "{shown}"
    );

    // Another machine, no log: two claims of one id, neither authoritative.
    let fresh = sandbox.dir.join("elsewhere").join("fresh");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &fresh.to_string_lossy(),
        ],
    );
    let output = sandbox
        .command("ohjaa", &fresh)
        .args(["judgements", &head, "--fetch", "--json"])
        .env("XDG_STATE_HOME", sandbox.dir.join("state-other"))
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", text(&output).1));
    let listed = json["judgements"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{json:#}");
    for judgement in listed {
        assert_eq!(judgement["trust"], "foreign", "{json:#}");
        assert_eq!(judgement["conflict"], true, "{json:#}");
        assert_eq!(judgement["sources"], serde_json::json!(["fetched"]));
    }
    let verdicts: std::collections::BTreeSet<&str> = listed
        .iter()
        .filter_map(|j| j["verdict"].as_str())
        .collect();
    assert_eq!(
        verdicts,
        ["approve", "request_changes"].into_iter().collect()
    );
}

/// Review of #46, findings 2 and 8: a forged line on origin that claims to
/// name a person (`"redacted": false`, an escape in its email) is shown as
/// a foreign claim, never as this clone's note, even once a push merged it
/// in and origin forgot it. Part 2, finding 7: once the entry is public, a
/// push is refused while origin's notes still hold it (in their history),
/// and says how a person would rewrite origin's ref.
#[test]
fn a_forged_line_from_origin_is_foreign_and_never_this_clones_note() {
    let (sandbox, repo, bare) = Sandbox::stage5("notes-foreign", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "feat/x"]);
    let other = sandbox.dir.join("elsewhere").join("attacker");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    let forged = format!(
        "{{\"at\":\"2026-01-01T00:00:00Z\",\"base\":\"aa\",\"blocking\":false,\"head\":\"{head}\",\
         \"id\":\"jd-1-1\",\"redacted\":false,\"reviewer\":{{\"kind\":\"person\",\"name\":\"Yair\",\
         \"email\":\"\\u001b[31mspoof@x\"}},\"schema\":\"ohjaa-judgement-note.v1\",\
         \"verdict\":\"approve\",\"independence\":{{\"judge\":\"procedural\"}}}}"
    );
    sandbox.git(
        &other,
        &[
            "notes",
            "--ref",
            JUDGEMENTS_REF,
            "add",
            "-f",
            "-m",
            &forged,
            &head,
        ],
    );
    sandbox.git(&other, &["push", "-q", "origin", JUDGEMENTS_REF]);

    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    let json = sandbox.judgements(&repo, &head, &[]);
    let listed = json["judgements"].as_array().unwrap();
    let foreign = listed.iter().find(|j| j["id"] == "jd-1-1").unwrap();
    assert_eq!(
        foreign["sources"],
        serde_json::json!(["merged", "fetched"]),
        "{json:#}"
    );
    assert_eq!(foreign["trust"], "foreign", "{json:#}");
    let shown = text(&sandbox.ohjaa_in(&repo, &["judgements", &head])).0;
    let line = shown.lines().find(|l| l.contains("jd-1-1")).unwrap();
    assert!(
        line.contains("(claimed) person Yair <?[31mspoof@x>")
            && line.contains("foreign: a shared note's claim"),
        "{shown}"
    );
    assert!(!shown.contains('\u{1b}'), "no escape reaches a terminal");
    // Origin forgets it, and this clone fetches again: still foreign, never
    // this clone's own note.
    sandbox.git(
        &other,
        &["fetch", "-q", "origin", "+refs/notes/*:refs/notes/*"],
    );
    sandbox.git(&other, &["notes", "--ref", JUDGEMENTS_REF, "remove", &head]);
    sandbox.git(&other, &["push", "-q", "-f", "origin", JUDGEMENTS_REF]);
    let json = sandbox.judgements(&repo, &head, &["--fetch"]);
    let foreign = json["judgements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == "jd-1-1")
        .cloned()
        .unwrap();
    assert_eq!(
        foreign["sources"],
        serde_json::json!(["merged"]),
        "{json:#}"
    );
    assert_eq!(foreign["trust"], "foreign");

    // The entry is public now. Origin's notes still name people in their
    // history (the forged line, and this clone's while it was private): no
    // push from here, loudly, with what a person would run.
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let origin_tip = sandbox.git(&bare, &["rev-parse", JUDGEMENTS_REF]);
    let (_, _) = sandbox.authored(&repo, "feat/y");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/y", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("origin already publishes")
            && stderr.contains(&format!(
                "git push --force origin {JUDGEMENTS_REF}:{JUDGEMENTS_REF}"
            )),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", JUDGEMENTS_REF]),
        origin_tip,
        "origin unchanged"
    );
}

/// Review of #46, finding 3 (P1): a public entry's notes commits name no
/// one: ohjaa writes every notes commit as `ohjaa <ohjaa@invalid>`, at a
/// fixed time in +0000; the reviewer's name, email and time zone are never
/// published with the ref.
#[test]
fn a_public_entrys_notes_commits_name_no_one() {
    let (sandbox, repo, bare) = Sandbox::stage5("notes-p1", Some("public"));
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let (_, _) = sandbox.authored(&repo, "feat/x");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/x", "--verdict", "approve", "--push"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let mut log = sandbox.command("git", &bare);
    log.args([
        "log",
        "--format=%an <%ae> | %cn <%ce> | %ad",
        "--date=raw",
        JUDGEMENTS_REF,
    ]);
    let said = text(&log.output().unwrap()).0;
    assert!(!said.trim().is_empty());
    for line in said.lines() {
        assert_eq!(
            line, "ohjaa <ohjaa@invalid> | ohjaa <ohjaa@invalid> | 946684800 +0000",
            "{said}"
        );
    }
}

/// Review of #46, finding 3 (P2): a named line that was ever in the notes
/// ref's history is never published. Removing it (`git notes remove`) does
/// not take it out of the history; the push is refused, says so, and names
/// `ohjaa judgements --rebuild-public`, which rewrites the ref as one commit
/// of redacted lines (this machine's Judgements kept from its log) with no
/// history. Then nothing named reaches origin.
#[test]
fn a_named_line_in_the_notes_history_is_never_published() {
    let (sandbox, repo, bare) = Sandbox::stage5("notes-p2", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/x",
            "--verdict",
            "request-changes",
            "--finding",
            "PRIVATE: customer acme-bank data in fixtures/x.sql",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let private_id = sandbox.logged_judgement(&repo, &head)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    // What the refusal at the tip invites: the named note removed.
    sandbox.git(&repo, &["notes", "--ref", JUDGEMENTS_REF, "remove", &head]);
    let (_, _) = sandbox.authored(&repo, "feat/z");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/z", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "recorded, not shared: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("in the history a push would publish")
            && stderr.contains("git notes remove")
            && stderr.contains("ohjaa judgements --rebuild-public"),
        "{stderr}"
    );
    assert_eq!(
        sandbox.git(&bare, &["for-each-ref", "refs/notes/"]),
        "",
        "nothing pushed"
    );

    // Rebuilt: one commit, redacted lines only, this machine's Judgements
    // kept from its log.
    let out = sandbox.ohjaa_in(&repo, &["judgements", "--rebuild-public"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains("as one commit by ohjaa <ohjaa@invalid> with no history")
            && stdout.contains("Nothing was pushed"),
        "{stdout}"
    );
    assert_eq!(
        sandbox.git(&repo, &["rev-list", "--count", JUDGEMENTS_REF]),
        "1"
    );
    let lines = sandbox.note_lines(&repo, &head);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains(&private_id) && lines[0].contains("\"redacted\":true"),
        "{lines:#?}"
    );
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/z", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    let mut log = sandbox.command("git", &bare);
    log.args(["log", "-p", "--format=%an %ae %cn %ce %ad", JUDGEMENTS_REF]);
    let published = text(&log.output().unwrap()).0;
    for word in ["acme-bank", "Rae", "rae@", "Ann Author", "ann@", "fixtures"] {
        assert!(
            !published.contains(word),
            "{word:?} published:\n{published}"
        );
    }
    let on_origin = sandbox.note_lines(&bare, &head);
    assert!(
        on_origin.iter().any(|l| l.contains(&private_id)),
        "the private-era Judgement, redacted, from the log: {on_origin:#?}"
    );
}

/// Review of #46, finding 7: a Judgement that could not be recorded says so
/// with its own exit code (4), never 1 ("recorded, not shared"); a log busy
/// for a moment is waited for. Judgements made at once all reach the notes.
#[test]
fn a_judgement_not_recorded_exits_four_and_a_busy_log_is_waited_for() {
    let (sandbox, repo, _) = Sandbox::stage5("exit4", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let log = sandbox.dir.join("state/ohjaa/sessions.redb");
    // Busy for longer than a reader's own retry: waited for, then recorded.
    let held = redb::Database::open(&log).unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(2500));
        drop(held);
    });
    let out = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    release.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    // Broken: not recorded, exit 4.
    let saved = fs::read(&log).unwrap();
    fs::write(&log, b"not a database").unwrap();
    let out = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(4), "{stdout}{stderr}");
    assert!(stderr.contains("not recorded"), "{stderr}");
    fs::write(&log, saved).unwrap();
    // Eight at once: each recorded, each line in the notes.
    let children: Vec<_> = (0..8)
        .map(|_| {
            let mut judge = sandbox.command("ohjaa", &repo);
            judge.args(["judge", "main..feat/x", "--verdict", "approve"]);
            for (key, value) in RAE {
                judge.env(key, value);
            }
            judge
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    }
    assert_eq!(sandbox.judgements_of(&repo, &head).len(), 9);
    assert_eq!(sandbox.note_lines(&repo, &head).len(), 9);
}

/// Review of #46, finding 8: how an entry's Judgements are shared is its
/// `sharing.toml`, never `repo.toml`, which an older ohjaa reads and which
/// refuses keys it does not know; `adopt --force` rewrites `repo.toml` and
/// leaves `sharing.toml` as the person wrote it.
#[test]
fn sharing_is_its_own_file_beside_repo_toml_and_adopt_keeps_it() {
    let (sandbox, repo, _) = Sandbox::stage5("sharing-toml", Some("private"));
    let entry = sandbox.config().join("repos/acme/widget");
    let (_, head) = sandbox.authored(&repo, "feat/x");
    let out = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    // Private, from sharing.toml: the note names the reviewer.
    assert!(
        sandbox.note_lines(&repo, &head)[0].contains("Rae Reviewer"),
        "{:#?}",
        sandbox.note_lines(&repo, &head)
    );
    let sharing = fs::read(entry.join("sharing.toml")).unwrap();
    let output = sandbox.ohjaa(&["adopt", &repo.to_string_lossy(), "--force"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output).1);
    assert_eq!(fs::read(entry.join("sharing.toml")).unwrap(), sharing);
    let repo_toml = fs::read_to_string(entry.join("repo.toml")).unwrap();
    let keys: Vec<&str> = repo_toml
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert_eq!(keys, vec!["branch = \"main\""], "{repo_toml}");
    // A sharing key in repo.toml is refused, and says where it belongs.
    fs::write(
        entry.join("repo.toml"),
        "branch = \"main\"\nvisibility = \"private\"\n",
    )
    .unwrap();
    let out = sandbox.judge_as(&repo, &RAE, &["main..feat/x", "--verdict", "approve"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(4), "{stdout}{stderr}");
    assert!(
        stderr.contains("visibility belongs in sharing.toml"),
        "{stderr}"
    );
}

/// Review of #46, finding 4: ohjaa stopped between recording an integrated
/// request and handing it to sykli. The next launch says the request was
/// never dispatched, never that sykli was asked, and asks sykli again at
/// once (not ten minutes later); the request it replaces says so.
#[test]
fn a_request_never_dispatched_is_said_so_and_asked_again_at_the_next_launch() {
    let (sandbox, repo, _) = Sandbox::stage5("crash-dispatch", Some("private"));
    let bin = sandbox.dir.join("bin");
    fs::rename(bin.join("sykli"), bin.join("sykli-stub")).unwrap();
    write_executable(
        &bin.join("sykli"),
        "#!/bin/sh\nif [ \"$1\" = run ] && [ -f \"$SANDBOX/crash-at-dispatch\" ]; then \
         for a in \"$@\"; do [ \"$a\" = --detach ] && { echo crash >> \"$SANDBOX/crashed\"; \
         kill -9 $PPID; exit 1; }; done; fi\nexec \"$SANDBOX/bin/sykli-stub\" \"$@\"\n",
    );
    fs::write(sandbox.dir.join("crash-at-dispatch"), "").unwrap();
    let first = sandbox.session_with(&repo, "first launch", "echo AGENT-RAN", &[]);
    assert!(
        !text(&first).0.contains("AGENT-RAN"),
        "ohjaa was stopped first"
    );
    assert_eq!(sandbox.read("crashed").lines().count(), 1);
    let record = sandbox.session_record(&repo, "first launch");
    let asked = record["validations"][0].clone();
    assert_eq!(asked["kind"], "integrated", "{record:#}");
    assert_eq!(asked["run"], serde_json::Value::Null);
    assert_eq!(asked["not_dispatched"], serde_json::Value::Null);
    let detached = |sandbox: &Sandbox| {
        sandbox
            .read("sykli-calls")
            .lines()
            .filter(|l| l.contains("--detach"))
            .count()
    };
    assert_eq!(detached(&sandbox), 0);

    fs::remove_file(sandbox.dir.join("crash-at-dispatch")).unwrap();
    let second = sandbox.session_with(&repo, "second launch", "echo AGENT-RAN-2", &[]);
    let (stdout, stderr) = text(&second);
    assert!(stdout.contains("AGENT-RAN-2"), "{stdout}{stderr}");
    let briefing = sandbox.last_briefing();
    assert!(
        briefing.contains(&format!(
            "request {} was recorded at",
            asked["request"].as_str().unwrap()
        )) && briefing.contains("requested, never dispatched (ohjaa stopped before asking sykli)"),
        "{briefing}"
    );
    assert!(
        !briefing.contains("ohjaa asked sykli to validate it at"),
        "{briefing}"
    );
    assert_eq!(detached(&sandbox), 1, "asked again at once");
    let again = sandbox.session_record(&repo, "second launch")["validations"][0].clone();
    assert!(again["run"].is_string(), "{again:#}");
    let first = sandbox.session_record(&repo, "first launch")["validations"][0].clone();
    let why = first["not_dispatched"].as_str().unwrap_or_default();
    assert!(
        why.contains("ohjaa stopped before asking sykli")
            && why.contains(&format!(
                "asked again as request {}",
                again["request"].as_str().unwrap()
            )),
        "{first:#}"
    );
}

/// Review of #46, finding 5: two launches at once in two worktrees of one
/// clone, the tip unverified: one asks sykli, under the clone's lock; the
/// other finds it asked, and says so. One run, never two in one checkout.
#[test]
fn two_launches_at_once_ask_sykli_once() {
    let (sandbox, repo, _) = Sandbox::stage5("two-launches", Some("private"));
    let wt = sandbox.dir.join("projects").join("wt-2");
    sandbox.git(
        &repo,
        &["worktree", "add", "-q", &wt.to_string_lossy(), "-b", "two"],
    );
    let spawn = |cwd: &Path, intent: &str| {
        let mut command = sandbox.command("ohjaa", cwd);
        command.args(["run", "sh", "--intent", intent, "--", "-c", "sleep 1"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = spawn(&repo, "launch A");
    let b = spawn(&wt, "launch B");
    let mut said = String::new();
    for child in [a, b] {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out).1);
        said.push_str(&text(&out).1);
    }
    let detached = sandbox
        .read("sykli-calls")
        .lines()
        .filter(|l| l.contains("--detach"))
        .count();
    assert_eq!(detached, 1, "{said}");
    let export = sandbox.export(&repo);
    let requests: Vec<&serde_json::Value> = export["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["validations"].as_array().into_iter().flatten())
        .collect();
    assert_eq!(requests.len(), 1, "{export:#}");
    assert!(
        said.contains("ohjaa asked sykli to validate it at"),
        "the other found it asked: {said}"
    );
}

/// Review of #46, finding 6: one rolling validation checkout per clone,
/// `integrated/<clone>/checkout`, reset to exactly the landed commit and
/// verified clean before each run, whatever the last run left there; the
/// last run's receipt copied into ohjaa's state, with its digest, before
/// the reset; and a checkout busy with a run never reset under it.
#[test]
fn one_rolling_checkout_per_clone_reset_clean_before_each_run() {
    let (sandbox, repo, _) = Sandbox::stage5("rolling", Some("private"));
    fs::write(sandbox.dir.join("record-checkout"), "").unwrap();
    fs::write(sandbox.dir.join("dirty-the-checkout"), "").unwrap();
    let launch = |intent: &str| {
        let out = sandbox.session_with(&repo, intent, "true", &[]);
        assert!(out.status.success(), "{}", text(&out).1);
        text(&out).1
    };
    let tip0 = sandbox.git(&repo, &["rev-parse", "main"]);
    launch("launch 0");
    let first = sandbox.session_record(&repo, "launch 0")["validations"][0].clone();
    let run0 = first["run"].as_str().unwrap().to_owned();
    let tip1 = sandbox.commit(&repo, "one");
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    launch("launch 1");
    let second = sandbox.session_record(&repo, "launch 1")["validations"][0].clone();
    let run1 = second["run"].as_str().unwrap().to_owned();
    // One checkout, rolled from one commit to the next.
    let checkout = PathBuf::from(first["repo"].as_str().unwrap());
    assert_eq!(second["repo"], first["repo"]);
    assert_eq!(checkout.file_name().unwrap(), "checkout");
    let clone_dir = checkout.parent().unwrap().to_path_buf();
    assert!(
        clone_dir.starts_with(sandbox.dir.join("state/ohjaa/integrated")),
        "{}",
        clone_dir.display()
    );
    let worktrees = sandbox.git(&repo, &["worktree", "list", "--porcelain"]);
    let ours = worktrees
        .lines()
        .filter(|l| l.starts_with("worktree ") && l.contains("/state/ohjaa/integrated/"))
        .count();
    assert_eq!(ours, 1, "{worktrees}");
    // Each run found its commit, clean, though the one before left junk.
    assert_eq!(sandbox.read(&format!("runs/{run0}/head")).trim(), tip0);
    assert_eq!(sandbox.read(&format!("runs/{run1}/head")).trim(), tip1);
    assert_eq!(sandbox.read(&format!("runs/{run1}/status")).trim(), "");
    // The first run's receipt, kept in ohjaa's state before the reset.
    let first = sandbox.session_record(&repo, "launch 0")["validations"][0].clone();
    let kept = PathBuf::from(
        first["receipt"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("{first:#}")),
    );
    assert_eq!(
        kept,
        clone_dir
            .join("receipts")
            .join(format!("{}.json", first["request"].as_str().unwrap()))
    );
    let original = sandbox.read(&format!("runs/{run0}/receipt"));
    assert_eq!(
        fs::read_to_string(&kept).unwrap(),
        fs::read_to_string(original.trim()).unwrap()
    );
    assert_eq!(first["receipt"]["digest"].as_str().unwrap().len(), 64);
    // A run still going: the checkout is not reset under it.
    fs::write(sandbox.dir.join("hold-runs"), "").unwrap();
    sandbox.commit(&repo, "two");
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    launch("launch 2");
    let held = sandbox.session_record(&repo, "launch 2")["validations"][0]["run"]
        .as_str()
        .unwrap()
        .to_owned();
    let tip3 = sandbox.commit(&repo, "three");
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    let said = launch("launch 3");
    assert!(
        said.contains(&format!(
            "the validation checkout is busy with sykli run {held}"
        )),
        "{said}"
    );
    assert_eq!(
        sandbox.session_record(&repo, "launch 3")["validations"],
        serde_json::Value::Null
    );
    assert_ne!(sandbox.git(&checkout, &["rev-parse", "HEAD"]), tip3);
}

/// Review of #46, finding 6: cargo in ohjaa's validation checkout builds in
/// a target dir of the clone's own (`integrated/<clone>/.cargo/config.toml`
/// names `integrated/<clone>/target`, outside the checkout), never in the
/// target dir every checkout shares (`build.target-dir` in
/// `$CARGO_HOME/config.toml`): two trees validated in sequence each run
/// their own program, and a person's build in the shared dir is neither
/// used nor touched. A real cargo builds a two-line crate.
#[test]
fn each_validated_tree_runs_its_own_binary_never_a_shared_target_dirs() {
    let (sandbox, repo, _) = Sandbox::stage5("cargo-target", Some("private"));
    let real = Path::new(env!("CARGO")).parent().unwrap().to_path_buf();
    for tool in ["cargo", "rustc"] {
        write_executable(
            &sandbox.dir.join("bin").join(tool),
            &format!("#!/bin/sh\nexec '{}' \"$@\"\n", real.join(tool).display()),
        );
    }
    let shared = sandbox.dir.join("shared-target");
    fs::write(
        sandbox.dir.join("cargo/config.toml"),
        format!("[build]\ntarget-dir = \"{}\"\n", shared.display()),
    )
    .unwrap();
    let program = |says: &str| {
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/main.rs"),
            format!("fn main() {{\n    println!(\"{says}\");\n}}\n"),
        )
        .unwrap();
    };
    fs::write(repo.join(".gitignore"), "/target\nCargo.lock\n").unwrap();
    program("one");
    sandbox.git(&repo, &["add", "-A"]);
    sandbox.git(&repo, &["commit", "-q", "-m", "one"]);
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    fs::write(sandbox.dir.join("cargo-run"), "").unwrap();
    let launch = |intent: &str| {
        let out = sandbox.session_with(&repo, intent, "true", &[]);
        assert!(out.status.success(), "{}", text(&out).1);
        let run = sandbox.session_record(&repo, intent)["validations"][0]["run"]
            .as_str()
            .unwrap()
            .to_owned();
        (
            sandbox.read(&format!("runs/{run}/out")),
            sandbox.read(&format!("runs/{run}/err")),
        )
    };
    let (one, err) = launch("validate one");
    assert_eq!(one.trim(), "one", "{err}");
    // The person builds another program in their checkout, in the shared
    // target dir, after the first validation.
    program("person");
    let mut build = sandbox.command("cargo", &repo);
    build.args(["build", "-q"]);
    let built = build.output().unwrap();
    assert!(built.status.success(), "{}", text(&built).1);
    sandbox.git(&repo, &["checkout", "-q", "--", "src/main.rs"]);
    program("two");
    sandbox.git(&repo, &["commit", "-q", "-am", "two"]);
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    let (two, err) = launch("validate two");
    assert_eq!(two.trim(), "two", "{err}");
    // The person's build in the shared dir is theirs: no validation built
    // over it.
    let mut person = Command::new(shared.join("debug/probe"));
    let person = person.output().unwrap();
    assert_eq!(text(&person).0.trim(), "person");
    // Built in the clone's own target dir, named by its own cargo config.
    let integrated = sandbox.dir.join("state/ohjaa/integrated");
    let clone_dir = fs::read_dir(&integrated)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .unwrap();
    let config = fs::read_to_string(clone_dir.join(".cargo/config.toml")).unwrap();
    assert!(
        config.contains(&format!(
            "target-dir = \"{}\"",
            clone_dir.join("target").display()
        )),
        "{config}"
    );
    assert!(clone_dir.join("target/debug/probe").is_file());
}

// ---- KATANA Stage 5, after review part 2 (#46): a process left behind, ------
// ---- the kernel's word, a checkout never reset under a run, submodules, ----
// ---- a push that retries, origin's names, malformed notes -------------------

/// `script` in sh, detached from the caller: a double fork (the subshell
/// exits at once, so it is re-parented to pid 1), and with `new_session`
/// setsid(2) too. Its stdio go nowhere.
fn detached(script: &str, new_session: bool) -> String {
    let quoted = script.replace('\'', r"'\''");
    if new_session {
        format!(
            "( perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV' sh -c '{quoted}' \
             </dev/null >/dev/null 2>&1 & )"
        )
    } else {
        format!("( sh -c '{quoted}' </dev/null >/dev/null 2>&1 & )")
    }
}

/// A person's identity taken by whatever runs `ohjaa judge` after it, with
/// OHJAA_SESSION dropped.
const AS_RAE: &str = "env -u OHJAA_SESSION GIT_COMMITTER_NAME='Rae Reviewer' \
     GIT_COMMITTER_EMAIL=rae@example.invalid GIT_AUTHOR_NAME='Rae Reviewer' \
     GIT_AUTHOR_EMAIL=rae@example.invalid";

impl Sandbox {
    /// Wait (at most `secs`) for `$SANDBOX/<name>`, then read it.
    fn wait_for(&self, name: &str, secs: u64) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while !self.dir.join(name).exists() {
            assert!(std::time::Instant::now() < deadline, "no {name}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.read(name)
    }

    /// A folder holding only ohjaa and git, and a `ps` when one is given:
    /// a PATH for a judge.
    fn path_with(&self, name: &str, ps: Option<&str>) -> PathBuf {
        let dir = self.dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(self.dir.join("bin/ohjaa"), dir.join("ohjaa")).unwrap();
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("git"))
            .find(|g| g.is_file())
            .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
        std::os::unix::fs::symlink(git, dir.join("git")).unwrap();
        if let Some(ps) = ps {
            write_executable(&dir.join("ps"), ps);
        }
        dir
    }
}

/// Review part 2, finding 1: a process an author session leaves behind
/// judges its Change after the session ended. (iii) A double fork that stays
/// in the session's process group is that session: refused. (ii) One that
/// also left the group (setsid) is a person claimed, not shown (no
/// controlling terminal): recorded, never `established`. (i) The same while
/// the author still runs, its commit not yet in any record: refused.
#[test]
fn law_two_a_process_an_author_left_behind_never_judges_its_change() {
    let (sandbox, repo, _) = Sandbox::stage5("left-behind", Some("private"));

    // (iii) A double fork, still in A's group, judging once A has ended.
    let judge = format!(
        "while [ ! -f \"$SANDBOX/a3-ended\" ]; do sleep 0.1; done; cd '{}' && {AS_RAE} \
         ohjaa judge main..feat/e3 --verdict approve > \"$SANDBOX/e3.out\" 2>&1; \
         echo $? > \"$SANDBOX/e3.code\"",
        repo.display()
    );
    let script = format!(
        "git checkout -q -b feat/e3 && echo e > e3 && git add e3 && git commit -qm 'A made this' \
         && git checkout -q main && {}",
        detached(&judge, false)
    );
    let output = sandbox.session_with(&repo, "author A3", &script, &ANN);
    assert!(output.status.success(), "{}", text(&output).1);
    fs::write(sandbox.dir.join("a3-ended"), "").unwrap();
    let code = sandbox.wait_for("e3.code", 30);
    let said = sandbox.read("e3.out");
    assert_eq!(code.trim(), "3", "{said}");
    assert!(
        said.contains("refused (law 2")
            && said.contains("a session that has ended and left this process behind"),
        "{said}"
    );
    let head = sandbox.git(&repo, &["rev-parse", "feat/e3"]);
    assert!(sandbox.judgements_of(&repo, &head).is_empty());

    // (ii) A double fork that also left the group (setsid), judging once
    // A has ended: no sign of A is left, and no terminal either.
    let judge = format!(
        "while [ ! -f \"$SANDBOX/a2-ended\" ]; do sleep 0.1; done; cd '{}' && {AS_RAE} \
         ohjaa judge main..feat/e2 --verdict approve > \"$SANDBOX/e2.out\" 2>&1; \
         echo $? > \"$SANDBOX/e2.code\"",
        repo.display()
    );
    let script = format!(
        "git checkout -q -b feat/e2 && echo e > e2 && git add e2 && git commit -qm 'A made this' \
         && git checkout -q main && {}",
        detached(&judge, true)
    );
    let output = sandbox.session_with(&repo, "author A2", &script, &ANN);
    assert!(output.status.success(), "{}", text(&output).1);
    fs::write(sandbox.dir.join("a2-ended"), "").unwrap();
    let code = sandbox.wait_for("e2.code", 30);
    assert_eq!(code.trim(), "0", "{}", sandbox.read("e2.out"));
    let head = sandbox.git(&repo, &["rev-parse", "feat/e2"]);
    let judged = sandbox.logged_judgement(&repo, &head);
    assert_eq!(judged["reviewer"]["kind"], "person", "{judged:#}");
    let (established, why) = independence_of(&judged);
    assert!(!established, "{judged:#}");
    assert!(why.contains("no controlling terminal"), "{why}");

    // (i) The same while A still runs: its commit is in no record yet, and
    // a person not shown is refused while a session open here may have
    // made it.
    let judge = format!(
        "cd '{}' && {AS_RAE} ohjaa judge main..feat/e1 --verdict approve > \"$SANDBOX/e1.out\" \
         2>&1; echo $? > \"$SANDBOX/e1.code\"",
        repo.display()
    );
    let script = format!(
        "git checkout -q -b feat/e1 && echo e > e1 && git add e1 && git commit -qm 'A made this' \
         && git checkout -q main && {} ; while [ ! -f \"$SANDBOX/e1.code\" ]; do sleep 0.1; done",
        detached(&judge, true)
    );
    let output = sandbox.session_with(&repo, "author A1", &script, &ANN);
    assert!(output.status.success(), "{}", text(&output).1);
    let said = sandbox.read("e1.out");
    assert_eq!(sandbox.read("e1.code").trim(), "3", "{said}");
    assert!(said.contains("no controlling terminal"), "{said}");
    let head = sandbox.git(&repo, &["rev-parse", "feat/e1"]);
    assert!(sandbox.judgements_of(&repo, &head).is_empty());
}

/// Review part 2, finding 2: who runs where is the kernel's word, never a
/// `ps` on PATH. Inside the author's session, with a `ps` that lies about
/// its caller's group, one that fails, and none at all, the author is
/// refused all the same; and an agent session with a lying `ps` is still
/// its session, never "a person".
#[test]
fn law_two_who_runs_where_is_the_kernels_word_not_a_ps_on_path() {
    let (sandbox, repo, _) = Sandbox::stage5("kernel-word", Some("private"));
    let lying = sandbox.path_with(
        "lyingps",
        Some(
            "#!/bin/sh\n/bin/ps \"$@\" | /usr/bin/awk -v me=$PPID '$1==me {$2=1; $3=me} {print}'\n",
        ),
    );
    let failing = sandbox.path_with("failps", Some("#!/bin/sh\necho 'ps: no' >&2\nexit 1\n"));
    let none = sandbox.path_with("nops", None);
    for (n, dir) in [&lying, &failing, &none].into_iter().enumerate() {
        let branch = format!("feat/ps{n}");
        let script = format!(
            "git checkout -q -b {branch} && echo p > p{n} && git add p{n} && \
             git commit -qm 'A made this' && git checkout -q main && {AS_RAE} PATH='{}' \
             ohjaa judge main..{branch} --verdict approve; echo PS-EXIT=$?",
            dir.display()
        );
        let output = sandbox.session_with(&repo, &format!("author ps{n}"), &script, &ANN);
        let said = format!("{}{}", text(&output).0, text(&output).1);
        assert!(said.contains("PS-EXIT=3"), "{}: {said}", dir.display());
        let head = sandbox.git(&repo, &["rev-parse", &branch]);
        assert!(sandbox.judgements_of(&repo, &head).is_empty());
    }
    // An agent session B judging A's ended work as "a person", with the
    // lying ps: still session B.
    let (_, head) = sandbox.authored(&repo, "feat/later");
    let script = format!(
        "{AS_RAE} PATH='{}' ohjaa judge main..feat/later --verdict approve; echo B-EXIT=$?",
        lying.display()
    );
    let output = sandbox.session_with(&repo, "agent B", &script, &ANN);
    let said = format!("{}{}", text(&output).0, text(&output).1);
    assert!(said.contains("B-EXIT=0"), "{said}");
    let judged = sandbox.logged_judgement(&repo, &head);
    assert_eq!(judged["reviewer"]["kind"], "agent", "{judged:#}");
    assert_eq!(
        judged["reviewer"]["session"],
        sandbox.session_id(&repo, "agent B").as_str()
    );
    assert!(!independence_of(&judged).0, "{judged:#}");
}

/// Review part 2, finding 3: the validation checkout is never reset under a
/// run that may still be going, whatever sykli calls the run: one `lost`
/// whose task still runs, one whose task's process group lives on, and one
/// sykli cannot say anything of are each "busy"; nothing is asked, and the
/// checkout stays as it was. Once the run has ended, the next launch asks.
#[test]
fn the_checkout_is_never_reset_under_a_run_that_may_still_be_going() {
    let (sandbox, repo, _) = Sandbox::stage5("busy-run", Some("private"));
    let launch = |intent: &str| {
        let out = sandbox.session_with(&repo, intent, "true", &[]);
        assert!(out.status.success(), "{}", text(&out).1);
        (
            text(&out).1,
            sandbox.session_record(&repo, intent)["validations"].clone(),
        )
    };
    let (_, first) = launch("launch 0");
    let tip0 = sandbox.git(&repo, &["rev-parse", "main"]);
    let checkout = PathBuf::from(first[0]["repo"].as_str().unwrap());
    let next = |file: &str| {
        sandbox.commit(&repo, file);
        sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    };
    let still_there = |label: &str| {
        assert_eq!(
            sandbox.git(&checkout, &["rev-parse", "HEAD"]),
            tip0,
            "{label}: reset under the run"
        );
    };
    // sykli: lost, its task still running.
    fs::write(
        sandbox.dir.join("run-status-json"),
        "{\"schema\":\"sykli-run-status.v1\",\"run\":\"RUN\",\"state\":\"lost\",\
         \"subject\":{\"tree\":\"t\",\"commit\":null,\"dirty\":false},\"tasks\":[{\"name\":\"ok\",\
         \"state\":\"running\",\"attempts\":[{\"id\":\"a1\",\"started\":1,\"finished\":null,\
         \"outcome\":null,\"process_group\":null}]}],\"receipt\":null}\n",
    )
    .unwrap();
    next("one");
    let (said, asked) = launch("launch 1");
    assert!(
        said.contains("the validation checkout is busy with sykli run")
            && said.contains("its task ok still running"),
        "{said}"
    );
    assert_eq!(asked, serde_json::Value::Null);
    still_there("lost, task running");
    // sykli: interrupted, every attempt ended, but a task's process group
    // lives on.
    let mut group = Command::new("sleep");
    group.arg("30");
    std::os::unix::process::CommandExt::process_group(&mut group, 0);
    let mut group = group.spawn().unwrap();
    fs::write(
        sandbox.dir.join("run-status-json"),
        format!(
            "{{\"schema\":\"sykli-run-status.v1\",\"run\":\"RUN\",\"state\":\"interrupted\",\
             \"subject\":{{\"tree\":\"t\",\"commit\":null,\"dirty\":false}},\"tasks\":[{{\"name\":\
             \"ok\",\"state\":\"interrupted\",\"attempts\":[{{\"id\":\"a1\",\"started\":1,\
             \"finished\":2,\"outcome\":\"interrupted\",\"process_group\":{}}}]}}],\
             \"receipt\":null}}\n",
            group.id()
        ),
    )
    .unwrap();
    next("two");
    let (said, asked) = launch("launch 2");
    assert!(
        said.contains(&format!(
            "process group {} of its task ok still runs",
            group.id()
        )),
        "{said}"
    );
    assert_eq!(asked, serde_json::Value::Null);
    still_there("a process group left");
    group.kill().unwrap();
    group.wait().unwrap();
    // sykli cannot say.
    fs::remove_file(sandbox.dir.join("run-status-json")).unwrap();
    fs::write(sandbox.dir.join("run-status-fails"), "").unwrap();
    next("three");
    let (said, asked) = launch("launch 3");
    assert!(
        said.contains("sykli cannot say what became of it"),
        "{said}"
    );
    assert_eq!(asked, serde_json::Value::Null);
    still_there("sykli cannot say");
    // The run has ended: asked, and reset.
    fs::remove_file(sandbox.dir.join("run-status-fails")).unwrap();
    next("four");
    let (said, asked) = launch("launch 4");
    assert!(asked[0]["run"].is_string(), "{said}");
    assert_eq!(
        sandbox.git(&checkout, &["rev-parse", "HEAD"]),
        sandbox.git(&repo, &["rev-parse", "main"])
    );
}

/// Review part 2, finding 4: a submodule's leftovers never outlive a run.
/// The gate initialises the submodule and builds in it (ignored files), and
/// later changes a file in it: the next run finds the submodule as a fresh
/// checkout has it (not populated), and a change in it never blocks
/// validation.
#[test]
fn a_submodules_leftovers_never_reach_the_next_run() {
    let (sandbox, repo, _) = Sandbox::stage5("submodule", Some("private"));
    let sub_bare = sandbox.dir.join("remotes/acme/sub.git");
    fs::create_dir_all(&sub_bare).unwrap();
    sandbox.git(&sub_bare, &["init", "-q", "--bare", "-b", "main"]);
    let sub_work = sandbox.dir.join("elsewhere/sub");
    fs::create_dir_all(&sub_work).unwrap();
    sandbox.git(&sub_work, &["init", "-q", "-b", "main"]);
    fs::write(sub_work.join(".gitignore"), "target/\n").unwrap();
    fs::write(sub_work.join("file"), "one\n").unwrap();
    sandbox.git(&sub_work, &["add", "-A"]);
    sandbox.git(&sub_work, &["commit", "-q", "-m", "sub one"]);
    sandbox.git(
        &sub_work,
        &["remote", "add", "origin", &sub_bare.to_string_lossy()],
    );
    sandbox.git(&sub_work, &["push", "-q", "origin", "main"]);
    sandbox.git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &sub_bare.to_string_lossy(),
            "sub",
        ],
    );
    sandbox.git(&repo, &["commit", "-q", "-m", "add sub"]);
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    fs::write(sandbox.dir.join("record-checkout"), "").unwrap();
    fs::write(sandbox.dir.join("sub-work"), "").unwrap();
    let launch = |intent: &str| {
        let out = sandbox.session_with(&repo, intent, "true", &[]);
        assert!(out.status.success(), "{}", text(&out).1);
        let validation = sandbox.session_record(&repo, intent)["validations"][0].clone();
        let run = validation["run"]
            .as_str()
            .unwrap_or_else(|| panic!("{validation:#}\n{}", text(&out).1))
            .to_owned();
        run
    };
    let next = |file: &str| {
        sandbox.commit(&repo, file);
        sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
    };
    let first = launch("launch 1");
    assert!(
        sandbox.read(&format!("runs/{first}/sub")).trim().is_empty(),
        "a fresh checkout's submodule is empty"
    );
    fs::write(sandbox.dir.join("sub-dirty"), "").unwrap();
    next("one");
    let second = launch("launch 2");
    assert_eq!(
        sandbox.read(&format!("runs/{second}/sub")).trim(),
        "",
        "the first run's sub/target leftovers reached the second"
    );
    assert_eq!(sandbox.read(&format!("runs/{second}/status")).trim(), "");
    fs::remove_file(sandbox.dir.join("sub-dirty")).unwrap();
    next("two");
    // The second run changed a tracked file in the submodule: the next run
    // is asked all the same, and finds it clean.
    let third = launch("launch 3");
    assert_eq!(sandbox.read(&format!("runs/{third}/sub")).trim(), "");
    assert_eq!(sandbox.read(&format!("runs/{third}/status")).trim(), "");
}

/// Review part 2, finding 5: a `.sykli` folder a run leaves below the top
/// is a leftover like any other: removed before the next run, which is
/// asked and finds the checkout clean. Only sykli's own folder at the top
/// stays.
#[test]
fn a_sykli_folder_below_the_top_is_a_leftover_like_any_other() {
    let (sandbox, repo, _) = Sandbox::stage5("nested-sykli", Some("private"));
    fs::write(sandbox.dir.join("record-checkout"), "").unwrap();
    fs::write(sandbox.dir.join("nested-sykli"), "").unwrap();
    let launch = |intent: &str| {
        let out = sandbox.session_with(&repo, intent, "true", &[]);
        assert!(out.status.success(), "{}", text(&out).1);
        let validation = sandbox.session_record(&repo, intent)["validations"][0].clone();
        validation["run"]
            .as_str()
            .unwrap_or_else(|| panic!("not asked: {validation:#}"))
            .to_owned()
    };
    launch("launch 1");
    for (n, file) in ["one", "two"].into_iter().enumerate() {
        sandbox.commit(&repo, file);
        sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "main"]);
        let run = launch(&format!("launch {}", n + 2));
        assert_eq!(sandbox.read(&format!("runs/{run}/status")).trim(), "");
        assert_eq!(
            sandbox.read(&format!("runs/{run}/nested-sykli")).trim(),
            "",
            "a nested .sykli survived the reset"
        );
    }
}

/// Review part 2, finding 6: Judgements pushed at once from three clones of
/// one origin, 60 in 20 rounds: each push that origin rejected because
/// another moved it first is tried again, so every one exits 0, none is
/// said "not shared", and origin holds every line.
#[test]
fn judgements_pushed_at_once_from_three_clones_all_arrive() {
    let (sandbox, repo, bare) = Sandbox::stage5("push-race", Some("private"));
    let mut make = sandbox.command("sh", &repo);
    make.args([
        "-c",
        "git checkout -q -b feat/x && echo x > x && git add x && git commit -qm x \
         && git push -q --no-verify origin feat/x && git checkout -q main",
    ]);
    for (key, value) in ANN {
        make.env(key, value);
    }
    assert!(make.output().unwrap().status.success());
    let head = sandbox.git(&repo, &["rev-parse", "feat/x"]);
    let clones: Vec<PathBuf> = std::iter::once(repo.clone())
        .chain(["widget-b", "widget-c"].into_iter().map(|name| {
            let other = sandbox.dir.join("projects").join(name);
            sandbox.git(
                &sandbox.dir,
                &[
                    "clone",
                    "-q",
                    &bare.to_string_lossy(),
                    &other.to_string_lossy(),
                ],
            );
            sandbox.git(&other, &["fetch", "-q", "origin", "feat/x:feat/x"]);
            other
        }))
        .collect();
    let mut failures = Vec::new();
    for _ in 0..20 {
        let children: Vec<_> = clones
            .iter()
            .map(|clone| {
                let mut judge = sandbox.command("ohjaa", clone);
                judge
                    .args(["judge", "main..feat/x", "--verdict", "approve", "--push"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                for (key, value) in RAE {
                    judge.env(key, value);
                }
                judge.spawn().unwrap()
            })
            .collect();
        for child in children {
            let out = child.wait_with_output().unwrap();
            if out.status.code() != Some(0) {
                failures.push(format!("{:?}: {}", out.status.code(), text(&out).1));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed: {failures:#?}",
        failures.len()
    );
    let mut show = sandbox.command("git", &bare);
    show.args(["notes", "--ref", JUDGEMENTS_REF, "show", &head]);
    let on_origin: std::collections::BTreeSet<String> = text(&show.output().unwrap())
        .0
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|note| note["id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(on_origin.len(), 60, "{on_origin:?}");
}

/// Review part 2, finding 7: names origin already publishes are never
/// republished. With origin's notes naming people (pushed while the entry
/// was private, and a line another clone appended with plain git, as a
/// person in +0200), a public entry's push is refused loudly, origin is not
/// moved, and the refusal prints what a person would run; once they ran it,
/// origin names no one, anywhere in its history.
#[test]
fn names_origin_already_publishes_are_never_republished() {
    let (sandbox, repo, bare) = Sandbox::stage5("origin-names", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/p1");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &[
            "main..feat/p1",
            "--verdict",
            "request-changes",
            "--finding",
            "PRIVATE: customer acme-bank data in fixtures/x.sql",
            "--push",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "feat/p1"]);
    let other = sandbox.dir.join("elsewhere").join("o");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    sandbox.git(
        &other,
        &[
            "fetch",
            "-q",
            "origin",
            &format!("{JUDGEMENTS_REF}:{JUDGEMENTS_REF}"),
        ],
    );
    let mut append = sandbox.command("git", &other);
    append
        .args([
            "notes",
            "--ref",
            JUDGEMENTS_REF,
            "append",
            "-m",
            "{\"schema\":\"ohjaa-judgement-note.v1\",\"id\":\"jd-7-7\",\"redacted\":false,\
             \"reviewer\":{\"kind\":\"person\",\"name\":\"Rae Reviewer\"},\"verdict\":\"approve\"}",
            &head,
        ])
        .env("GIT_COMMITTER_NAME", "Rae Reviewer")
        .env("GIT_COMMITTER_EMAIL", "rae@example.invalid")
        .env("GIT_COMMITTER_DATE", "1791900000 +0200");
    assert!(append.output().unwrap().status.success());
    sandbox.git(&other, &["push", "-q", "origin", JUDGEMENTS_REF]);

    sandbox.entry_says(
        "acme",
        "widget",
        Some("visibility = \"public\"\nshare_judgements = true".to_owned()),
    );
    let origin_tip = sandbox.git(&bare, &["rev-parse", JUDGEMENTS_REF]);
    let (_, _) = sandbox.authored(&repo, "feat/p2");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/p2", "--verdict", "approve", "--push"],
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains(
            "origin already publishes 2 note line(s) that are not redacted and 1 \
                         notes commit(s) by people"
        ),
        "{stderr}"
    );
    let commands = [
        format!("git fetch origin +{JUDGEMENTS_REF}:refs/notes/ohjaa/fetched/judgements"),
        "ohjaa judgements --rebuild-public".to_owned(),
        format!("git push --force origin {JUDGEMENTS_REF}:{JUDGEMENTS_REF}"),
    ];
    for command in &commands {
        assert!(stderr.contains(command.as_str()), "{command}: {stderr}");
    }
    assert_eq!(
        sandbox.git(&bare, &["rev-parse", JUDGEMENTS_REF]),
        origin_tip,
        "origin not moved"
    );
    assert!(
        sandbox
            .note_lines(&repo, &head)
            .iter()
            .all(|line| !line.contains("jd-7-7")),
        "origin's named line was not merged into this clone's notes"
    );

    // A person runs what it printed.
    sandbox.git(
        &repo,
        &[
            "fetch",
            "-q",
            "origin",
            &format!("+{JUDGEMENTS_REF}:refs/notes/ohjaa/fetched/judgements"),
        ],
    );
    let out = sandbox.ohjaa_in(&repo, &["judgements", "--rebuild-public"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    sandbox.git(
        &repo,
        &[
            "push",
            "-q",
            "--force",
            "origin",
            &format!("{JUDGEMENTS_REF}:{JUDGEMENTS_REF}"),
        ],
    );
    let (_, _) = sandbox.authored(&repo, "feat/p3");
    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/p3", "--verdict", "approve", "--push"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let mut log = sandbox.command("git", &bare);
    log.args(["log", "-p", "--format=%an %ae %cn %ce %ad", JUDGEMENTS_REF]);
    let published = text(&log.output().unwrap()).0;
    for word in ["acme-bank", "Rae", "rae@", "+0200", "jd-7-7"] {
        assert!(
            !published.contains(word),
            "{word:?} published:\n{published}"
        );
    }
}

/// Review part 2, finding 9: malformed lines on origin's note are shown as
/// invalid, never dropped from view, and a push keeps every line byte for
/// byte: one that is not UTF-8, and one ending in `\r`.
#[test]
fn malformed_note_lines_are_shown_and_kept_byte_for_byte() {
    let (sandbox, repo, bare) = Sandbox::stage5("malformed", Some("private"));
    let (_, head) = sandbox.authored(&repo, "feat/bad");
    sandbox.git(&repo, &["push", "-q", "--no-verify", "origin", "feat/bad"]);
    let other = sandbox.dir.join("elsewhere").join("bad");
    sandbox.git(
        &sandbox.dir,
        &[
            "clone",
            "-q",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"this is not json\n");
    bytes.extend_from_slice(
        b"{\"schema\":\"ohjaa-judgement-note.v1\",\"id\":\"jd-1-1\",\"verdict\":\"appr\n",
    );
    bytes.extend_from_slice(
        b"{\"schema\":\"ohjaa-judgement-note.v1\",\"id\":\"jd-2-2\",\"verdict\":\"approve\",\"x\":\"\xff\xfe\"}\n",
    );
    bytes.extend_from_slice(
        b"{\"schema\":\"ohjaa-judgement-note.v1\",\"id\":\"jd-3-3\",\"verdict\":\"reject\"}\r\n",
    );
    let file = sandbox.dir.join("bad-note");
    fs::write(&file, &bytes).unwrap();
    sandbox.git(&other, &["fetch", "-q", "origin", "feat/bad:feat/bad"]);
    // The bytes as they are (`notes add -F` would clean them up).
    let blob = sandbox.git(&other, &["hash-object", "-w", &file.to_string_lossy()]);
    sandbox.git(
        &other,
        &[
            "notes",
            "--ref",
            JUDGEMENTS_REF,
            "add",
            "-f",
            "-C",
            &blob,
            &head,
        ],
    );
    sandbox.git(&other, &["push", "-q", "origin", JUDGEMENTS_REF]);

    let json = sandbox.judgements(&repo, &head, &["--fetch"]);
    let invalid = json["invalid"].as_array().cloned().unwrap_or_default();
    let why: Vec<&str> = invalid
        .iter()
        .filter_map(|bad| bad["why"].as_str())
        .collect();
    assert_eq!(why.len(), 3, "{json:#}");
    for expected in ["not JSON", "JSON cut short", "not UTF-8"] {
        assert!(why.contains(&expected), "{expected}: {json:#}");
    }
    assert!(
        json["judgements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == "jd-3-3" && j["trust"] == "foreign"),
        "{json:#}"
    );
    let shown = text(&sandbox.ohjaa_in(&repo, &["judgements", &head])).0;
    assert_eq!(shown.matches("INVALID note line").count(), 3, "{shown}");

    let out = sandbox.judge_as(
        &repo,
        &RAE,
        &["main..feat/bad", "--verdict", "approve", "--push"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let after = sandbox
        .git_out(&bare, &["notes", "--ref", JUDGEMENTS_REF, "show", &head])
        .stdout;
    let has = |needle: &[u8]| after.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"\"x\":\"\xff\xfe\"}"), "not UTF-8: kept byte for byte");
    assert!(!has("\u{fffd}".as_bytes()), "never re-encoded");
    assert!(has(b"\"reject\"}\r\n"), "its \\r kept");
    assert!(has(b"this is not json\n") && has(b"\"verdict\":\"appr\n"));
}
