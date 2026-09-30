//! The session log: what each agent session was briefed, read back, committed,
//! finished and handed over (faija-ontology.v2, the Agent's lifecycle).
//!
//! ```text
//! 1 entering → 2 oriented → 3 working, phases x+1 → 4 finished work → 5 leaving
//! ```
//!
//! Machine-local, in redb at `$XDG_STATE_HOME/ohjaa/sessions.redb`. Knowledge
//! never lives here — it stays files in git — so three rules hold:
//!
//! - deleting the log loses no knowledge, only this machine's history of it;
//! - a locked or broken log never blocks or fails a launch: it warns;
//! - a broken log is reported and left alone, never silently replaced.
//!
//! States are observed, not declared (law 3): phases are commits and their
//! sykli receipts, finished work is a pull request, leaving is the process
//! ending. The two things an agent writes — the read-back and the handover —
//! are kept, and shown, as its testimony.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use redb::{Database, DatabaseError, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::intake::rfc3339;

const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
/// Repository path → the id of its most recent session.
const LATEST: TableDefinition<&str, &str> = TableDefinition::new("latest");

/// The environment variable that names the session to the agent inside it.
pub const SESSION_ENV: &str = "OHJAA_SESSION";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivered {
    pub id: String,
    pub scope: String,
    pub as_of: String,
}

/// The agent's read-back: "I know the state".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readback {
    pub at_ms: i64,
    pub text: String,
}

/// The agent's testimony for whoever comes next.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfinished: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surprised: Option<String>,
}

/// A sykli receipt whose subject is a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub outcome: String,
    /// Whether the worktree had changes beyond the commit when sykli ran.
    pub dirty: bool,
}

/// One phase of work: a commit, and what sykli saw on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub commit: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finished {
    /// It could not be looked up (no `gh`, no network): not the same as none.
    Unknown { why: String },
    /// There is no pull request for the branch.
    NoPullRequest,
    PullRequest {
        url: String,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_evidence: Option<Evidence>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "how", rename_all = "snake_case")]
pub enum Exit {
    Code { code: i32 },
    Signal { signal: i32 },
    Unknown,
}

/// What leaving observed. Written by ohjaa, never by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    pub ended_ms: i64,
    pub exit: Exit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toimija_session: Option<String>,
    /// `None` when they could not be observed (no starting commit, git
    /// failed, or leaving was interrupted): never shown as "no commits".
    #[serde(default)]
    pub phases: Option<Vec<Phase>>,
    pub finished: Finished,
    /// A signal cut leaving short; the record holds what was known by then.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// What leaving observed in each repository the session named with
    /// `--scope`, beyond the one it was launched in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<ScopedLeft>,
}

/// A repository a session named with `--scope`, as it stood at entering. It
/// is observed like the one the session was launched in: its sessions counted
/// at launch, its commits and pull request at leaving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scoped {
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_at_start: Option<String>,
}

/// What leaving observed in one scoped repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedLeft {
    pub repo: String,
    /// `None` when they could not be observed, as for the launch repository.
    #[serde(default)]
    pub phases: Option<Vec<Phase>>,
    pub finished: Finished,
}

/// One session, entering to leaving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    /// The repository the session ran in; outside one, the folder.
    pub repo: String,
    /// The session ran in a folder that is not a Git repository: there were no
    /// commits or pull request to observe.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_repo: bool,
    pub intent: String,
    pub started_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_at_start: Option<String>,
    /// The repositories named with `--scope`, other than the launch one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped: Vec<Scoped>,
    pub delivered: Vec<Delivered>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback: Option<Readback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover: Option<Handover>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<Left>,
    /// Intake has drafted proposals from this session.
    #[serde(default)]
    pub drafted: bool,
    /// Source-reported snapshots, separately labelled from observed work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage: Vec<crate::usage::Receipt>,
}

