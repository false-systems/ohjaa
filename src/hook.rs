//! `ohjaa hook <name> [args]`: the global git hook dispatcher.
//!
//! `ohjaa apply` points the global `core.hooksPath` at the home's `hooks/`,
//! where every git hook is a two-line dispatcher into this command. Two jobs:
//!
//! 1. **The gate (pre-push only).** The repository is found by the URL git
//!    pushes to. When the home has an entry for it with a contract, sykli
//!    judges the push against that contract. A refusal stops the push.
//! 2. **Chaining (every hook).** A global `core.hooksPath` switches off every
//!    clone's own `.git/hooks`, so the dispatcher runs the repository's own
//!    hook, `$(git rev-parse --git-common-dir)/hooks/<name>` when executable,
//!    with the same arguments and the same stdin, and exits with its status.
//!
//! Nothing is ever written into the repository.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result};

use crate::home::{self, RepoId};

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

/// The dispatcher `ohjaa apply` writes as `hooks/<name>`.
pub fn dispatcher(name: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Written by `ohjaa apply`, checked by `ohjaa doctor`; edits are replaced.\n\
         exec ohjaa hook {name} \"$@\"\n"
    )
}

/// Refused, as sykli's hooks say it.
const REFUSED: i32 = 1;
/// Could not evaluate.
const CANNOT: i32 = 2;

pub fn run(name: &str, args: &[OsString]) -> Result<i32> {
    if !HOOK_NAMES.contains(&name) {
        eprintln!("ohjaa hook: {name:?} is not a git hook");
        return Ok(CANNOT);
    }
    let mut input = None;
    if name == "pre-push" {
        match gate(args)? {
            Gate::Refused(code) => return Ok(code),
            Gate::Passed(read) => input = read,
        }
    }
    chain(name, args, input.as_deref())
}

enum Gate {
    Refused(i32),
    /// Passed; carries git's stdin when the gate read it, for the chained hook.
    Passed(Option<Vec<u8>>),
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

/// Whether the sykli on PATH can judge a push against a contract outside the
/// repository; the reason when it cannot.
fn sykli_takes_a_contract() -> std::result::Result<(), String> {
    let output = Command::new("sykli")
        .args(["hook", "pre-push", "--help"])
        .stdin(Stdio::null())
        .output();
    match output {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("sykli is not on PATH".to_owned())
        }
        Err(error) => Err(format!("sykli cannot be run ({error})")),
        Ok(output) if String::from_utf8_lossy(&output.stdout).contains("--contract") => Ok(()),
        Ok(_) => Err(
            "the sykli on PATH cannot check a contract kept outside the repository \
             (`sykli hook pre-push --help` names no --contract); update it with \
             `ohjaa apply --tools`"
                .to_owned(),
        ),
    }
}

fn read_stdin() -> Result<Vec<u8>> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .context("cannot read what git pushes")?;
    Ok(input)
}

