//! `ohjaa hook <name> [args]`: the git hook dispatcher.
//!
//! A repository whose remote names an entry includes `core.hooksPath =
//! <home>/hooks` (see `gitconfig`), where each needed hook is a dispatcher into
//! this command: pre-push, and every hook an observed clone has of its own. No
//! other repository runs it. Two jobs:
//!
//! 1. **Observing (pre-push only).** The repository is found by the URL git
//!    pushes to. When the home has an entry for it and the push moves the
//!    entry's branch, sykli judges the push against the entry's gate, quietly.
//!    Nothing is refused for it: a push sykli does not accept, or cannot
//!    judge, goes on, and one line flags it as unproven.
//! 2. **Chaining (every hook).** A `core.hooksPath` switches off the clone's
//!    own `.git/hooks`, so the dispatcher runs the repository's own hook,
//!    `$(git rev-parse --git-common-dir)/hooks/<name>` when executable, with
//!    the same arguments and the same stdin, and exits with its status: the
//!    repository's own hooks still refuse what they refuse.
//!
//! Nothing is ever written into the repository.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

use crate::home::{self, RepoId};
use crate::process;

/// Every hook git runs, as git 2.54 lists them (`githooks(5)`).
pub const HOOK_NAMES: [&str; 28] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// Hooks that never get a dispatcher: a file of that name changes what git
/// does in every repository that shares the hooks folder, whatever the
/// dispatcher then chains to. `push-to-checkout` takes over the worktree
/// update of `receive.denyCurrentBranch=updateInstead`, `proc-receive` takes
/// over `receive.procReceiveRefs`, and `fsmonitor-watchman` is never run from
/// a hooks folder at all (`core.fsmonitor` names it).
pub const NOT_DISPATCHED: [&str; 3] = ["push-to-checkout", "proc-receive", "fsmonitor-watchman"];

/// What a dispatcher runs: the ohjaa it execs and, for pre-push, the sykli
/// the gate is handed to. Both by the absolute paths `apply` found on PATH,
/// so a git that runs with another PATH (a GUI client) finds them too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub ohjaa: PathBuf,
    pub sykli: Option<PathBuf>,
}

/// The dispatcher `ohjaa apply` writes as `<entry>/hooks/<name>`.
pub fn dispatcher(name: &str, dispatch: &Dispatch) -> String {
    let sykli = match (&dispatch.sykli, name) {
        (Some(sykli), "pre-push") => {
            format!("OHJAA_SYKLI={}; export OHJAA_SYKLI\n", quote_path(sykli))
        }
        _ => String::new(),
    };
    format!(
        "#!/bin/sh\n\
         # Written by `ohjaa apply`, checked by `ohjaa doctor`; edits are replaced.\n\
         {sykli}exec {} hook {name} \"$@\"\n",
        quote_path(&dispatch.ohjaa)
    )
}

pub fn quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn unquote(quoted: &str) -> Option<PathBuf> {
    let inner = quoted.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(PathBuf::from(inner.replace("'\\''", "'")))
}

/// What a dispatcher runs, when the file is one of `apply`'s dispatchers
/// (unedited) for `name`.
pub fn dispatcher_target(name: &str, text: &str) -> Option<Dispatch> {
    let exec = text.lines().find_map(|line| line.strip_prefix("exec "))?;
    let ohjaa = unquote(exec.strip_suffix(&format!(" hook {name} \"$@\""))?)?;
    let sykli = text.lines().find_map(|line| {
        unquote(
            line.strip_prefix("OHJAA_SYKLI=")?
                .strip_suffix("; export OHJAA_SYKLI")?,
        )
    });
    let dispatch = Dispatch { ohjaa, sykli };
    (dispatcher(name, &dispatch) == text).then_some(dispatch)
}

/// The first `name` on PATH, as the shell finds it: made absolute, never
/// resolved, so `~/.cargo/bin/ohjaa` stays that and a reinstall keeps every
/// dispatcher working.
pub fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| crate::gitconfig::executable(path))
        .map(|path| std::path::absolute(&path).unwrap_or(path))
}

/// Could not run: not a hook, or the repository's own hooks cannot be found.
const CANNOT: i32 = 2;