/// Why the log could not be used. Never a reason to stop a launch.
#[derive(Debug)]
pub enum StoreError {
    /// Another ohjaa holds it; retried, then given up.
    Locked,
    /// It exists but cannot be read as a session log. Left untouched.
    Broken {
        path: PathBuf,
        why: String,
    },
    Other(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Locked => f.write_str("the session log is busy (another ohjaa holds it)"),
            StoreError::Broken { path, why } => write!(
                f,
                "the session log {} is unreadable ({why}); it was left untouched — move it \
                 aside to start a new one: mv '{}' '{}.broken'",
                path.display(),
                path.display(),
                path.display()
            ),
            StoreError::Other(why) => write!(f, "the session log failed: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

fn other(error: impl std::fmt::Display) -> StoreError {
    StoreError::Other(error.to_string())
}

pub fn store_path(state: &Path) -> PathBuf {
    state.join("sessions.redb")
}

pub struct Store {
    db: Database,
}

impl Store {
    /// One native session belongs to at most one wrapper session. Check and
    /// attach in one transaction so concurrent imports cannot double attribute it.
    pub fn attach_usage(&self, id: &str, receipt: crate::usage::Receipt) -> anyhow::Result<()> {
        receipt.validate()?;
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(SESSIONS)?;
            for row in table.iter()? {
                let (key, bytes) = row?;
                let session: Session = serde_json::from_slice(bytes.value())?;
                anyhow::ensure!(key.value() == id || !session.usage.iter().any(|r| r.agent == receipt.agent && r.session_id == receipt.session_id), "native session already attributed to {}", key.value());
            }
            let mut session: Session = {
                let bytes = table
                    .get(id)?
                    .ok_or_else(|| anyhow::anyhow!("session {id} is not in the log"))?;
                serde_json::from_slice(bytes.value())?
            };
            let agent = Path::new(&session.agent)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&session.agent);
            anyhow::ensure!(
                agent == receipt.agent,
                "receipt agent does not match session agent"
            );
            crate::usage::merge(&mut session.usage, receipt)?;
            let bytes = serde_json::to_vec(&session)?;
            table.insert(id, bytes.as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Open the log, creating it only where no file exists. An empty or
    /// unreadable file is reported, never replaced: redb would treat an empty
    /// file as a fresh database and the history would silently vanish.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let mut delay = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        loop {
            match Self::open_once(path) {
                Err(StoreError::Locked) if waited < Duration::from_millis(1500) => {
                    std::thread::sleep(delay);
                    waited += delay;
                    delay = (delay * 2).min(Duration::from_millis(200));
                }
                result => return result,
            }
        }
    }

    fn open_once(path: &Path) -> Result<Store, StoreError> {
        let broken = |why: String| StoreError::Broken {
            path: path.to_path_buf(),
            why,
        };
        let db = match std::fs::metadata(path) {
            Ok(meta) if meta.len() == 0 => return Err(broken("the file is empty".to_owned())),
            Ok(_) => Database::open(path).map_err(|error| match error {
                DatabaseError::DatabaseAlreadyOpen => StoreError::Locked,
                other => broken(other.to_string()),
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(other)?;
                }
                Database::create(path).map_err(|error| match error {
                    DatabaseError::DatabaseAlreadyOpen => StoreError::Locked,
                    error => other(error),
                })?
            }
            Err(error) => return Err(other(error)),
        };
        let store = Store { db };
        // Both tables exist from the first open, so a reader never has to
        // tell "empty" from "missing".
        let tx = store.db.begin_write().map_err(other)?;
        tx.open_table(SESSIONS).map_err(other)?;
        tx.open_table(LATEST).map_err(other)?;
        tx.commit().map_err(other)?;
        Ok(store)
    }

    pub fn put(&self, session: &Session) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(session).map_err(other)?;
        let tx = self.db.begin_write().map_err(other)?;
        {
            let mut sessions = tx.open_table(SESSIONS).map_err(other)?;
            sessions
                .insert(session.id.as_str(), bytes.as_slice())
                .map_err(other)?;
        }
        note_latest(&tx, session)?;
        tx.commit().map_err(other)
    }

    pub fn get(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let tx = self.db.begin_read().map_err(other)?;
        let sessions = tx.open_table(SESSIONS).map_err(other)?;
        let Some(value) = sessions.get(id).map_err(other)? else {
            return Ok(None);
        };
        serde_json::from_slice(value.value())
            .map(Some)
            .map_err(|error| other(format!("session {id} is unreadable: {error}")))
    }

    /// Read, change and write back one session in a single transaction.
    pub fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut Session),
    ) -> Result<Option<Session>, StoreError> {
        let tx = self.db.begin_write().map_err(other)?;
        let updated = {
            let mut sessions = tx.open_table(SESSIONS).map_err(other)?;
            let current = match sessions.get(id).map_err(other)? {
                Some(value) => serde_json::from_slice::<Session>(value.value()).map_err(other)?,
                None => return Ok(None),
            };
            let mut session = current;
            change(&mut session);
            let bytes = serde_json::to_vec(&session).map_err(other)?;
            sessions.insert(id, bytes.as_slice()).map_err(other)?;
            drop(sessions);
            note_latest(&tx, &session)?;
            session
        };
        tx.commit().map_err(other)?;
        Ok(Some(updated))
    }

    pub fn latest_for(&self, repo: &str) -> Result<Option<Session>, StoreError> {
        let id = {
            let tx = self.db.begin_read().map_err(other)?;
            let latest = tx.open_table(LATEST).map_err(other)?;
            match latest.get(repo).map_err(other)? {
                Some(id) => id.value().to_owned(),
                None => return Ok(None),
            }
        };
        self.get(&id)
    }

    /// Every session, oldest first. Unreadable entries are skipped with their
    /// id reported, so one bad record cannot hide the rest.
    pub fn all(&self) -> Result<(Vec<Session>, Vec<String>), StoreError> {
        let tx = self.db.begin_read().map_err(other)?;
        let sessions = tx.open_table(SESSIONS).map_err(other)?;
        let mut out = Vec::new();
        let mut unreadable = Vec::new();
        for entry in sessions.iter().map_err(other)? {
            let (id, value) = entry.map_err(other)?;
            match serde_json::from_slice::<Session>(value.value()) {
                Ok(session) => out.push(session),
                Err(_) => unreadable.push(id.value().to_owned()),
            }
        }
        out.sort_by_key(|session| session.started_ms);
        Ok((out, unreadable))
    }
}

