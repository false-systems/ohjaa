//! `ohjaa gate`: run the gate a push is judged by for the repository here,
//! and leave the receipt that proves HEAD: the push finds it, and so do the
//! marks (`.sykli/receipt.json`, and the copy `sykli run` keeps under
//! `.sykli/receipts/`).
//!
//! Inside an `ohjaa run` session (`OHJAA_SESSION`), the request is written
//! into the session's record before sykli is asked (a `ValidationRef`), and
//! what sykli gives back is filled in: its run id, the receipt it wrote, the
//! lock it read. References only: what became of the run is sykli's to say.
//! A sykli with durable runs (KATANA D3: `sykli run --detach` and `sykli
//! status --run`) is handed the run detached, so the run and its id outlive
//! this gate and the session; another one runs it in the foreground, as
//! always.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::gitconfig::{self, push_url, repo_at, Listing};
use crate::home::{self, absolute, RepoId};
use crate::hook::{facts_for_sykli, quote_path};
use crate::lifecycle;
use crate::process;
use crate::sessions::{self, ObservedBranch, ReceiptRef, ValidationKind, ValidationRef};

/// The gate of the repository at `top`.
struct Gate {
    contract: PathBuf,
    /// Whose it is, for people.
    whose: String,
    /// The branch the entry observes; `None` for a repository's own contract.
    branch: Option<String>,
}

/// The gate of the repository at `top`: the contract of the entry origin's
/// push URL names, else the repository's own `sykli.json`; with whose it is.
fn gate_of(top: &Path, listing: &Listing) -> Result<Option<Gate>> {
    let home = absolute(home::ohjaa_home()?);
    if let Some(id) = push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)
    {
        match home::find_entry(&home, &id)? {
            Some(entry) if entry.has_contract() => {
                return Ok(Some(Gate {
                    contract: entry.contract(),
                    whose: format!("the entry for {} (branch {})", id.slug(), entry.branch),
                    branch: Some(entry.branch.clone()),
                }))
            }
            Some(entry) => bail!(
                "{} has an entry but no gate: {} is missing (ohjaa adopt --force {})",
                id.slug(),
                entry.contract().display(),
                top.display()
            ),
            None => {}
        }
    }
    let own = top.join("sykli.json");
    Ok(own.is_file().then(|| Gate {
        contract: own,
        whose: "the repository's own sykli.json (no entry)".to_owned(),
        branch: None,
    }))
}