pub fn run(name: &str, args: &[OsString]) -> Result<i32> {
    if !HOOK_NAMES.contains(&name) {
        eprintln!("ohjaa hook: {name:?} is not a git hook");
        return Ok(CANNOT);
    }
    let here = match here() {
        Ok(here) => here,
        Err(error) => {
            // Skipping the repository's own hook would pass a check it may
            // hold, so this refuses instead.
            eprintln!("ohjaa hook {name}: cannot find the repository's own hooks: {error:#}");
            return Ok(CANNOT);
        }
    };
    let mut observed = Observed::default();
    if name == "pre-push" {
        observed = observe(args, &here)?;
    }
    let code = chain(name, args, observed.input.as_deref(), &here.common)?;
    // Said last, where the person sees it, and only for a push that goes.
    if code == 0 {
        if let Some(flag) = observed.flag {
            eprintln!("{flag}");
        }
    }
    Ok(code)
}

/// Where git runs a hook, worked out once: the work tree's root (git's
/// current directory for a hook in a non-bare repository) and the common
/// git directory, from the layout. One git process only when the layout
/// does not say.
struct Here {
    top: Option<PathBuf>,
    common: PathBuf,
}

fn here() -> Result<Here> {
    let env = |var: &str| std::env::var_os(var).filter(|value| !value.is_empty());
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    if let Some(common) = common_dir_in(&cwd, env("GIT_DIR"), env("GIT_COMMON_DIR")) {
        let top = env("GIT_WORK_TREE")
            .map(|top| cwd.join(top))
            .or_else(|| cwd.join(".git").exists().then(|| cwd.clone()));
        return Ok(Here { top, common });
    }
    let ask = |args: &[&str]| -> Result<Vec<String>> {
        Ok(home::git_value(args, None)?
            .context("git names no repository here")?
            .lines()
            .map(str::to_owned)
            .collect())
    };
    let lines = ask(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
        "--show-toplevel",
    ])
    // A bare repository has no top level to show.
    .or_else(|_| ask(&["rev-parse", "--path-format=absolute", "--git-common-dir"]))?;
    let mut lines = lines.into_iter();
    let common = PathBuf::from(lines.next().context("git named no common directory")?);
    Ok(Here {
        top: lines.next().map(PathBuf::from),
        common,
    })
}

/// What observing a push found: git's stdin when it was read (the chained
/// hook gets it), and the flag to say when the push goes.
#[derive(Default)]
struct Observed {
    input: Option<Vec<u8>>,
    flag: Option<String>,
}

/// One line of git's pre-push stdin.
struct Update {
    local_sha: String,
    remote_ref: String,
}

fn updates(input: &str) -> std::result::Result<Vec<Update>, String> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(
            |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [_, local_sha, remote_ref, _] => Ok(Update {
                    local_sha: local_sha.to_owned(),
                    remote_ref: remote_ref.to_owned(),
                }),
                _ => Err(format!("unexpected pre-push line {line:?}")),
            },
        )
        .collect()
}

/// The sykli a push is handed to: the one `apply` recorded in the
/// dispatcher (`OHJAA_SYKLI`), else the one on PATH.
fn sykli_program() -> PathBuf {
    std::env::var_os("OHJAA_SYKLI")
        .filter(|path| !path.is_empty())
        .map_or_else(|| PathBuf::from("sykli"), PathBuf::from)
}

/// `sykli hook pre-push --help`, or why sykli cannot be run.
fn sykli_help(sykli: &Path) -> std::result::Result<String, String> {
    let output = Command::new(sykli)
        .args(["hook", "pre-push", "--help"])
        .stdin(Stdio::null())
        .output();
    match output {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(if sykli == Path::new("sykli") {
                "sykli is not on PATH".to_owned()
            } else {
                format!(
                    "the sykli `ohjaa apply` recorded ({}) is gone",
                    sykli.display()
                )
            })
        }
        Err(error) => Err(format!("sykli cannot be run ({error})")),
        Ok(output) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
    }
}

/// Why this sykli cannot judge a push against a contract outside the
/// repository for exactly the entry's branch, if it cannot. Both flags are
/// required: without `--branch`, sykli would judge the branch
/// `refs/remotes/<remote>/HEAD` names, which need not be the entry's.
fn cannot_judge(help: &str) -> Option<String> {
    let lacking: Vec<&str> = ["--contract", "--branch"]
        .into_iter()
        .filter(|flag| !help.contains(flag))
        .collect();
    (!lacking.is_empty()).then(|| {
        format!(
            "this sykli cannot judge a branch against a contract kept outside the \
             repository (`sykli hook pre-push --help` names no {}); update sykli",
            lacking.join(" and no ")
        )
    })
}