/// Keep `LATEST` pointing at each repository's latest *ended* session. A
/// session still running is never "the last session here": the perimeter
/// already shows who is live, and an unfinished record must not read as a
/// crash. "Here" is where a session was launched and every repository it
/// named with `--scope`: the one that ended last wins, wherever it started.
fn note_latest(tx: &redb::WriteTransaction, session: &Session) -> Result<(), StoreError> {
    let Some(left) = &session.left else {
        return Ok(());
    };
    let sessions = tx.open_table(SESSIONS).map_err(other)?;
    let mut latest = tx.open_table(LATEST).map_err(other)?;
    let heres = std::iter::once(session.repo.as_str())
        .chain(session.scoped.iter().map(|scoped| scoped.repo.as_str()));
    for here in heres {
        let newer = match latest.get(here).map_err(other)? {
            None => true,
            Some(id) if id.value() == session.id => true,
            Some(id) => {
                let current = sessions
                    .get(id.value())
                    .map_err(other)?
                    .and_then(|value| serde_json::from_slice::<Session>(value.value()).ok());
                current
                    .and_then(|current| current.left.map(|l| l.ended_ms))
                    .is_none_or(|ended| left.ended_ms >= ended)
            }
        };
        if newer {
            latest.insert(here, session.id.as_str()).map_err(other)?;
        }
    }
    Ok(())
}

/// A fresh session id: when it started, and which ohjaa started it.
pub fn new_id(started_ms: i64) -> String {
    format!("oh-{started_ms}-{}", std::process::id())
}

/// Session ids are names, never paths.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

// ---- rendering --------------------------------------------------------------

/// A time for people. Always UTC, and always says so: a bare 23:53 reads as
/// local time to anyone east or west of Greenwich.
fn clock(ms: i64) -> String {
    let full = rfc3339(ms.div_euclid(1000));
    // "2026-09-26T10:02:03Z" → "2026-09-26 10:02 UTC"
    format!("{} {} UTC", &full[..10], &full[11..16])
}