/// `sykli <command> --help`, said on stdout or stderr; empty when sykli
/// cannot be asked.
fn sykli_help(command: &str) -> String {
    Command::new("sykli")
        .args([command, "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .unwrap_or_default()
}

fn shown(words: &[std::ffi::OsString]) -> String {
    words
        .iter()
        .map(|word| {
            let word = word.to_string_lossy();
            if word.starts_with('-') {
                word.into_owned()
            } else {
                quote_path(Path::new(word.as_ref()))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The hash `contract`'s lock (`sykli.lock` beside it) pins for it, read as
/// sykli reads it: `sykli-lock.v1`'s one hash, or `sykli-lock.v2`'s entry
/// for the contract's file name. `None` without a lock that pins it.
fn pinned(contract: &Path) -> Option<String> {
    let lock: Value =
        serde_json::from_slice(&std::fs::read(contract.with_file_name("sykli.lock")).ok()?).ok()?;
    match lock["schema"].as_str()? {
        "sykli-lock.v1" => lock["contract_hash"].as_str().map(str::to_owned),
        "sykli-lock.v2" => {
            let name = contract.file_name()?.to_str()?;
            lock["contracts"][name].as_str().map(str::to_owned)
        }
        _ => None,
    }
}

/// A validation request recorded in the session this gate runs inside. The
/// log is opened for each write only, never held while sykli runs.
struct Request {
    log: PathBuf,
    session: String,
    id: String,
}

impl Request {
    /// Write the request into the session's record, before sykli is asked:
    /// HEAD and its tree (one git process), the contract and the hash its
    /// lock pins. Says why when it is not recorded; never stops the gate.
    fn record(top: &Path, common: &Path, gate: &Gate) -> Option<Request> {
        let not = |why: String| {
            println!("ohjaa gate: {why}: this request is not recorded");
            None
        };
        let Ok(session) = std::env::var(sessions::SESSION_ENV) else {
            return not(format!(
                "not inside an ohjaa session ({})",
                sessions::SESSION_ENV
            ));
        };
        if !sessions::is_valid_id(&session) {
            return not(format!(
                "{} is not a session id: `{session}`",
                sessions::SESSION_ENV
            ));
        }
        // HEAD, its tree, and the observed branch's tip as this clone last
        // saw it: one git process, which says `missing` for what is not.
        let mut asked = String::from("HEAD\nHEAD^{tree}\n");
        if let Some(branch) = &gate.branch {
            asked.push_str(&format!("refs/remotes/origin/{branch}\n"));
        }
        let answer = process::run(
            Command::new("git")
                .arg("-C")
                .arg(top)
                .args(["cat-file", "--batch-check=%(objectname)"]),
            Some(asked.as_bytes()),
            process::LOCAL,
        );
        let lines: Vec<String> = match answer {
            Ok(output) if output.status.success() => output
                .stdout_text()
                .lines()
                .map(|line| line.trim().to_owned())
                .collect(),
            Ok(output) => {
                return not(format!(
                    "git cannot say HEAD here ({})",
                    output.failure_detail()
                ))
            }
            Err(error) => return not(format!("git cannot say HEAD here ({error:#})")),
        };
        let oid = |n: usize| {
            lines
                .get(n)
                .filter(|line| !line.ends_with(" missing") && !line.is_empty())
                .cloned()
        };
        let (Some(commit), Some(tree)) = (oid(0), oid(1)) else {
            return not("HEAD has no commit yet".to_owned());
        };
        // Never integrated before sykli says what ran (`settle_kind`): a run
        // of uncommitted changes at the tip is no run of the tip.
        let observed_branch = gate.branch.as_ref().map(|name| ObservedBranch {
            name: name.clone(),
            tip: oid(2),
            updated_ms: crate::mark::updated(common, name).map(|at| at.ms),
        });
        let requested_ms = now_ms();
        let validation = ValidationRef {
            request: sessions::new_request_id(requested_ms),
            run: None,
            kind: ValidationKind::Candidate,
            repo: std::fs::canonicalize(top)
                .unwrap_or_else(|_| top.to_path_buf())
                .to_string_lossy()
                .into_owned(),
            commit,
            tree,
            contract: gate.contract.to_string_lossy().into_owned(),
            contract_digest: pinned(&gate.contract),
            lock_digest: None,
            requested_ms,
            receipt: None,
            common: Some(
                std::fs::canonicalize(common)
                    .unwrap_or_else(|_| common.to_path_buf())
                    .to_string_lossy()
                    .into_owned(),
            ),
            observed_branch,
            ran: None,
        };
        let log = match crate::knowledge::state_home() {
            Ok(state) => sessions::store_path(&state.join("ohjaa")),
            Err(error) => return not(format!("the session log cannot be found ({error:#})")),
        };
        let id = validation.request.clone();
        let written = sessions::Store::open(&log)
            .and_then(|store| store.update(&session, |s| s.validations.push(validation)));
        match written {
            Ok(Some(_)) => {
                println!(
                    "ohjaa gate: request {id} recorded in session {session} before asking sykli"
                );
                Some(Request { log, session, id })
            }
            Ok(None) => not(format!("session {session} is not in the session log")),
            Err(error) => not(error.to_string()),
        }
    }

    /// Fill in what sykli gave for the request: references only.
    fn fill(&self, what: &str, change: impl FnOnce(&mut ValidationRef)) {
        let written = sessions::Store::open(&self.log).and_then(|store| {
            store.update(&self.session, |s| {
                if let Some(validation) = s.validations.iter_mut().find(|v| v.request == self.id) {
                    change(validation);
                    validation.settle_kind();
                }
            })
        });
        if let Err(error) = written {
            println!(
                "ohjaa gate: {error}: {what} is not recorded in request {}",
                self.id
            );
        }
    }
}

/// The receipt sykli keeps of a run under `.sykli/receipts/rcpt_<sha256>.json`
/// that holds the same receipt as `printed` (what `sykli run --json` wrote),
/// written since `since_ms`: its path, and the sha256 sykli names it by.
fn kept_receipt(top: &Path, printed: &Value, since_ms: i64) -> Option<ReceiptRef> {
    let since = std::time::UNIX_EPOCH + Duration::from_millis(since_ms.max(0) as u64)
        - Duration::from_secs(2);
    std::fs::read_dir(top.join(".sykli").join("receipts"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| modified >= since)
        })
        .find(|path| {
            std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|kept| kept == *printed)
        })
        .map(|path| {
            let digest = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.strip_prefix("rcpt_"))
                .filter(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
                .map(str::to_owned);
            ReceiptRef {
                path: path.to_string_lossy().into_owned(),
                digest,
            }
        })
}

/// `ohjaa gate`: run the gate for the repository here, the way a push is
/// judged, and leave the receipt that proves HEAD. One git process (the
/// repository's config), and inside a session one more (HEAD and its tree,
/// for the request); the rest is sykli's.
pub fn gate_here(print_only: bool, wait: Duration) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = repo_at(&cwd)?;
    let listing = gitconfig::clone_listing(&top)?;
    let Some(gate) = gate_of(&top, &listing)? else {
        eprintln!(
            "ohjaa gate: {} has no gate: origin names no entry (ohjaa adopt), and there is \
             no sykli.json in the repository",
            top.display()
        );
        return Ok(2);
    };
    let contract = gate.contract.clone();
    let quoted = quote_path(&contract);
    let (run_help, verify_help) = if print_only {
        (String::new(), String::new())
    } else {
        (sykli_help("run"), sykli_help("verify"))
    };
    let run_facts = facts_for_sykli(&run_help, Some(&top), &common);
    let verify_facts = facts_for_sykli(&verify_help, Some(&top), &common);
    let extra = |facts: &[std::ffi::OsString]| {
        if facts.is_empty() {
            String::new()
        } else {
            format!(" {}", shown(facts))
        }
    };
    let run = format!(
        "sykli run {quoted}{} --json > .sykli/receipt.json",
        extra(&run_facts)
    );
    let verify = format!(
        "sykli verify .sykli/receipt.json --contract {quoted}{} --commit HEAD",
        extra(&verify_facts)
    );
    println!(
        "ohjaa gate: the gate is {} ({})",
        contract.display(),
        gate.whose
    );
    if print_only {
        println!(
            "cd {} && mkdir -p .sykli && {run} && {verify}",
            quote_path(&top)
        );
        return Ok(0);
    }
    println!("ohjaa gate: in {}", top.display());
    let sykli_dir = top.join(".sykli");
    std::fs::create_dir_all(&sykli_dir)
        .with_context(|| format!("cannot create {}", sykli_dir.display()))?;
    let receipt_path = sykli_dir.join("receipt.json");
    let request = Request::record(&top, &common, &gate);
    // A sykli with durable runs gets the run detached, and is asked about it
    // by its id.
    let status_help = if process::names_flag(&run_help, "--detach") {
        lifecycle::sykli_status_help()
    } else {
        lifecycle::StatusHelp::default()
    };
    let ran = if status_help.runs {
        println!("$ sykli run {quoted}{} --detach --json", extra(&run_facts));
        detached(
            &top,
            &common,
            &contract,
            &run_facts,
            &receipt_path,
            request.as_ref(),
            status_help,
            wait,
        )?
    } else {
        println!("$ {run}");
        foreground(&top, &contract, &run_facts, &receipt_path, request.as_ref())?
    };
    if let Err(code) = ran {
        return Ok(code);
    }
    println!("$ {verify}");
    let status = Command::new("sykli")
        .args(["verify", ".sykli/receipt.json", "--contract"])
        .arg(&contract)
        .args(&verify_facts)
        .args(["--commit", "HEAD"])
        .current_dir(&top)
        .status()
        .context("cannot run sykli")?;
    let code = status.code().unwrap_or(1);
    if status.success() {
        println!("ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so HEAD is proven");
    } else {
        println!("ohjaa gate: failed: sykli verify exited {code}");
    }
    Ok(code)
}

/// `sykli run <contract> --json > .sykli/receipt.json`, in the foreground,
/// as always. Then what sykli gave is filled in: the run id the receipt
/// names (a sykli with durable runs names one), the lock it read, and the
/// receipt it keeps of the run. `Err(code)` when the run failed.
fn foreground(
    top: &Path,
    contract: &Path,
    facts: &[std::ffi::OsString],
    receipt_path: &Path,
    request: Option<&Request>,
) -> Result<std::result::Result<(), i32>> {
    let receipt = std::fs::File::create(receipt_path)
        .with_context(|| format!("cannot write {}", receipt_path.display()))?;
    let started_ms = now_ms();
    let status = Command::new("sykli")
        .arg("run")
        .arg(contract)
        .args(facts)
        .arg("--json")
        .current_dir(top)
        .stdout(receipt)
        .status()
        .context("cannot run sykli")?;
    if let Some(request) = request {
        let printed = std::fs::read(receipt_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|printed| printed["schema"] == "sykli-receipt.v1");
        match printed {
            Some(printed) => {
                let kept = kept_receipt(top, &printed, started_ms).unwrap_or_else(|| ReceiptRef {
                    path: receipt_path.to_string_lossy().into_owned(),
                    digest: None,
                });
                request.fill("the receipt", |validation| {
                    validation.run = printed["run"].as_str().map(str::to_owned);
                    // What ran, in the receipt's words: a dirty run, or one
                    // of another tree, is never integrated.
                    validation.ran =
                        printed["subject"]["tree_oid"]
                            .as_str()
                            .map(|tree| sessions::Ran {
                                tree: tree.to_owned(),
                                dirty: printed["subject"]["dirty"].as_bool().unwrap_or(true),
                            });
                    validation.lock_digest = printed["lock_digest"].as_str().map(str::to_owned);
                    if validation.contract_digest.is_none() {
                        validation.contract_digest =
                            printed["contract_hash"].as_str().map(str::to_owned);
                    }
                    validation.receipt = Some(kept);
                });
            }
            None => println!("ohjaa gate: sykli wrote no receipt: none is recorded"),
        }
    }
    if !status.success() {
        let code = status.code().unwrap_or(1);
        println!("ohjaa gate: failed: sykli run exited {code}; the receipt says why");
        return Ok(Err(code));
    }
    Ok(Ok(()))
}

/// `ohjaa gate`'s exit when sykli has not said: the run is still going past
/// `--wait`, or it ended in a state other than completed (interrupted, lost,
/// indeterminate). Not proven, and not failed (EX_TEMPFAIL: ask again later).
pub const NOT_YET: i32 = 75;

/// `sykli run <contract> --detach --json`, which answers `{schema:
/// "sykli-run.v1", run}` once the run is recorded: the run id is filled in
/// at once, then sykli is asked about the run (`sykli status --run <id>
/// --json`) until it ends or `wait` has passed, what it says ran is filled
/// in, and the receipt it names is filled in and saved as
/// `.sykli/receipt.json`. Should this gate be stopped meanwhile, the run goes
/// on, and its id is in the record. `Err(code)` when it did not pass:
/// [`NOT_YET`] when sykli has not said it passed or failed.
#[allow(clippy::too_many_arguments)]
fn detached(
    top: &Path,
    common: &Path,
    contract: &Path,
    facts: &[std::ffi::OsString],
    receipt_path: &Path,
    request: Option<&Request>,
    help: lifecycle::StatusHelp,
    wait: Duration,
) -> Result<std::result::Result<(), i32>> {
    let output = Command::new("sykli")
        .arg("run")
        .arg(contract)
        .args(facts)
        .args(["--detach", "--json"])
        .current_dir(top)
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("cannot run sykli")?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        println!("ohjaa gate: failed: sykli run --detach exited {code}");
        return Ok(Err(code));
    }
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let Some(run) = answer["run"]
        .as_str()
        .filter(|_| answer["schema"] == "sykli-run.v1")
        .map(str::to_owned)
    else {
        println!(
            "ohjaa gate: failed: sykli run --detach named no run (sykli-run.v1): {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
        return Ok(Err(1));
    };
    if let Some(request) = request {
        request.fill("sykli's run id", |validation| {
            validation.run = Some(run.clone())
        });
    }
    println!("ohjaa gate: sykli run {run} requested");
    let ask_again = format!("ask again with: sykli status --run {run}");
    let mut poll = Duration::from_millis(100);
    let started = Instant::now();
    let mut waiting_said = false;
    let mut subject_filled = false;
    let status = loop {
        let status = match lifecycle::sykli_run_status(top, Some(common), help, &run) {
            Ok(status) => status,
            Err(why) => {
                println!(
                    "ohjaa gate: not proven: sykli could not say what became of run {run}: \
                     {why}; it is recorded, {ask_again}"
                );
                return Ok(Err(NOT_YET));
            }
        };
        // What ran, as soon as sykli says: a dirty run is never integrated.
        if !subject_filled {
            if let (Some(request), Some(ran)) = (request, lifecycle::ran_of_status(&status)) {
                request.fill("what sykli ran", |validation| validation.ran = Some(ran));
                subject_filled = true;
            }
        }
        if !matches!(status["state"].as_str(), Some("requested" | "running")) {
            break status;
        }
        if started.elapsed() >= wait {
            println!(
                "ohjaa gate: not proven yet: {}; still running after {}s, so not failed \
                 either. It is recorded; {ask_again}",
                lifecycle::describe_run_status(&run, &status),
                wait.as_secs()
            );
            return Ok(Err(NOT_YET));
        }
        if !waiting_said && started.elapsed() > Duration::from_secs(2) {
            println!(
                "ohjaa gate: waiting for sykli run {run} (at most {}s)",
                wait.as_secs()
            );
            waiting_said = true;
        }
        std::thread::sleep(poll.min(wait.saturating_sub(started.elapsed())));
        poll = (poll * 2).min(Duration::from_secs(1));
    };
    println!(
        "ohjaa gate: {}",
        lifecycle::describe_run_status(&run, &status)
    );
    let receipt = lifecycle::receipt_of_status(&status, top);
    if let Some(request) = request {
        request.fill("the receipt", |validation| {
            validation.receipt = receipt.clone();
            validation.lock_digest = status["lock_digest"].as_str().map(str::to_owned);
            if validation.contract_digest.is_none() {
                validation.contract_digest = status["contract_digest"].as_str().map(str::to_owned);
            }
            if let Some(ran) = lifecycle::ran_of_status(&status) {
                validation.ran = Some(ran);
            }
        });
    }
    if lifecycle::run_ended_unknown(&status) {
        println!(
            "ohjaa gate: not proven: sykli says the run ended without completing, so what it \
             ran is unknown; not failed. It is recorded; {ask_again}"
        );
        return Ok(Err(NOT_YET));
    }
    let Some(receipt) = receipt else {
        println!(
            "ohjaa gate: not proven: sykli names no receipt for run {run}; it is recorded, \
             {ask_again}"
        );
        return Ok(Err(NOT_YET));
    };
    // Saved where a push and the marks look, as the foreground run leaves it.
    let bytes = std::fs::read(&receipt.path)
        .with_context(|| format!("cannot read sykli's receipt {}", receipt.path))?;
    let partial = receipt_path.with_extension("json.partial");
    std::fs::write(&partial, &bytes)
        .and_then(|()| std::fs::rename(&partial, receipt_path))
        .with_context(|| format!("cannot write {}", receipt_path.display()))?;
    let outcome = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|receipt| receipt["outcome"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "unreadable".to_owned());
    if !matches!(outcome.as_str(), "passed" | "cached") {
        println!("ohjaa gate: failed: the receipt of sykli run {run} says {outcome}");
        return Ok(Err(1));
    }
    Ok(Ok(()))
}
