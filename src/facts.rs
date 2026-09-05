//! Derive an `ohjaa-cycle-facts.v1` snapshot from the tools that own each
//! fact. Ohjaa stores none of it: every run asks Toimija, Teko, and Sauma
//! again, and an inspection that fails is reported as `inconclusive` — which
//! the reducer holds on — never as an empty list or a passing gate.
//!
//! The composition owner seals `cycle.v1`; the artifacts it references live
//! wherever that owner keeps them, so their paths are inputs here. Everything
//! else — the repository point, sessions, gate receipts, Teko's work state —
//! is derived on demand from the repository this runs in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::cycle::{Cycle, Decision, Evidence, Facts};

/// Where the sealed upstream artifacts live. Absent verdict means "not yet".
pub struct ArtifactPaths {
    pub work: PathBuf,
    pub decision: PathBuf,
    pub qa_plan: PathBuf,
    pub verdict: Option<PathBuf>,
}

/// One derived snapshot plus what the reducer's caller needs to act on it:
/// the sealed facts JSON, the current repository point, and the attempt
/// numbers each leg would launch with.
pub struct Derived {
    pub facts: Facts,
    pub sealed: Value,
    pub repository_point: RepositoryPoint,
    pub implementation_attempt: u32,
    pub qa_attempt: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepositoryPoint {
    pub id: String,
    pub consistency_token: String,
    pub tree_oid: String,
}

#[derive(Deserialize)]
struct RepositoryPointExport {
    repository: RepositoryPoint,
}

#[derive(Deserialize)]
struct SessionsReport {
    sessions: Vec<Session>,
    coverage: Coverage,
}

#[derive(Deserialize)]
struct Coverage {
    status: String,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    liveness: String,
    cycle_action: Option<CycleAction>,
    /// What Toimija saw the session do: commit, push, open_pr.
    #[serde(default)]
    effects_observed: Vec<String>,
    /// "partial" when Toimija could not inspect every effect.
    #[serde(default)]
    effects_coverage: Option<String>,
}

#[derive(Deserialize)]
struct CycleAction {
    cycle: String,
    leg: String,
}

#[derive(Deserialize)]
struct GateStatus {
    gate: String,
    status: String,
    receipt: Option<String>,
}

#[derive(Deserialize)]
struct GateReceipt {
    exit_code: Option<i32>,
}

#[derive(Deserialize)]
struct WorkArtifact {
    packet: WorkPacket,
}

#[derive(Deserialize)]
struct WorkPacket {
    work: WorkRef,
    #[serde(default)]
    extensions: Value,
}

#[derive(Deserialize)]
struct WorkRef {
    id: String,
}

#[derive(Deserialize)]
struct TekoCurrent {
    active: Option<ActiveWork>,
}

#[derive(Deserialize)]
struct ActiveWork {
    work: WorkRef,
    #[serde(default)]
    status: Option<WorkflowStatus>,
}

#[derive(Deserialize, Default)]
struct WorkflowStatus {
    #[serde(default)]
    obligations: Vec<ObligationStatus>,
}

#[derive(Deserialize)]
struct ObligationStatus {
    id: String,
    status: String,
}

#[derive(Deserialize)]
struct DecisionArtifact {
    status: String,
    problem: Option<Problem>,
    #[serde(default)]
    signer: Option<Signer>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    repository_point: Option<DecisionPoint>,
}

#[derive(Deserialize)]
struct Signer {
    identity: String,
}

#[derive(Deserialize)]
struct DecisionPoint {
    consistency_token: String,
}

#[derive(Deserialize)]
struct Problem {
    code: String,
}

#[derive(Deserialize)]
struct Verdict {
    schema: String,
    verdict: String,
    plan: DigestRef,
    repository: VerdictRepository,
    #[serde(default)]
    independence: Independence,
}

#[derive(Deserialize)]
struct DigestRef {
    digest: String,
}

#[derive(Deserialize)]
struct VerdictRepository {
    #[serde(rename = "final")]
    final_point: String,
}

#[derive(Deserialize, Default)]
struct Independence {
    #[serde(default)]
    test_sessions: Vec<SessionRef>,
}

#[derive(Deserialize)]
struct SessionRef {
    id: String,
}

#[derive(Deserialize)]
struct WitnessRecord {
    record: String,
    work: String,
}

/// What the verdict path holds, before it becomes reducer evidence.
enum VerdictFact {
    /// No verdict yet.
    Missing,
    /// A verdict exists but describes a repository point the tree has left:
    /// QA has to run again, it is not a judgement about this tree.
    Stale,
    /// Kelpo judged this tree: pass, fail, or an inconclusive that a human
    /// must read (independence, wrong plan, unverifiable envelope).
    Judged(Evidence),
}

/// The facts a session listing yields for one leg, before budget policy:
/// how many finished (closed or gone quiet) and how many are still active.
struct LegSessions {
    finished: usize,
    live: usize,
}

pub fn derive(root: &Path, cycle: &Cycle, paths: &ArtifactPaths) -> Result<Derived> {
    let repository_point = repository_point(root)?;
    let work: WorkArtifact = sealed_artifact(&paths.work, &cycle.work.digest, "work")?;
    let work_id = work.packet.work.id.clone();
    let required_gates: Vec<String> = work
        .packet
        .extensions
        .get("toimija.required_gates.v1")
        .and_then(Value::as_array)
        .map(|gates| {
            gates
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    let decision = decision(cycle, &paths.decision);
    let qa_plan = match sealed_digest(&paths.qa_plan) {
        Ok(digest) if digest == cycle.qa_plan.digest => Evidence::Current,
        Ok(_) => inconclusive("qa plan digest differs from the cycle"),
        Err(error) => inconclusive(&format!("qa plan: {error:#}")),
    };

    let sessions = sessions(root, &cycle.sauma.digest);
    let effects = match &sessions {
        Ok(report) => effects(report, &cycle.effects),
        Err(error) => inconclusive(&format!("sessions: {error:#}")),
    };
    let teko = teko_current(root);
    let (implementation, implementation_attempt) = match (&sessions, &teko) {
        (Ok(report), Ok(current)) => implementation_evidence(
            &leg_sessions(report, "implementation"),
            current,
            &work_id,
            cycle.implementation.max_attempts,
        ),
        (Err(error), _) | (_, Err(error)) => {
            (inconclusive(&format!("implementation: {error:#}")), 0)
        }
    };
    let implementation_sessions: Vec<String> = sessions
        .as_ref()
        .map(|report| {
            report
                .iter()
                .filter(|session| {
                    session
                        .cycle_action
                        .as_ref()
                        .is_some_and(|a| a.leg == "implementation")
                })
                .map(|session| session.id.clone())
                .collect()
        })
        .unwrap_or_default();

    let gates = gates(root, &required_gates);
    let verdict_fact = verdict(
        paths.verdict.as_deref(),
        &cycle.qa_plan.digest,
        &repository_point.consistency_token,
        &implementation_sessions,
    );
    // QA is done when Kelpo judged this tree, whatever it said: the reducer
    // reads the judgement next, and an inconclusive one holds for a human. A
    // stale or missing verdict sends the leg's sessions to decide between
    // pending, live, and spent.
    let (qa, qa_attempt) = match &sessions {
        Ok(report) => {
            let leg = leg_sessions(report, "qa");
            let attempt = (leg.finished + leg.live) as u32;
            match verdict_fact {
                VerdictFact::Judged(_) => (Evidence::Current, attempt),
                VerdictFact::Stale | VerdictFact::Missing => {
                    leg_evidence(&leg, 1 + cycle.retry.worker_failure)
                }
            }
        }
        Err(error) => (inconclusive(&format!("sessions: {error:#}")), 0),
    };
    let verdict = match verdict_fact {
        VerdictFact::Judged(evidence) => evidence,
        VerdictFact::Stale | VerdictFact::Missing => Evidence::Pending,
    };
    let closure = match &teko {
        Ok(current) => closure(root, current, &work_id),
        Err(error) => inconclusive(&format!("closure: {error:#}")),
    };

    let facts_body = serde_json::json!({
        "schema": "ohjaa-cycle-facts.v1",
        "cycle": cycle.sauma.digest,
        "decision": decision_label(&decision),
        "effects": evidence_label(&effects),
        "qa_plan": evidence_label(&qa_plan),
        "implementation": evidence_label(&implementation),
        "gates": evidence_label(&gates),
        "qa": evidence_label(&qa),
        "verdict": evidence_label(&verdict),
        "closure": evidence_label(&closure),
    });
    let sealed = stamp(
        &facts_body,
        &repository_point.consistency_token,
        &cycle.sauma.digest,
    )?;
    let facts: Facts = serde_json::from_value(sealed.clone())
        .context("sauma stamp returned facts that do not parse")?;
    Ok(Derived {
        facts,
        sealed,
        repository_point,
        implementation_attempt,
        qa_attempt,
    })
}

fn inconclusive(reason: &str) -> Evidence {
    eprintln!("ohjaa: inconclusive — {reason}");
    Evidence::Inconclusive
}

fn decision_label(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allow => "allow",
        Decision::Deny => "deny",
        Decision::Hold => "hold",
        Decision::Escalate => "escalate",
    }
}

fn evidence_label(evidence: &Evidence) -> &'static str {
    match evidence {
        Evidence::Pending => "pending",
        Evidence::Current => "current",
        Evidence::Failed => "failed",
        Evidence::Inconclusive => "inconclusive",
    }
}

fn run_json<T: serde::de::DeserializeOwned>(mut command: Command, what: &str) -> Result<T> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {what}"))?;
    // A non-zero exit that still carries the versioned answer is the answer:
    // `toimija gates status` exits 1 to say "stale", not "could not look".
    // Only an unparseable answer is a failed inspection.
    if output.stdout.trim_ascii().is_empty() {
        anyhow::bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "{what} returned invalid JSON (exit {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })
}

fn toimija(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("toimija");
    command.arg("-q").args(args).current_dir(root);
    command
}

fn repository_point(root: &Path) -> Result<RepositoryPoint> {
    let export: RepositoryPointExport = run_json(
        toimija(root, &["repository-point", "--json"]),
        "toimija repository-point",
    )?;
    Ok(export.repository)
}

/// `sauma verify` must accept the file, and the envelope digest is the
/// artifact's identity: a mismatch against the cycle is a different artifact,
/// whatever the file is called.
fn sealed_digest(path: &Path) -> Result<String> {
    let status = Command::new("sauma")
        .arg("verify")
        .arg(path)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("cannot run sauma verify for {}", path.display()))?;
    anyhow::ensure!(
        status.success(),
        "{} is not a verified Sauma artifact",
        path.display()
    );
    let value: Value = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?,
    )
    .with_context(|| format!("{} is not JSON", path.display()))?;
    value
        .pointer("/sauma/digest")
        .and_then(Value::as_str)
        .map(String::from)
        .with_context(|| format!("{} has no sauma digest", path.display()))
}