/// sykli's flags that take the repository's facts instead of asking git
/// for them again, when this sykli has them.
pub fn facts_for_sykli(help: &str, top: Option<&Path>, common: &Path) -> Vec<OsString> {
    match top {
        Some(top) if help.contains("--repo-root") && help.contains("--git-common-dir") => vec![
            "--repo-root".into(),
            top.as_os_str().to_owned(),
            "--git-common-dir".into(),
            common.as_os_str().to_owned(),
        ],
        _ => Vec::new(),
    }
}

fn read_stdin() -> Result<Vec<u8>> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .context("cannot read what git pushes")?;
    Ok(input)
}

/// The flag for a push to `branch` that is not proven, `why` in sykli's
/// words (or why sykli could not be asked).
fn unproven(branch: &str, why: &str) -> String {
    format!(
        "ohjaa: UNPROVEN push to {branch} ({why}). It is pushed and flagged; prove it with: \
         ohjaa gate"
    )
}

/// sykli's reason, from what `sykli hook pre-push` said on stderr: its first
/// line, without the hook's name or the commit and ref the flag names again.
fn sykli_reason(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = line.strip_prefix("sykli hook pre-push: ").unwrap_or(line);
    // `refusing to push <sha> to <ref>: <reason>`
    let reason = line
        .strip_prefix("refusing to push ")
        .and_then(|rest| rest.split_once(": ").map(|(_, reason)| reason))
        .unwrap_or(line);
    Some(reason.to_owned())
}

/// Observe a push: never refuse it. The URL is git's `$2`, the entry is a
/// file, and the repository's facts come from [`here`], so no git process is
/// started here. A push that moves the entry's branch is handed to `sykli
/// hook pre-push` against the entry's gate, quietly; when sykli does not
/// accept it, or cannot be asked, the push goes on with a flag.
fn observe(args: &[OsString], here: &Here) -> Result<Observed> {
    let url = args.get(1).map(|url| url.to_string_lossy().into_owned());
    let Some(id) = url.as_deref().and_then(RepoId::parse) else {
        return Ok(Observed::default());
    };
    let home = home::ohjaa_home()?;
    let slug = id.slug();
    let entry = match home::find_entry(&home, &id) {
        Ok(Some(entry)) => entry,
        Ok(None) => return Ok(Observed::default()),
        Err(error) => {
            return Ok(Observed {
                input: Some(read_stdin()?),
                flag: Some(format!(
                    "ohjaa: UNOBSERVED push to {slug}: its entry cannot be read ({error:#}). It \
                     is pushed; fix the entry (ohjaa doctor)"
                )),
            })
        }
    };
    let input = read_stdin()?;
    let observed = |flag: Option<String>, input: Vec<u8>| Observed {
        input: Some(input),
        flag,
    };
    let text = String::from_utf8_lossy(&input).into_owned();
    let pushed = match updates(&text) {
        Ok(updates) => updates,
        Err(error) => {
            let flag = unproven(
                &entry.branch,
                &format!("what is pushed cannot be read: {error}"),
            );
            return Ok(observed(Some(flag), input));
        }
    };
    let observed_ref = format!("refs/heads/{}", entry.branch);
    let moves = pushed.iter().any(|update| {
        update.remote_ref == observed_ref && !update.local_sha.bytes().all(|b| b == b'0')
    });
    if !moves {
        return Ok(observed(None, input));
    }
    if !entry.has_contract() {
        let why = format!(
            "its entry has no gate: {} is missing (ohjaa adopt --force)",
            entry.contract().display()
        );
        return Ok(observed(Some(unproven(&entry.branch, &why)), input));
    }
    let sykli = sykli_program();
    let help = match sykli_help(&sykli) {
        Ok(help) => help,
        Err(why) => return Ok(observed(Some(unproven(&entry.branch, &why)), input)),
    };
    if let Some(why) = cannot_judge(&help) {
        return Ok(observed(Some(unproven(&entry.branch, &why)), input));
    }
    let mut command = Command::new(&sykli);
    command
        .args(["hook", "pre-push", "--contract"])
        .arg(entry.contract())
        .args(["--branch", &entry.branch])
        .args(facts_for_sykli(&help, here.top.as_deref(), &here.common))
        .args(args.iter().take(2));
    let flag = match process::run(&mut command, Some(&input), process::LOCAL) {
        Err(error) => Some(unproven(
            &entry.branch,
            &format!("sykli hook pre-push could not run: {error:#}"),
        )),
        Ok(output) if output.status.success() => None,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = sykli_reason(&stderr)
                .unwrap_or_else(|| format!("sykli hook pre-push exited {}", output.status));
            Some(unproven(&entry.branch, &why))
        }
    };
    Ok(observed(flag, input))
}