fn gate(args: &[OsString]) -> Result<Gate> {
    const HOOK: &str = "ohjaa hook pre-push";
    let remote = args.first().map(|arg| arg.to_string_lossy().into_owned());
    let url = match args.get(1) {
        Some(url) => Some(url.to_string_lossy().into_owned()),
        None => remote.as_deref().and_then(|remote| {
            home::git_value(&["remote", "get-url", remote], None)
                .ok()
                .flatten()
        }),
    };
    let Some(id) = url.as_deref().and_then(RepoId::parse) else {
        return Ok(Gate::Passed(None));
    };
    let home = home::ohjaa_home()?;
    let entry = match home::find_entry(&home, &id) {
        Ok(Some(entry)) => entry,
        Ok(None) => return Ok(Gate::Passed(None)),
        Err(error) => {
            eprintln!("{HOOK}: refusing the push: the entry for {id} cannot be read: {error:#}");
            eprintln!("{HOOK}: fix it, or skip every hook once with: git push --no-verify");
            return Ok(Gate::Refused(CANNOT));
        }
    };
    if !entry.has_contract() {
        eprintln!(
            "{HOOK}: {} has an entry but no gate ({} is missing); not gated",
            id.slug(),
            entry.contract().display()
        );
        return Ok(Gate::Passed(None));
    }
    let input = read_stdin()?;
    match sykli_takes_a_contract() {
        Ok(()) => {
            let mut command = Command::new("sykli");
            command
                .args(["hook", "pre-push", "--contract"])
                .arg(entry.contract())
                .args(args.iter().take(2));
            let status = run_with_input(&mut command, Some(&input))
                .context("cannot run sykli hook pre-push")?;
            if status.success() {
                return Ok(Gate::Passed(Some(input)));
            }
            eprintln!(
                "{HOOK}: the gate for {} ({}) refused this push",
                id.slug(),
                entry.contract().display()
            );
            Ok(Gate::Refused(exit_code(status).max(REFUSED)))
        }
        Err(why) => {
            let text = String::from_utf8_lossy(&input);
            let updates = match updates(&text) {
                Ok(updates) => updates,
                Err(error) => {
                    eprintln!("{HOOK}: cannot read what is pushed: {error}");
                    return Ok(Gate::Refused(CANNOT));
                }
            };
            let gated = format!("refs/heads/{}", entry.branch);
            let pushed = updates.iter().find(|update| {
                update.remote_ref == gated && !update.local_sha.bytes().all(|b| b == b'0')
            });
            if let Some(update) = pushed {
                let sha = &update.local_sha;
                eprintln!(
                    "{HOOK}: refusing to push {} to {gated} of {}: its gate is {}, but {why}",
                    &sha[..sha.len().min(12)],
                    id.slug(),
                    entry.contract().display()
                );
                return Ok(Gate::Refused(REFUSED));
            }
            Ok(Gate::Passed(Some(input)))
        }
    }
}

/// `$(git rev-parse --git-common-dir)/hooks/<name>`, where git would look
/// without a `core.hooksPath`. Every worktree shares it.
///
/// Read from the repository's layout when it says ([`common_dir_in`]), so the
/// hooks git runs on every commit and ref update (twelve for one commit) do
/// not each start a `git rev-parse`; asked of git otherwise.
fn own_hook(name: &str) -> Result<PathBuf> {
    let env = |var: &str| std::env::var_os(var).filter(|dir| !dir.is_empty());
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let common = match common_dir_in(&cwd, env("GIT_DIR"), env("GIT_COMMON_DIR")) {
        Some(common) => common,
        None => PathBuf::from(
            home::git_value(
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                None,
            )?
            .context("git names no repository here")?,
        ),
    };
    Ok(common.join("hooks").join(name))
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
fn common_dir_in(
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

/// Run the repository's own hook, if it has one git would have run.
fn chain(name: &str, args: &[OsString], input: Option<&[u8]>) -> Result<i32> {
    let own = match own_hook(name) {
        Ok(own) => own,
        Err(error) => {
            // Skipping the repository's own hook would pass a check it may
            // hold, so this refuses instead.
            eprintln!("ohjaa hook {name}: cannot find the repository's own hooks: {error:#}");
            return Ok(CANNOT);
        }
    };
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
    // A repository hook that is (a link to) this dispatcher would run itself
    // forever.
    if let Ok(home) = home::ohjaa_home() {
        if home::same_path(&own, &home::hooks_dir(&home).join(name)) {
            return Ok(0);
        }
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
    fn every_dispatcher_execs_ohjaa_with_its_own_name() {
        for name in HOOK_NAMES {
            let text = dispatcher(name);
            assert!(text.starts_with("#!/bin/sh\n"), "{name}");
            assert!(
                text.ends_with(&format!("exec ohjaa hook {name} \"$@\"\n")),
                "{name}"
            );
        }
        let mut names = HOOK_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), HOOK_NAMES.len(), "no hook twice");
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