fn sealed_artifact<T: serde::de::DeserializeOwned>(
    path: &Path,
    expected: &str,
    name: &str,
) -> Result<T> {
    let digest = sealed_digest(path)?;
    anyhow::ensure!(
        digest == expected,
        "{name} artifact {} is {digest}, the cycle references {expected}",
        path.display()
    );
    serde_json::from_slice(&std::fs::read(path)?)
        .with_context(|| format!("invalid {name} artifact {}", path.display()))
}

fn decision(cycle: &Cycle, path: &Path) -> Decision {
    let artifact: DecisionArtifact = match sealed_artifact(path, &cycle.decision.digest, "decision")
    {
        Ok(artifact) => artifact,
        Err(error) => {
            eprintln!("ohjaa: hold — decision: {error:#}");
            return Decision::Hold;
        }
    };
    let hold = |reason: &str| {
        eprintln!("ohjaa: hold — decision: {reason}");
        Decision::Hold
    };
    // A decision is valid only at the cycle's input point, until it expires,
    // and only from a signer the composition owner allows to launch.
    if let (Some(anchor), Some(point)) = (&cycle.sauma.anchored_at, &artifact.repository_point) {
        if &point.consistency_token != anchor {
            return hold(
                "decision was made at a different repository point than the cycle's input",
            );
        }
    }
    if let Some(expires_at) = &artifact.expires_at {
        if expires_at.as_str() < utc_now().as_str() {
            return hold(&format!("decision expired at {expires_at}"));
        }
    }
    if let Some(allowed) = &cycle.decision.signers {
        match &artifact.signer {
            Some(signer) if allowed.contains(&signer.identity) => {}
            Some(signer) => {
                return hold(&format!(
                    "decision signed by `{}`, which the cycle does not allow to launch",
                    signer.identity
                ))
            }
            None => return hold("decision carries no signer, and the cycle names who may launch"),
        }
    }
    match artifact.status.as_str() {
        "decided" => Decision::Allow,
        "inconclusive" => Decision::Hold,
        "refused"
            if artifact
                .problem
                .as_ref()
                .is_some_and(|problem| problem.code == "policy.hard_conflict") =>
        {
            Decision::Escalate
        }
        "refused" => Decision::Deny,
        other => hold(&format!("decision status `{other}` is unknown")),
    }
}