/// The common git directory as git's on-disk layout names it
/// (gitrepository-layout(5)), for a hook git started in `cwd`.
///
/// Git runs a hook from the root of the working tree (from `$GIT_DIR` in a
/// bare repository). It exports `GIT_DIR` for a linked worktree and while
/// `git init` or `git clone` makes a repository (when `rev-parse` cannot see
/// it yet); otherwise `cwd/.git` is the directory, or a `gitdir: <path>` file.
/// A git directory with a `commondir` file shares that one's hooks. `None`
/// when the layout does not say, and git must be asked.
pub fn common_dir_in(
    cwd: &Path,
    git_dir: Option<OsString>,
    common_dir: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(common) = common_dir {
        return Some(cwd.join(common));
    }
    let git_dir = match git_dir {
        Some(dir) => cwd.join(dir),
        None => {
            let dot_git = cwd.join(".git");
            let meta = std::fs::metadata(&dot_git).ok()?;
            if meta.is_dir() {
                dot_git
            } else {
                let text = std::fs::read_to_string(&dot_git).ok()?;
                cwd.join(text.strip_prefix("gitdir:")?.trim())
            }
        }
    };
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Some(git_dir.join(common.trim())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(git_dir),
        Err(_) => None,
    }
}

/// Run the repository's own hook, `<common dir>/hooks/<name>` (where git
/// would look without a `core.hooksPath`; every worktree shares it), if it
/// has one git would have run.
fn chain(name: &str, args: &[OsString], input: Option<&[u8]>, common: &Path) -> Result<i32> {
    let own = common.join("hooks").join(name);
    let meta = match std::fs::metadata(&own) {
        Ok(meta) if meta.is_file() => meta,
        _ => return Ok(0),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            // Git's own words for a hook it skips (advice.ignoredHook).
            eprintln!(
                "hint: The '{}' hook was ignored because it's not set as executable.",
                own.display()
            );
            return Ok(0);
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    // A repository hook that is (a copy of, or a link to) one of ohjaa's
    // dispatchers would run itself forever.
    if std::fs::read_to_string(&own).is_ok_and(|text| dispatcher_target(name, &text).is_some()) {
        return Ok(0);
    }
    let status = match run_with_input(Command::new(&own).args(args), input) {
        Err(error) if is_exec_format_error(&error) => {
            // Git runs a hook without a `#!` line through the shell; so does this.
            run_with_input(
                Command::new("/bin/sh")
                    .arg(&own)
                    .args(args.iter().map(OsString::as_os_str)),
                input,
            )
        }
        other => other,
    }
    .with_context(|| format!("cannot run {}", own.display()))?;
    Ok(exit_code(status))
}

fn is_exec_format_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ENOEXEC)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Run `command` with the terminal's stdout and stderr. With `input`, that is
/// its stdin (a hook that stops reading early is not an error); without, it
/// inherits ours, unread.
fn run_with_input(command: &mut Command, input: Option<&[u8]>) -> std::io::Result<ExitStatus> {
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::inherit()
    });
    let mut child = command.spawn()?;
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        let bytes = bytes.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
        let status = child.wait();
        let _ = writer.join();
        return status;
    }
    child.wait()
}