fn describe_exit(exit: Exit) -> String {
    match exit {
        Exit::Code { code: 0 } => "clean (exit 0)".to_owned(),
        Exit::Code { code } => format!("exit {code}"),
        Exit::Signal { signal } => format!("killed by signal {signal}"),
        Exit::Unknown => "unknown".to_owned(),
    }
}

fn describe_evidence(evidence: Option<&Evidence>) -> String {
    match evidence {
        Some(e) if e.dirty => format!("{} (dirty tree)", e.outcome),
        Some(e) => e.outcome.clone(),
        None => "no sykli run".to_owned(),
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn repo_name(repo: &str) -> &str {
    Path::new(repo)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(repo)
}

fn handover_lines(handover: Option<&Handover>, out: &mut String) {
    match handover {
        None => out.push_str("none written\n"),
        Some(h) => {
            let mut any = false;
            for (label, value) in [
                ("unfinished  ", &h.unfinished),
                ("next agent  ", &h.next),
                ("surprised by", &h.surprised),
            ] {
                if let Some(value) = value {
                    let _ = writeln!(out, "{label}  {value}");
                    any = true;
                }
            }
            if !any {
                out.push_str("written, but empty\n");
            }
        }
    }
}

/// The leaving record, as printed at exit: observed first, then testimony.
pub fn render_record(session: &Session) -> String {
    let mut out = String::new();
    let ended = session.left.as_ref().map_or_else(
        || "still open".to_owned(),
        |left| {
            format!(
                "{} · ended: {}",
                clock(left.ended_ms),
                describe_exit(left.exit)
            )
        },
    );
    let _ = writeln!(
        out,
        "session {} · {} · {} · {} → {}",
        session.id,
        session.agent,
        repo_name(&session.repo),
        clock(session.started_ms),
        ended
    );
    out.push_str("─ observed (written by ohjaa; the agent cannot change it) ─\n");
    let _ = writeln!(out, "intent        {}", session.intent);
    let readback = match &session.readback {
        Some(r) => format!("read back at {}", &clock(r.at_ms)[11..]),
        None => "no read-back".to_owned(),
    };
    let _ = writeln!(
        out,
        "knew          {} knowledge node(s) delivered · {readback}",
        session.delivered.len()
    );
    if let Some(left) = &session.left {
        if !session.outside_repo {
            let _ = writeln!(
                out,
                "phases        {}",
                describe_phases(left.phases.as_ref(), left.interrupted)
            );
            let _ = writeln!(out, "finished      {}", describe_finished(&left.finished));
        } else if left.scoped.is_empty() {
            // The folder has nothing to observe, and no --scope named a repository.
            out.push_str("phases        none to observe (not a Git repository)\n");
            out.push_str("finished      none to observe (not a Git repository)\n");
        }
        for scoped in &left.scoped {
            let label = format!("in {}", repo_name(&scoped.repo));
            let _ = writeln!(
                out,
                "{label:<13} phases {}",
                describe_phases(scoped.phases.as_ref(), left.interrupted)
            );
            let _ = writeln!(
                out,
                "{label:<13} finished {}",
                describe_finished(&scoped.finished)
            );
        }
        if let Some(toimija) = &left.toimija_session {
            let _ = writeln!(out, "toimija       {toimija}");
        }
    }
    out.push_str("─ usage (imported source assertions; estimates are not bills) ─\n");
    if session.usage.is_empty() {
        out.push_str("not collected; unknown, not zero\n");
    }
    for receipt in &session.usage {
        let _ = writeln!(
            out,
            "{} {} · {} · {} model row(s) · source {}",
            receipt.agent,
            receipt.session_id,
            if receipt.complete {
                "complete source snapshot"
            } else {
                "partial source snapshot"
            },
            receipt.entries.len(),
            receipt.source
        );
        for entry in &receipt.entries {
            let number = |n: Option<u64>| n.map_or_else(|| "unknown".to_owned(), |n| n.to_string());
            let _ = writeln!(
                out,
                "  {}/{} input={} output={} cache-read={} cache-write={} estimate-usd-micros={}",
                entry.provider,
                entry.model,
                number(entry.input_tokens),
                number(entry.output_tokens),
                number(entry.cache_read_tokens),
                number(entry.cache_write_tokens),
                number(entry.estimated_usd_micros)
            );
        }
    }
    out.push_str("─ handover (the agent's own words) ─\n");
    handover_lines(session.handover.as_ref(), &mut out);
    out
}

/// What `ohjaa session` says when no session here has ended: never "none",
/// while records exist that have not ended or could not be read.
pub fn render_none_ended(unended: &[Session], unreadable: usize) -> String {
    let mut out = String::new();
    if unended.is_empty() {
        out.push_str("no session recorded here\n");
    } else {
        out.push_str("no session here has ended yet\n");
        out.push_str("not ended (still running, or its leaving was never recorded):\n");
        for session in unended {
            let _ = writeln!(
                out,
                "  {} · {} · started {} · {}",
                session.id,
                session.agent,
                clock(session.started_ms),
                session.intent
            );
        }
        out.push_str("see one with `ohjaa session <id>`\n");
    }
    if unreadable > 0 {
        let _ = writeln!(
            out,
            "{unreadable} record(s) in the log could not be read; they may be from here"
        );
    }
    out
}

fn describe_phases(phases: Option<&Vec<Phase>>, interrupted: bool) -> String {
    match phases {
        None if interrupted => "not observed (leaving was interrupted)".to_owned(),
        None => "not observed".to_owned(),
        Some(phases) if phases.is_empty() => "no commits".to_owned(),
        Some(phases) => phases
            .iter()
            .map(|p| {
                format!(
                    "{} {}",
                    short(&p.commit),
                    describe_evidence(p.evidence.as_ref())
                )
            })
            .collect::<Vec<_>>()
            .join(" · "),
    }
}

fn describe_finished(finished: &Finished) -> String {
    match finished {
        Finished::Unknown { why } => format!("unknown ({why})"),
        Finished::NoPullRequest => "no pull request".to_owned(),
        Finished::PullRequest {
            url,
            state,
            head_evidence,
        } => format!(
            "{url} ({}) · evidence on its head: {}",
            state.to_lowercase(),
            describe_evidence(head_evidence.as_ref())
        ),
    }
}

fn summarise_work(phases: Option<&Vec<Phase>>) -> String {
    match phases {
        Some(phases) => {
            let with_evidence = phases
                .iter()
                .filter(|p| p.evidence.as_ref().is_some_and(|e| e.outcome == "passed"))
                .count();
            format!(
                "{} commit(s), {with_evidence} with a passing sykli run",
                phases.len()
            )
        }
        None => "commits not observed".to_owned(),
    }
}

fn summarise_finished(finished: &Finished) -> String {
    match finished {
        Finished::PullRequest { url, state, .. } => format!("{url} ({})", state.to_lowercase()),
        Finished::NoPullRequest => "no pull request".to_owned(),
        Finished::Unknown { .. } => "unknown".to_owned(),
    }
}

/// "Last session here", for the next session's entering briefing in `here`
/// (a `LATEST` key). A session launched elsewhere that named `here` with
/// `--scope` says where it was launched, so it never reads as launched here.
pub fn render_last(session: &Session, here: &str) -> String {
    let mut out = String::new();
    // Only ended sessions are "the last session here" (see `note_latest`);
    // a record without its leaving is said as exactly that, never as a crash.
    let ended = session.left.as_ref().map_or_else(
        || "leaving not recorded".to_owned(),
        |left| {
            format!(
                "ended {}, {}",
                clock(left.ended_ms),
                describe_exit(left.exit)
            )
        },
    );
    let launched = if session.repo == here {
        String::new()
    } else {
        format!(
            "; launched from {}, it named this repository with --scope",
            session.repo
        )
    };
    let _ = writeln!(
        out,
        "Last session here ({}, {}, {ended}{launched}):",
        session.id, session.agent
    );
    let _ = writeln!(out, "- intent: {}", session.intent);
    let scoped = session
        .left
        .as_ref()
        .map_or(&[][..], |left| &left.scoped[..]);
    if session.outside_repo && scoped.is_empty() {
        out.push_str(
            "- work: not a Git repository, so no commits or pull request to observe; \
             its handover says what changed\n",
        );
    } else if let Some(left) = &session.left {
        if !session.outside_repo {
            let _ = writeln!(
                out,
                "- work: {}; finished: {}",
                summarise_work(left.phases.as_ref()),
                summarise_finished(&left.finished)
            );
        }
        for scoped in scoped {
            let _ = writeln!(
                out,
                "- work in {}: {}; finished: {}",
                repo_name(&scoped.repo),
                summarise_work(scoped.phases.as_ref()),
                summarise_finished(&scoped.finished)
            );
        }
    }
    match &session.handover {
        None => out.push_str("- it left no handover\n"),
        Some(h) => {
            if let Some(v) = &h.unfinished {
                let _ = writeln!(out, "- unfinished: {v}");
            }
            if let Some(v) = &h.next {
                let _ = writeln!(out, "- for the next agent: {v}");
            }
            if let Some(v) = &h.surprised {
                let _ = writeln!(out, "- it was surprised by: {v}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ohjaa-sessions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session(id: &str, repo: &str, started_ms: i64) -> Session {
        Session {
            id: id.to_owned(),
            agent: "claude".to_owned(),
            repo: repo.to_owned(),
            outside_repo: false,
            intent: "retry uploads".to_owned(),
            started_ms,
            branch: Some("main".to_owned()),
            head_at_start: None,
            scoped: Vec::new(),
            delivered: vec![Delivered {
                id: "rust-only".to_owned(),
                scope: "ours".to_owned(),
                as_of: "2026-09-24".to_owned(),
            }],
            readback: None,
            handover: None,
            left: None,
            drafted: false,
            usage: Vec::new(),
        }
    }

    fn ended(mut session: Session, ended_ms: i64) -> Session {
        session.left = Some(Left {
            ended_ms,
            exit: Exit::Code { code: 0 },
            toimija_session: None,
            phases: Some(Vec::new()),
            finished: Finished::NoPullRequest,
            interrupted: false,
            scoped: Vec::new(),
        });
        session
    }

    #[test]
    fn a_root_session_is_recorded_repository_by_repository() {
        let mut s = session("oh-7", "/r/projects", 1_790_000_000_000);
        s.outside_repo = true;
        s.left = Some(Left {
            ended_ms: 1_790_003_600_000,
            exit: Exit::Code { code: 0 },
            toimija_session: None,
            phases: None,
            finished: Finished::Unknown {
                why: "not a Git repository".to_owned(),
            },
            interrupted: false,
            scoped: vec![
                ScopedLeft {
                    repo: "/r/projects/ohjaa".to_owned(),
                    phases: Some(vec![Phase {
                        commit: "a1b2c3d4e5".to_owned(),
                        subject: "fix".to_owned(),
                        evidence: Some(Evidence {
                            outcome: "passed".to_owned(),
                            dirty: false,
                        }),
                    }]),
                    finished: Finished::NoPullRequest,
                },
                ScopedLeft {
                    repo: "/r/projects/toimija".to_owned(),
                    phases: Some(Vec::new()),
                    finished: Finished::NoPullRequest,
                },
            ],
        });
        let record = render_record(&s);
        assert!(
            !record.contains("none to observe"),
            "a root session with scoped repositories has work to show: {record}"
        );
        assert!(
            record.contains("in ohjaa      phases a1b2c3d passed"),
            "{record}"
        );
        assert!(
            record.contains("in toimija    phases no commits"),
            "{record}"
        );
        assert!(
            record.contains("in toimija    finished no pull request"),
            "{record}"
        );
        let last = render_last(&s, &s.repo);
        assert!(
            last.contains("- work in ohjaa: 1 commit(s), 1 with a passing sykli run"),
            "{last}"
        );
        assert!(!last.contains("not a Git repository"), "{last}");
        assert!(!last.contains("launched from"), "{last}");
        // Briefed inside a repository it named, it says where it was launched.
        let there = render_last(&s, "/r/projects/ohjaa");
        assert!(
            there.contains("; launched from /r/projects, it named this repository with --scope):"),
            "{there}"
        );
    }

    fn named(mut session: Session, repos: &[&str]) -> Session {
        session.scoped = repos
            .iter()
            .map(|repo| Scoped {
                repo: (*repo).to_owned(),
                branch: None,
                head_at_start: None,
            })
            .collect();
        session
    }

    #[test]
    fn a_session_is_the_last_one_in_every_repository_it_named() {
        let dir = temp("latestnamed");
        let store = Store::open(&store_path(&dir)).unwrap();
        store
            .put(&ended(session("oh-1", "/r/a", 1_000), 1_500))
            .unwrap();
        store
            .put(&ended(session("oh-2", "/r/b", 1_000), 3_000))
            .unwrap();
        // From the root, naming a and b; it ends after oh-1, before oh-2.
        let root = named(session("oh-3", "/r", 2_000), &["/r/a", "/r/b"]);
        store.put(&root).unwrap();
        assert_eq!(
            store.latest_for("/r/a").unwrap().unwrap().id,
            "oh-1",
            "a session still running is never the last one, wherever it was named"
        );
        store
            .update("oh-3", |s| *s = ended(s.clone(), 2_500))
            .unwrap();
        assert_eq!(store.latest_for("/r").unwrap().unwrap().id, "oh-3");
        assert_eq!(store.latest_for("/r/a").unwrap().unwrap().id, "oh-3");
        assert_eq!(
            store.latest_for("/r/b").unwrap().unwrap().id,
            "oh-2",
            "the one that ended last wins, wherever it was launched"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_last_session_here_is_the_latest_ended_never_a_live_one() {
        let dir = temp("latestended");
        let store = Store::open(&store_path(&dir)).unwrap();
        store
            .put(&ended(session("oh-1", "/r/a", 1_000), 1_500))
            .unwrap();
        // Entering records a live session: it is not "the last session here".
        store.put(&session("oh-2", "/r/a", 2_000)).unwrap();
        assert_eq!(store.latest_for("/r/a").unwrap().unwrap().id, "oh-1");
        // Two overlapping sessions: the one that ended last is the latest.
        store.put(&session("oh-3", "/r/a", 3_000)).unwrap();
        store
            .update("oh-3", |s| *s = ended(s.clone(), 3_500))
            .unwrap();
        store
            .update("oh-2", |s| *s = ended(s.clone(), 3_200))
            .unwrap();
        assert_eq!(store.latest_for("/r/a").unwrap().unwrap().id, "oh-3");
        store
            .update("oh-2", |s| *s = ended(s.clone(), 4_000))
            .unwrap();
        assert_eq!(store.latest_for("/r/a").unwrap().unwrap().id, "oh-2");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sessions_round_trip_and_the_latest_per_repo_is_found() {
        let dir = temp("roundtrip");
        let store = Store::open(&store_path(&dir)).unwrap();
        store
            .put(&ended(session("oh-1", "/r/a", 1_000), 1_500))
            .unwrap();
        store
            .put(&ended(session("oh-2", "/r/b", 2_000), 2_500))
            .unwrap();
        store
            .put(&ended(session("oh-3", "/r/a", 3_000), 3_500))
            .unwrap();
        assert_eq!(store.latest_for("/r/a").unwrap().unwrap().id, "oh-3");
        assert_eq!(store.latest_for("/r/b").unwrap().unwrap().id, "oh-2");
        assert!(store.latest_for("/r/c").unwrap().is_none());
        let updated = store
            .update("oh-2", |s| {
                s.readback = Some(Readback {
                    at_ms: 2_500,
                    text: "I know".to_owned(),
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(store.get("oh-2").unwrap().unwrap(), updated);
        assert!(store.update("nope", |_| {}).unwrap().is_none());
        let (all, unreadable) = store.all().unwrap();
        assert_eq!(
            all.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["oh-1", "oh-2", "oh-3"]
        );
        assert!(unreadable.is_empty());
        drop(store);
        // And it survives reopening: this is the whole point of a log.
        let store = Store::open(&store_path(&dir)).unwrap();
        assert_eq!(store.all().unwrap().0.len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_file_is_reported_and_never_replaced() {
        let dir = temp("empty");
        let path = store_path(&dir);
        std::fs::write(&path, b"").unwrap();
        let error = Store::open(&path)
            .err()
            .expect("an empty log must be refused");
        assert!(matches!(error, StoreError::Broken { .. }), "{error}");
        assert!(error.to_string().contains("mv "), "{error}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "left untouched");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupt_file_is_reported_and_left_untouched() {
        let dir = temp("corrupt");
        let path = store_path(&dir);
        std::fs::write(&path, b"this is not a redb database at all, not even close").unwrap();
        let before = std::fs::read(&path).unwrap();
        let error = Store::open(&path)
            .err()
            .expect("a corrupt log must be refused");
        assert!(matches!(error, StoreError::Broken { .. }), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_held_log_is_reported_as_locked_not_broken() {
        let dir = temp("locked");
        let path = store_path(&dir);
        let holder = Store::open(&path).unwrap();
        let started = std::time::Instant::now();
        let error = Store::open(&path)
            .err()
            .expect("a held log must not open twice");
        assert!(matches!(error, StoreError::Locked), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "retries are bounded"
        );
        drop(holder);
        assert!(Store::open(&path).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_record_separates_what_was_observed_from_what_the_agent_said() {
        let mut s = session("oh-9", "/r/selko", 1_790_000_000_000);
        s.readback = Some(Readback {
            at_ms: 1_790_000_060_000,
            text: "goal".to_owned(),
        });
        s.left = Some(Left {
            ended_ms: 1_790_003_600_000,
            exit: Exit::Code { code: 0 },
            toimija_session: Some("1790000000-42".to_owned()),
            phases: Some(vec![
                Phase {
                    commit: "a1b2c3d4e5".to_owned(),
                    subject: "one".to_owned(),
                    evidence: Some(Evidence {
                        outcome: "passed".to_owned(),
                        dirty: false,
                    }),
                },
                Phase {
                    commit: "0718abcdef".to_owned(),
                    subject: "two".to_owned(),
                    evidence: None,
                },
            ]),
            finished: Finished::NoPullRequest,
            interrupted: false,
            scoped: Vec::new(),
        });
        let record = render_record(&s);
        let observed = record.find("─ observed").unwrap();
        let testimony = record.find("─ handover").unwrap();
        assert!(observed < testimony, "{record}");
        assert!(record.contains("ended: clean (exit 0)"), "{record}");
        assert!(
            record.contains("a1b2c3d passed · 0718abc no sykli run"),
            "{record}"
        );
        assert!(
            record.contains("1 knowledge node(s) delivered · read back at 14:14 UTC"),
            "{record}"
        );
        assert!(
            record.contains(" · 2026-09-21 14:13 UTC → 2026-09-21 15:13 UTC · ended"),
            "every time says its zone: {record}"
        );
        assert!(
            record.contains("none written"),
            "a missing handover is said: {record}"
        );

        s.handover = Some(Handover {
            at_ms: 1,
            unfinished: Some("windows paths".to_owned()),
            next: None,
            surprised: Some("the fixture skips".to_owned()),
        });
        let last = render_last(&s, &s.repo);
        assert!(last.contains("Last session here (oh-9, claude"), "{last}");
        assert!(
            last.contains("ended 2026-09-21 15:13 UTC, clean"),
            "said once, not twice: {last}"
        );
        assert!(
            last.contains("2 commit(s), 1 with a passing sykli run"),
            "{last}"
        );
        assert!(last.contains("- unfinished: windows paths"), "{last}");
        assert!(
            last.contains("- it was surprised by: the fixture skips"),
            "{last}"
        );
    }

    #[test]
    fn session_ids_are_names_not_paths() {
        assert!(is_valid_id(&new_id(1_790_000_000_000)));
        for bad in ["", "../x", "a/b", "a b", ".", &"x".repeat(65)] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }
}
