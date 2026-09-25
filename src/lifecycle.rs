//! Running a session from entering to leaving.
//!
//! `ohjaa run` stays the parent of `toimija run` so that it is still there when
//! the session ends: leaving must always write a record, crash or not. While
//! it waits it must not die of a signal meant for the session:
//!
//! - Ctrl-C (SIGINT) and a closed terminal (SIGHUP) go to the whole foreground
//!   process group, so toimija and the agent already receive them. Ohjaa only
//!   survives them; forwarding would deliver them twice.
//! - SIGTERM aimed at ohjaa alone does not reach the session, so ohjaa passes
//!   it on to toimija, which passes it on to the agent.
//!
//! Handlers, not `SIG_IGN`: an ignored disposition is inherited across exec,
//! and the agent must still stop on Ctrl-C.

use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::process;
use crate::sessions::{Evidence, Exit, Finished, Phase};

static TERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(signal: libc::c_int) {
    // Only an atomic store: the one thing that is safe inside a handler.
    if signal == libc::SIGTERM {
        TERM_RECEIVED.store(true, Ordering::SeqCst);
    }
}

/// Install the waiting parent's handlers. Call only after the child exists.
pub fn survive_signals() {
    let handler = on_signal as extern "C" fn(libc::c_int);
    // SAFETY: `on_signal` only performs an atomic store, which is
    // async-signal-safe; the handler stays valid for the life of the process.
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
}

/// Wait for the session to end, forwarding SIGTERM to it once.
pub fn wait(child: &mut Child) -> std::io::Result<ExitStatus> {
    let mut forwarded = false;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if !forwarded && TERM_RECEIVED.load(Ordering::SeqCst) {
            forwarded = true;
            // SAFETY: plain kill(2) on the pid of a child we have not reaped.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn exit_of(status: ExitStatus) -> Exit {
    if let Some(code) = status.code() {
        return Exit::Code { code };
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return Exit::Signal { signal };
        }
    }
    Exit::Unknown
}

/// The exit code `ohjaa run` returns for the session's end.
pub fn exit_code(exit: Exit) -> i32 {
    match exit {
        Exit::Code { code } => code,
        Exit::Signal { signal } => 128 + signal,
        Exit::Unknown => 1,
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = process::run(
        Command::new("git").arg("-C").arg(repo).args(args),
        None,
        process::LOCAL,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| output.stdout_text().trim().to_owned())
}

pub fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "--verify", "-q", "HEAD"])
}

pub fn branch(repo: &Path) -> Option<String> {
    git(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
}

/// The sykli receipts in a repository, keyed by the HEAD tree they ran on.
/// Reads `sykli-receipt.v1`: `subject.head_tree_oid`, `subject.dirty`,
/// `outcome`, `started_at_ms`. The newest receipt for a tree wins.
pub fn receipts_by_tree(repo: &Path) -> std::collections::HashMap<String, (i64, Evidence)> {
    let mut found = std::collections::HashMap::new();
    let Ok(entries) = std::fs::read_dir(repo.join(".sykli").join("receipts")) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(receipt) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            continue;
        };
        if receipt["schema"] != "sykli-receipt.v1" {
            continue;
        }
        let (Some(tree), Some(outcome)) = (
            receipt["subject"]["head_tree_oid"].as_str(),
            receipt["outcome"].as_str(),
        ) else {
            continue;
        };
        let at = receipt["started_at_ms"].as_i64().unwrap_or(0);
        let evidence = Evidence {
            outcome: outcome.to_owned(),
            dirty: receipt["subject"]["dirty"].as_bool().unwrap_or(true),
        };
        match found.get(tree) {
            Some((seen, _)) if *seen >= at => {}
            _ => {
                found.insert(tree.to_owned(), (at, evidence));
            }
        }
    }
    found
}

/// State 3, observed: every commit made since the session started, each with
/// whatever sykli saw on its tree.
pub fn phases(repo: &Path, head_at_start: Option<&str>) -> Vec<Phase> {
    // No starting point means no way to tell this session's commits apart.
    let Some(start) = head_at_start else {
        return Vec::new();
    };
    let range = format!("{start}..HEAD");
    let Some(log) = git(
        repo,
        &[
            "log",
            "--reverse",
            "--max-count=50",
            "--format=%H%x1f%T%x1f%s",
            &range,
        ],
    ) else {
        return Vec::new();
    };
    let receipts = receipts_by_tree(repo);
    log.lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            let (commit, tree, subject) = (fields.next()?, fields.next()?, fields.next()?);
            Some(Phase {
                commit: commit.to_owned(),
                subject: subject.to_owned(),
                evidence: receipts.get(tree).map(|(_, evidence)| evidence.clone()),
            })
        })
        .collect()
}