/// The status as a shell reports it: the exit code, or 128 + the signal.
fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    CANNOT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dispatcher_execs_ohjaa_by_its_path_and_pre_push_names_sykli() {
        let dispatch = Dispatch {
            ohjaa: PathBuf::from("/Users/x/.cargo/bin/ohjaa"),
            sykli: Some(PathBuf::from("/Users/x/.local/bin/sykli")),
        };
        for name in HOOK_NAMES {
            let text = dispatcher(name, &dispatch);
            assert!(text.starts_with("#!/bin/sh\n"), "{name}");
            assert!(
                text.ends_with(&format!(
                    "exec '/Users/x/.cargo/bin/ohjaa' hook {name} \"$@\"\n"
                )),
                "{name}"
            );
            assert_eq!(
                text.contains("OHJAA_SYKLI='/Users/x/.local/bin/sykli'; export OHJAA_SYKLI\n"),
                name == "pre-push",
                "{name}"
            );
            let read = dispatcher_target(name, &text).unwrap();
            assert_eq!(read.ohjaa, dispatch.ohjaa);
            assert_eq!(read.sykli.is_some(), name == "pre-push", "{name}");
        }
        let odd = Dispatch {
            ohjaa: PathBuf::from("/tmp/it's here/ohjaa"),
            sykli: None,
        };
        let text = dispatcher("pre-push", &odd);
        assert_eq!(dispatcher_target("pre-push", &text), Some(odd));
        assert_eq!(
            dispatcher_target("pre-commit", &text),
            None,
            "another hook's"
        );
        let edited = text.replace("exec", "echo hi\nexec");
        assert_eq!(dispatcher_target("pre-push", &edited), None, "edited");
        let mut names = HOOK_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), HOOK_NAMES.len(), "no hook twice");
    }

    #[test]
    fn sykli_gets_the_facts_only_when_it_takes_them() {
        let common = Path::new("/r/.git");
        let both = "  --repo-root <DIR>\n  --git-common-dir <DIR>\n";
        assert_eq!(
            facts_for_sykli(both, Some(Path::new("/r")), common),
            ["--repo-root", "/r", "--git-common-dir", "/r/.git"].map(OsString::from)
        );
        assert!(facts_for_sykli("  --repo-root <DIR>\n", Some(Path::new("/r")), common).is_empty());
        assert!(
            facts_for_sykli(both, None, common).is_empty(),
            "a bare repository"
        );
        assert_eq!(cannot_judge("--contract --branch"), None);
        assert!(cannot_judge("--contract")
            .unwrap()
            .contains("names no --branch"));
    }

    #[test]
    fn the_common_dir_is_read_from_the_layout_git_wrote() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        let linked = root.join("linked");
        std::fs::create_dir_all(main.join(".git/worktrees/linked")).unwrap();
        std::fs::create_dir_all(&linked).unwrap();
        // A linked worktree, written with relative paths (`--relative-paths`).
        std::fs::write(
            linked.join(".git"),
            "gitdir: ../main/.git/worktrees/linked\n",
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/linked/commondir"), "../..\n").unwrap();
        let canonical = |path: PathBuf| std::fs::canonicalize(path).unwrap();
        let common = canonical(main.join(".git"));

        let found = |cwd: &Path, git_dir: Option<&Path>, common_dir: Option<&Path>| {
            common_dir_in(
                cwd,
                git_dir.map(|dir| dir.as_os_str().to_owned()),
                common_dir.map(|dir| dir.as_os_str().to_owned()),
            )
            .map(canonical)
        };
        assert_eq!(
            found(&main, None, None),
            Some(common.clone()),
            "the main checkout"
        );
        assert_eq!(
            found(&linked, None, None),
            Some(common.clone()),
            "a linked worktree"
        );
        let exported = main.join(".git/worktrees/linked");
        assert_eq!(
            found(&linked, Some(&exported), None),
            Some(common.clone()),
            "GIT_DIR"
        );
        assert_eq!(
            found(&root, Some(Path::new("main/.git")), None),
            Some(common.clone())
        );
        assert_eq!(
            found(&root, None, Some(&main.join(".git"))),
            Some(common),
            "GIT_COMMON_DIR"
        );
        assert_eq!(found(&root, None, None), None, "no layout here: ask git");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_flag_carries_sykli_s_first_reason() {
        let refused = "sykli hook pre-push: refusing to push 191f7b020a58 to refs/heads/main: \
                       no receipt in this worktree is a run of its tree\n\
                       sykli hook pre-push: to produce a receipt, check out 191f7b020a58\n";
        assert_eq!(
            sykli_reason(refused).as_deref(),
            Some("no receipt in this worktree is a run of its tree")
        );
        assert_eq!(
            sykli_reason("\nsykli hook pre-push: cannot evaluate: bad stdin\n").as_deref(),
            Some("cannot evaluate: bad stdin")
        );
        assert_eq!(sykli_reason(" \n"), None);
        assert_eq!(
            unproven("main", "why"),
            "ohjaa: UNPROVEN push to main (why). It is pushed and flagged; prove it with: \
             ohjaa gate"
        );
    }

    #[test]
    fn pre_push_lines_are_read_and_a_garbled_one_is_an_error() {
        let pushed = updates(
            "refs/heads/main 1111 refs/heads/main 0000\n\nrefs/heads/x 2222 refs/heads/x 3333\n",
        )
        .unwrap();
        assert_eq!(pushed.len(), 2);
        assert_eq!(pushed[0].remote_ref, "refs/heads/main");
        assert_eq!(pushed[1].local_sha, "2222");
        assert!(updates("refs/heads/main 1111\n").is_err());
    }
}