/// Now, as `YYYY-MM-DDTHH:MM:SSZ`, comparable as a string with the RFC 3339
/// UTC timestamps the artifacts carry. Civil-date arithmetic inline so this
/// crate keeps its dependency list.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (h, m, s) = ((secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    // Howard Hinnant's days-to-civil.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn sessions(root: &Path, cycle: &str) -> Result<Vec<Session>> {
    let report: SessionsReport =
        run_json(toimija(root, &["sessions", "--json"]), "toimija sessions")?;
    anyhow::ensure!(
        report.coverage.status == "available",
        "session coverage is {} — absence is not evidence",
        report.coverage.status
    );
    Ok(report
        .sessions
        .into_iter()
        .filter(|session| {
            session
                .cycle_action
                .as_ref()
                .is_some_and(|a| a.cycle == cycle)
        })
        .collect())
}

fn leg_sessions(sessions: &[Session], leg: &str) -> LegSessions {
    let mut result = LegSessions {
        finished: 0,
        live: 0,
    };
    for session in sessions
        .iter()
        .filter(|session| session.cycle_action.as_ref().is_some_and(|a| a.leg == leg))
    {
        // Only an active heartbeat is live. A session that went quiet is a
        // finished attempt: relaunched within budget, never waited on forever.
        if session.liveness == "active" {
            result.live += 1;
        } else {
            result.finished += 1;
        }
    }
    result
}

