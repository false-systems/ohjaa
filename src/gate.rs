//! `ohjaa gate`: run the gate a push is judged by for the repository here,
//! and leave the receipt that proves HEAD: the push finds it, and so do the
//! marks (`.sykli/receipt.json`, the copy `sykli run` keeps under
//! `.sykli/receipts/`, and the one sykli keeps in the run's own folder under
//! the clone's common dir, which outlives the worktree).
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

use anyhow::{Context, Result};
use serde_json::Value;

use crate::derived::{GateAt, Gates, Objects, Own};
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
    /// For `--print`: the derived gate is not kept there yet.
    unwritten: bool,
    /// Derived from what `sykli init` writes for HEAD's tree: its run proves
    /// HEAD only as committed.
    inferred: bool,
}

/// What git said of this checkout, in one `git cat-file --batch`: HEAD, its
/// tree, and the observed branch's tip as this clone last saw it.
#[derive(Default)]
struct Read {
    head: Option<(String, String)>,
    tip: Option<String>,
    /// Why git could not say, when it could not.
    failed: Option<String>,
}

/// The gate of the repository at `top`: for an entry origin's push URL
/// names, the gate of what this checkout runs; else the repository's own
/// `sykli.json`; with whose it is. A checkout with a `sykli.json` has the
/// gate derived from it and the `sykli.lock` beside it, read as files, no
/// git process: when they are HEAD's, it is exactly HEAD's gate, and when
/// they are edited, the gate of the commit they will be, so its receipt
/// proves that commit (a receipt binds the contract it ran under, and every
/// commit is judged by the gate derived from the commit). A checkout with
/// none has HEAD's gate, derived as every commit's is: from HEAD's own
/// `sykli.json`, else from what `sykli init` writes for HEAD's tree (read
/// through the checkout's one git process, and asked of sykli in a scratch
/// checkout of the tree). Kept in ohjaa's state for a
/// run (`keep`), else in a folder of this process's own.
fn gate_of(
    top: &Path,
    common: &Path,
    listing: &Listing,
    objects: &mut Option<Objects>,
    (gates, keep): (&Gates, bool),
) -> Result<Option<Gate>> {
    let home = absolute(home::ohjaa_home()?);
    if let Some(id) = push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)
    {
        if let Some(entry) = home::find_entry(&home, &id)? {
            let whose = format!("the entry for {} (branch {})", id.slug(), entry.branch);
            let gate = crate::mark::gate_for_entry(&id, &entry, top, common);
            let (at, from) = match checkout_own(top).map_err(|why| anyhow::anyhow!(why))? {
                Some(own) => {
                    let at = GateAt::Derived(Box::new(crate::derived::derive(&own, &gate.overlay)));
                    let from = at.describe("in this checkout");
                    (at, from)
                }
                None => {
                    let at = head_gate(top, common, &gate.overlay, objects, gates);
                    let from = at.describe("at HEAD");
                    (at, from)
                }
            };
            let inferred = matches!(&at, GateAt::Derived(derived) if derived.inferred.is_some());
            let no_gate = |why: String| {
                anyhow::anyhow!(
                    "{} has an entry but no gate in this checkout: {why}",
                    id.slug()
                )
            };
            // `--print` writes nothing: it names where the run keeps the
            // gate, and says when it is not written yet.
            let kept = match (&at, keep) {
                (GateAt::Derived(derived), false) => gates.kept_at(derived),
                _ => None,
            };
            let (contract, unwritten) = match kept {
                Some((path, there)) => (path, !there),
                None => (at.contract(gates, keep).map_err(no_gate)?, false),
            };
            return Ok(Some(Gate {
                contract,
                whose: format!("{whose}: {from}"),
                branch: Some(entry.branch.clone()),
                unwritten,
                inferred,
            }));
        }
    }
    let own = top.join("sykli.json");
    Ok(own.is_file().then(|| Gate {
        contract: own,
        whose: "the repository's own sykli.json (no entry)".to_owned(),
        branch: None,
        unwritten: false,
        inferred: false,
    }))
}

/// HEAD's gate, for a checkout with no `sykli.json`: derived as every
/// commit's is, from what HEAD holds, through the checkout's one git
/// process.
fn head_gate(
    top: &Path,
    common: &Path,
    overlay: &crate::derived::Overlay,
    objects: &mut Option<Objects>,
    gates: &Gates,
) -> GateAt {
    let holds = match opened(objects, top).and_then(|objects| objects.holds_at("HEAD")) {
        Ok(holds) => holds,
        Err(why) => {
            return GateAt::Missing(format!(
                "this checkout has no sykli.json, and HEAD's contract cannot be read ({why})"
            ))
        }
    };
    GateAt::of(overlay, "HEAD", holds, |tree| {
        gates.inferred(common, tree, Path::new("sykli"), process::LOCAL)
    })
}

