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
    pub phases: Vec<Phase>,
    pub finished: Finished,
}

/// One session, entering to leaving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub repo: String,
    pub intent: String,
    pub started_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_at_start: Option<String>,
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
            let mut latest = tx.open_table(LATEST).map_err(other)?;
            latest
                .insert(session.repo.as_str(), session.id.as_str())
                .map_err(other)?;
        }
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

fn clock(ms: i64) -> String {
    let full = rfc3339(ms.div_euclid(1000));
    // "2026-09-26T10:02:03Z" → "2026-09-26 10:02"
    format!("{} {}", &full[..10], &full[11..16])
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
        if left.phases.is_empty() {
            out.push_str("phases        no commits\n");
        } else {
            let phases: Vec<String> = left
                .phases
                .iter()
                .map(|p| {
                    format!(
                        "{} {}",
                        short(&p.commit),
                        describe_evidence(p.evidence.as_ref())
                    )
                })
                .collect();
            let _ = writeln!(out, "phases        {}", phases.join(" · "));
        }
        let finished = match &left.finished {
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
        };
        let _ = writeln!(out, "finished      {finished}");
        if let Some(toimija) = &left.toimija_session {
            let _ = writeln!(out, "toimija       {toimija}");
        }
    }
    out.push_str("─ handover (the agent's own words) ─\n");
    handover_lines(session.handover.as_ref(), &mut out);
    out
}

/// "Last session here", for the next session's entering briefing.
pub fn render_last(session: &Session) -> String {
    let mut out = String::new();
    let ended = session.left.as_ref().map_or_else(
        || "did not record its leaving".to_owned(),
        |left| {
            format!(
                "ended {} UTC, {}",
                clock(left.ended_ms),
                describe_exit(left.exit)
            )
        },
    );
    let _ = writeln!(
        out,
        "Last session here ({}, {}, {ended}):",
        session.id, session.agent
    );
    let _ = writeln!(out, "- intent: {}", session.intent);
    if let Some(left) = &session.left {
        let with_evidence = left
            .phases
            .iter()
            .filter(|p| p.evidence.as_ref().is_some_and(|e| e.outcome == "passed"))
            .count();
        let finished = match &left.finished {
            Finished::PullRequest { url, state, .. } => {
                format!("{url} ({})", state.to_lowercase())
            }
            Finished::NoPullRequest => "no pull request".to_owned(),
            Finished::Unknown { .. } => "unknown".to_owned(),
        };
        let _ = writeln!(
            out,
            "- work: {} commit(s), {with_evidence} with a passing sykli run; finished: {finished}",
            left.phases.len()
        );
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
            intent: "retry uploads".to_owned(),
            started_ms,
            branch: Some("main".to_owned()),
            head_at_start: None,
            delivered: vec![Delivered {
                id: "rust-only".to_owned(),
                scope: "ours".to_owned(),
                as_of: "2026-09-24".to_owned(),
            }],
            readback: None,
            handover: None,
            left: None,
            drafted: false,
        }
    }

    #[test]
    fn sessions_round_trip_and_the_latest_per_repo_is_found() {
        let dir = temp("roundtrip");
        let store = Store::open(&store_path(&dir)).unwrap();
        store.put(&session("oh-1", "/r/a", 1_000)).unwrap();
        store.put(&session("oh-2", "/r/b", 2_000)).unwrap();
        store.put(&session("oh-3", "/r/a", 3_000)).unwrap();
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
            phases: vec![
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
            ],
            finished: Finished::NoPullRequest,
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
            record.contains("1 knowledge node(s) delivered · read back at"),
            "{record}"
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
        let last = render_last(&s);
        assert!(last.contains("Last session here (oh-9, claude"), "{last}");
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