/// Evidence for a leg whose success is judged elsewhere (by Teko for the
/// implementation, by the verdict for QA): here only pending, live, or spent.
fn leg_evidence(leg: &LegSessions, max_attempts: u32) -> (Evidence, u32) {
    let attempt = (leg.finished + leg.live) as u32;
    if leg.live > 0 {
        (
            inconclusive("a session for this leg is still live"),
            attempt,
        )
    } else if leg.finished as u32 >= max_attempts {
        (Evidence::Failed, attempt)
    } else {
        (Evidence::Pending, attempt)
    }
}

/// The implementation is done when Teko says the tree changed within scope,
/// never when a process exited 0. No active work means nothing is left to
/// implement: closure will say what happened.
fn implementation_evidence(
    leg: &LegSessions,
    current: &TekoCurrent,
    work_id: &str,
    max_attempts: u32,
) -> (Evidence, u32) {
    let attempt = (leg.finished + leg.live) as u32;
    let raw_id = work_id.strip_prefix("work_").unwrap_or(work_id);
    match &current.active {
        None => (Evidence::Current, attempt),
        Some(active) if active.work.id != raw_id => (
            inconclusive(&format!(
                "Teko has different active work: {}",
                active.work.id
            )),
            attempt,
        ),
        Some(active) => {
            let resolved = |id: &str| {
                active.status.as_ref().is_some_and(|status| {
                    status
                        .obligations
                        .iter()
                        .any(|obligation| obligation.id == id && obligation.status == "resolved")
                })
            };
            if resolved("repository-changed") && resolved("scope") {
                (Evidence::Current, attempt)
            } else if leg.live > 0 {
                (
                    inconclusive("an implementation session is still live"),
                    attempt,
                )
            } else if leg.finished as u32 >= max_attempts {
                (Evidence::Failed, attempt)
            } else {
                (Evidence::Pending, attempt)
            }
        }
    }
}

/// Every effect Toimija observed on this cycle's sessions must be declared.
fn effects(sessions: &[Session], declared: &[crate::cycle::Effect]) -> Evidence {
    let mut observed: Vec<&str> = Vec::new();
    for session in sessions {
        if session.effects_coverage.as_deref() == Some("partial") {
            return inconclusive(&format!(
                "session {} could not observe every effect",
                session.id
            ));
        }
        observed.extend(session.effects_observed.iter().map(String::as_str));
    }
    let allowed = |effect: &str| {
        declared.iter().any(|declared| match declared {
            crate::cycle::Effect::Commit => effect == "commit",
            crate::cycle::Effect::Push => effect == "push",
            crate::cycle::Effect::OpenPr => effect == "open_pr",
        })
    };
    match observed.iter().find(|effect| !allowed(effect)) {
        Some(effect) => {
            eprintln!("ohjaa: undeclared effect observed: {effect}");
            Evidence::Failed
        }
        None => Evidence::Current,
    }
}

fn teko_current(root: &Path) -> Result<TekoCurrent> {
    let mut command = Command::new("teko");
    command.arg("current").current_dir(root);
    run_json(command, "teko current")
}