/// The checkout's own `sykli.json` and the `sykli.lock` beside it, as files;
/// `None` without a `sykli.json`. A symbolic link of that name is said so.
fn checkout_own(top: &Path) -> std::result::Result<Option<Own>, String> {
    let read = |name: &str| -> std::result::Result<Option<Vec<u8>>, String> {
        let path = top.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
            Ok(meta) if meta.file_type().is_symlink() => Err(format!(
                "{} is a symbolic link; a gate is judged by a file of that name",
                path.display()
            )),
            Ok(meta) if !meta.is_file() => Err(format!("{} is not a file", path.display())),
            Ok(_) => std::fs::read(&path)
                .map(Some)
                .map_err(|error| format!("cannot read {}: {error}", path.display())),
        }
    };
    let Some(contract) = read("sykli.json")? else {
        return Ok(None);
    };
    Ok(Some(Own {
        contract,
        lock: read("sykli.lock")?,
    }))
}

/// This checkout's `git cat-file --batch`, started the first time it is
/// needed.
fn opened<'a>(objects: &'a mut Option<Objects>, top: &Path) -> Result<&'a mut Objects, String> {
    if objects.is_none() {
        *objects = Some(Objects::in_checkout(top)?);
    }
    Ok(objects.as_mut().expect("just opened"))
}

/// HEAD, its tree and the observed branch's tip, through `objects`.
fn read_here(objects: &mut Option<Objects>, top: &Path, branch: Option<&str>) -> Read {
    let objects = match opened(objects, top) {
        Ok(objects) => objects,
        Err(why) => {
            return Read {
                failed: Some(why),
                ..Read::default()
            }
        }
    };
    let head = match objects.commit("HEAD") {
        Ok(head) => head.map(|head| (head.oid, head.tree)),
        Err(why) => {
            return Read {
                failed: Some(why),
                ..Read::default()
            }
        }
    };
    let tip = branch.and_then(|branch| {
        objects
            .get(&format!("refs/remotes/origin/{branch}"))
            .ok()
            .flatten()
            .map(|tip| tip.oid)
    });
    Read {
        head,
        tip,
        failed: None,
    }
}

/// `sykli <command> --help`, said on stdout or stderr; empty when sykli
/// cannot be asked.
pub(crate) fn sykli_help(command: &str) -> String {
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
pub(crate) fn pinned(contract: &Path) -> Option<String> {
    let lock = std::fs::read(contract.with_file_name("sykli.lock")).ok()?;
    crate::derived::pinned_in(&lock, contract.file_name()?.to_str()?)
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
    /// HEAD and its tree (through the checkout's one git process, which the
    /// gate may have started already), the contract and the hash its lock
    /// pins. Says why when it is not recorded; never stops the gate.
    fn record(
        top: &Path,
        common: &Path,
        gate: &Gate,
        objects: &mut Option<Objects>,
    ) -> Option<Request> {
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
        // saw it, from the checkout's one git process.
        let read = read_here(objects, top, gate.branch.as_deref());
        if let Some(why) = read.failed {
            return not(format!("git cannot say HEAD here ({why})"));
        }
        let Some((commit, tree)) = read.head else {
            return not("HEAD has no commit yet".to_owned());
        };
        // Never integrated before sykli says what ran (`settle_kind`): a run
        // of uncommitted changes at the tip is no run of the tip.
        let observed_branch = gate.branch.as_ref().map(|name| ObservedBranch {
            name: name.clone(),
            tip: read.tip,
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
            not_dispatched: None,
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
/// repository's config), and inside a session, or in a checkout with no
/// `sykli.json`, one more (HEAD and its tree); for a tree whose gate sykli
/// init writes, two more (its scratch checkout); the rest is sykli's.
pub fn gate_here(print_only: bool, wait: Duration) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = repo_at(&cwd)?;
    let listing = gitconfig::clone_listing(&top)?;
    // The gate comes from the checkout's files; the request alone asks git
    // (HEAD, its tree and the branch's tip), in one process.
    let mut objects = None;
    let gates = Gates::of_state();
    let Some(gate) = gate_of(&top, &common, &listing, &mut objects, (&gates, !print_only))? else {
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
    if gate.inferred {
        println!(
            "ohjaa gate: it declares what HEAD's tree tracks, so it proves HEAD as committed: \
             commit first (a run of changes beside HEAD runs another tree)"
        );
    }
    if gate.unwritten {
        println!(
            "ohjaa gate: it is not written yet: `ohjaa gate` writes it there when it runs \
             (--print writes nothing)"
        );
    }
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
    let request = Request::record(&top, &common, &gate, &mut objects);
    // Nothing more is asked of git: its process ends before sykli runs.
    drop(objects);
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