/// State 4, observed: the branch's pull request, if `gh` can say.
pub fn finished(repo: &Path) -> Finished {
    let output = process::run(
        Command::new("gh")
            .args(["pr", "view", "--json", "url,state,headRefOid"])
            .current_dir(repo),
        None,
        Duration::from_secs(20),
    );
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            return Finished::Unknown {
                why: format!("{error:#}"),
            }
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return if stderr.contains("no pull requests found") {
            Finished::NoPullRequest
        } else if stderr.contains("gh auth login") || stderr.contains("GH_TOKEN") {
            Finished::Unknown {
                why: "gh is not signed in".to_owned(),
            }
        } else {
            Finished::Unknown {
                why: output.failure_detail(),
            }
        };
    }
    let Ok(pr) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Finished::Unknown {
            why: "unreadable gh output".to_owned(),
        };
    };
    let head_evidence = pr["headRefOid"]
        .as_str()
        .and_then(|head| git(repo, &["rev-parse", &format!("{head}^{{tree}}")]))
        .and_then(|tree| receipts_by_tree(repo).remove(&tree).map(|(_, e)| e));
    Finished::PullRequest {
        url: pr["url"].as_str().unwrap_or("").to_owned(),
        state: pr["state"].as_str().unwrap_or("unknown").to_owned(),
        head_evidence,
    }
}

/// The toimija session the child ran: toimija names it `<secs>-<pid>` after
/// its own process, which is ohjaa's child. Asked of toimija, never read from
/// its files.
pub fn toimija_session(repo: &Path, child_pid: u32) -> Option<String> {
    let output = process::run(
        Command::new("toimija")
            .args(["sessions", "--json"])
            .current_dir(repo),
        None,
        process::LOCAL,
    )
    .ok()?;
    let payload: Value = serde_json::from_slice(&output.stdout).ok()?;
    let suffix = format!("-{child_pid}");
    payload["sessions"]
        .as_array()?
        .iter()
        .filter_map(|session| session["id"].as_str())
        .find(|id| id.ends_with(&suffix))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ohjaa-life-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "T"],
            vec!["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(&dir)
                .status()
                .unwrap()
                .success());
        }
        dir
    }

    fn commit(dir: &Path, file: &str, message: &str) -> String {
        std::fs::write(dir.join(file), message).unwrap();
        assert!(Command::new("git")
            .args(["add", file])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        git(dir, &["rev-parse", "HEAD^{tree}"]).unwrap()
    }

    fn receipt(dir: &Path, name: &str, tree: &str, outcome: &str, at: i64, dirty: bool) {
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join(format!("{name}.json")),
            format!(
                r#"{{"schema":"sykli-receipt.v1","contract_hash":"x","started_at_ms":{at},
                   "subject":{{"repository":"/r","tree_oid":"{tree}","inputs_digest":"d",
                   "head_tree_oid":"{tree}","dirty":{dirty}}},"tasks":[],"outcome":"{outcome}"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn phases_are_this_sessions_commits_with_what_sykli_saw() {
        let dir = repo("phases");
        let start = head(&dir).unwrap();
        let first = commit(&dir, "a.txt", "first phase");
        let second = commit(&dir, "b.txt", "second phase");
        receipt(&dir, "old", &first, "failed", 1, false);
        receipt(&dir, "new", &first, "passed", 2, false);
        receipt(&dir, "dirty", &second, "passed", 3, true);
        let found = phases(&dir, Some(&start));
        assert_eq!(
            found.len(),
            2,
            "the commit before the session is not a phase"
        );
        assert_eq!(found[0].subject, "first phase");
        assert_eq!(
            found[0].evidence.as_ref().unwrap().outcome,
            "passed",
            "the newest receipt wins"
        );
        assert!(found[1].evidence.as_ref().unwrap().dirty);

        let third = commit(&dir, "c.txt", "third phase");
        let _ = third;
        assert!(
            phases(&dir, Some(&start))[2].evidence.is_none(),
            "no run is said, not guessed"
        );
        assert!(
            phases(&dir, None).is_empty(),
            "without a start, no phase is claimed"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_receipt_of_another_schema_is_not_evidence() {
        let dir = repo("schema");
        let tree = commit(&dir, "a.txt", "x");
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join("r.json"),
            format!(r#"{{"schema":"other.v1","subject":{{"head_tree_oid":"{tree}"}},"outcome":"passed"}}"#),
        )
        .unwrap();
        assert!(receipts_by_tree(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn signals_map_to_the_shells_exit_codes() {
        assert_eq!(exit_code(Exit::Code { code: 7 }), 7);
        assert_eq!(exit_code(Exit::Signal { signal: 15 }), 143);
        let status = Command::new("sh")
            .args(["-c", "kill -TERM $$"])
            .status()
            .unwrap();
        assert_eq!(exit_of(status), Exit::Signal { signal: 15 });
    }
}