fn gates(root: &Path, required: &[String]) -> Evidence {
    if required.is_empty() {
        return Evidence::Current;
    }
    let statuses: Vec<GateStatus> = match run_json(
        toimija(root, &["gates", "status", "--json"]),
        "toimija gates status",
    ) {
        Ok(statuses) => statuses,
        Err(error) => return inconclusive(&format!("gates: {error:#}")),
    };
    let mut pending = false;
    for gate in required {
        let Some(status) = statuses.iter().find(|status| &status.gate == gate) else {
            return inconclusive(&format!("gate `{gate}` is not defined in this repository"));
        };
        if status.status != "current" {
            pending = true;
            continue;
        }
        let Some(receipt) = &status.receipt else {
            pending = true;
            continue;
        };
        let path = root
            .join(".toimija/runs/gates")
            .join(format!("{receipt}.json"));
        let receipt: GateReceipt = match sealed_digest(&path).and_then(|_| {
            serde_json::from_slice(&std::fs::read(&path)?).context("gate receipt is not JSON")
        }) {
            Ok(receipt) => receipt,
            Err(error) => return inconclusive(&format!("gate `{gate}` receipt: {error:#}")),
        };
        if receipt.exit_code != Some(0) {
            return Evidence::Failed;
        }
    }
    if pending {
        Evidence::Pending
    } else {
        Evidence::Current
    }
}

fn verdict(
    path: Option<&Path>,
    plan_digest: &str,
    final_token: &str,
    implementation_sessions: &[String],
) -> VerdictFact {
    let Some(path) = path else {
        return VerdictFact::Missing;
    };
    if !path.exists() {
        return VerdictFact::Missing;
    }
    let verdict: Verdict = match sealed_digest(path)
        .and_then(|_| serde_json::from_slice(&std::fs::read(path)?).context("verdict is not JSON"))
    {
        Ok(verdict) => verdict,
        Err(error) => return VerdictFact::Judged(inconclusive(&format!("verdict: {error:#}"))),
    };
    if verdict.schema != "kelpo-verdict.v1" {
        return VerdictFact::Judged(inconclusive("verdict is not a kelpo-verdict.v1"));
    }
    if verdict.plan.digest != plan_digest {
        return VerdictFact::Judged(inconclusive(
            "verdict was judged against a different QA plan",
        ));
    }
    if verdict.repository.final_point != final_token {
        eprintln!("ohjaa: verdict is anchored to a repository point that has moved; QA runs again");
        return VerdictFact::Stale;
    }
    if verdict
        .independence
        .test_sessions
        .iter()
        .any(|session| implementation_sessions.contains(&session.id))
    {
        return VerdictFact::Judged(inconclusive(
            "QA evidence came from the implementation session",
        ));
    }
    VerdictFact::Judged(match verdict.verdict.as_str() {
        "pass" => Evidence::Current,
        "fail" => Evidence::Failed,
        _ => inconclusive("Kelpo verdict is inconclusive"),
    })
}

fn closure(root: &Path, current: &TekoCurrent, work_id: &str) -> Evidence {
    // Teko's packet prefixes work ids with `work_`; its status document does not.
    let raw_id = work_id.strip_prefix("work_").unwrap_or(work_id);
    if let Some(active) = &current.active {
        if active.work.id == raw_id {
            return Evidence::Pending;
        }
        return inconclusive(&format!(
            "Teko has different active work: {}",
            active.work.id
        ));
    }
    let log = root.join(".teko/witness.jsonl");
    if !log.exists() {
        return inconclusive("no active Teko work and no witness log");
    }
    if Command::new("sauma")
        .arg("verify")
        .arg(&log)
        .stdout(Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
    {
        return inconclusive("Teko witness log does not verify");
    }
    let Ok(text) = std::fs::read_to_string(&log) else {
        return inconclusive("Teko witness log is unreadable");
    };
    let mut outcome = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<WitnessRecord>(line) else {
            return inconclusive("Teko witness log has a malformed record");
        };
        if record.work != raw_id {
            continue;
        }
        outcome = Some(match record.record.as_str() {
            "teko-completion-witness.v1" => Evidence::Current,
            "teko-abandonment-witness.v1" => Evidence::Failed,
            _ => Evidence::Inconclusive,
        });
    }
    outcome.unwrap_or_else(|| inconclusive("Teko has no witness for this work"))
}

/// Seal through the neutral verifier so the snapshot wears the same envelope
/// as every other artifact in the chain, anchored to the point it describes.
fn stamp(body: &Value, anchor: &str, cycle_digest: &str) -> Result<Value> {
    let mut child = Command::new("sauma")
        .args(["stamp", "-", "--producer", "ohjaa", "--anchor", anchor])
        .args(["--artifact", cycle_digest, "--token", anchor])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("cannot run sauma stamp")?;
    use std::io::Write as _;
    child
        .stdin
        .take()
        .context("sauma stdin unavailable")?
        .write_all(body.to_string().as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(output.status.success(), "sauma stamp failed");
    serde_json::from_slice(&output.stdout).context("sauma stamp returned invalid JSON")
}
