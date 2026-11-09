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
//!
//! A log an ohjaa with `ohjaa judge` wrote also holds that command's two
//! tables (`judgements`, `judgement_responses`). This ohjaa never opens them:
//! such a log is read and written as any other, and the tables stay as they
//! were.

use std::path::{Path, PathBuf};
use std::time::Duration;

use redb::{
    Database, DatabaseError, ReadOnlyDatabase, ReadTransaction, ReadableDatabase, ReadableTable,
    TableDefinition, TableError,
};
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
    /// Repository identity for repo-scoped knowledge; older records omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The agent's own instruction file that held it at launch, current, so
    /// that the briefing left it out; absent when the briefing carried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub outcome: String,
    /// Whether the worktree had changes beyond the commit when sykli ran.
    pub dirty: bool,
    /// How many tasks the receipt records; older records omit it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tasks: u32,
    /// How many of them sykli reused from its cache instead of running:
    /// said as cached, never counted as passed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One phase of work: a commit, and what sykli saw on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Phase {
    pub commit: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// The branch it was made on, or that a push from here named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit's tree, which a sykli receipt names; older records omit it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tree: String,
    /// The checkout it was made in, when that is not the repository the
    /// record is about (a worktree of it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// No reference or reflog names it any more (its branch, worktree and
    /// remote-tracking branch were all deleted): found in the object store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreachable: bool,
}

/// What `sykli verify --commit` said about one commit. A receipt's own
/// `outcome` is its claim; only this says passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Check {
    /// sykli verified a passing, clean receipt bound to exactly this commit.
    Passed,
    /// sykli refused it, in its own words.
    Failed { why: String },
    /// No sykli receipt names the commit's tree.
    Unchecked,
    /// No sykli receipt names the commit's tree, and the checkout it was
    /// made in is gone, its receipts with it: what cannot be observed is
    /// recorded, never asserted (law 3).
    UncheckedGone,
    /// sykli could not be asked: it has no `--commit`, is not installed, or
    /// gave no answer in time; or the commit has no sykli contract.
    Unverified { why: String },
    /// sykli refused on the checkout it ran in, not on the commit: a sykli
    /// that still compares the tree on disk, in a checkout that has moved on.
    Stale { why: String },
    /// A receipt of its tree passed, but under another contract or lock than
    /// the one it is judged by now (sykli verify exit 4): the gate was edited
    /// since, or the run used another contract. Not proven, and not failed.
    OtherContract { why: String },
}

/// A commit that reached the default branch during the session: one this
/// session made that is on it now, or one it pushed to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landed {
    pub commit: String,
    pub subject: String,
    /// The branch it reached: `origin/main`, or the local `main`.
    pub onto: String,
    /// What sykli said: asked only of the tip of each push (what a pre-push
    /// Boundary gates); the commits before it are their receipts' word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<Check>,
    /// Its receipt, as the receipt says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// The branch still sat at it when the session left.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub at_tip: bool,
    /// It cannot be shown to be this session's: observed, never flagged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattributed: bool,
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
        /// The receipt for its head's tree, as the receipt says it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_evidence: Option<Evidence>,
        /// Its head commit; older records omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<String>,
        /// What sykli said about its head; older records omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_check: Option<Check>,
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
    /// The commits that reached the default branch of the launch repository
    /// during the session, each with what sykli said about it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landed: Vec<Landed>,
    /// Commits made in the launch repository during the session that cannot
    /// be shown to be its own (law 3): observed, never credited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<Phase>,
    /// The other sessions that shared its places while it ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<String>,
    /// Its ohjaa was gone before leaving could run (killed, crashed, or the
    /// machine restarted: only its absence is observed): the session was
    /// closed at a later launch that found it gone. `ended_ms` is when that
    /// was observed, not when it ended, and nothing it did was observed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orphaned: bool,
    /// The agent's own sessions it ran, found in the agent's transcripts at
    /// leaving, or at the launch that closed it as orphaned (D7). Older
    /// records have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<crate::usage::Native>,
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
    /// Not named with `--scope`: found at leaving, a repository directly in
    /// the launch folder (or in a folder `--scope` named) whose branches or
    /// checkouts moved while the session ran. Another agent may have moved
    /// them, so it is said as found, never as named.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub found: bool,
    /// As for the launch repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landed: Vec<Landed>,
    /// As for the launch repository; in a found one, every commit made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed: Vec<Phase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<String>,
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
    /// The Intent this session works on. It is separate from the session's
    /// own id: minted when a session is given `--intent`, and carried by each
    /// session that continues it with `--intent-id`. Older records have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    /// What it was asked to do (`--task`), as given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The second it was launched in: its id and every time window are
    /// built on whole seconds.
    pub started_ms: i64,
    /// The launch's own instant, in milliseconds. Only the agent's own
    /// records, which carry milliseconds, are compared with it: a transcript
    /// begun in the second of the launch but before it was resumed by it.
    /// Older records have none ([`Session::launch_instant_ms`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_ms: Option<i64>,
    /// The process group the session runs in (`toimija run` leads it, and
    /// the agent runs in it). Whether the group is alive says whether the
    /// session is, even after the ohjaa that started it is gone. Recorded
    /// just after the spawn; older records have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// When the group was started: a group of that id led by a process that
    /// started later is another one, the id reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_started_ms: Option<i64>,
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
    /// Why the receipt the launched command wrote at exit was refused: a
    /// refusal is said, never shown as "not collected".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_refused: Option<String>,
    /// The agent's own sessions its environment named when it read back or
    /// handed over from inside this one, first named first: several after a
    /// `/clear`. Leaving adds them to `left.native`. Older records have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_env: Vec<crate::usage::FromEnv>,
}

impl Session {
    /// When it ended, as far as that was observed, to rank the sessions in a
    /// place: `None` while it runs. An orphan's end was never observed, only
    /// that its ohjaa was gone at some later launch, so it ranks by its start:
    /// a session that really ended after that comes after it.
    pub fn ended_rank(&self) -> Option<i64> {
        self.left.as_ref().map(|left| {
            if left.orphaned {
                self.started_ms
            } else {
                left.ended_ms
            }
        })
    }
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
    /// attach in one transaction so concurrent imports cannot double
    /// attribute it. A row that cannot be read is skipped in that check, never
    /// fatal to every import: the ids skipped are returned, so the caller can
    /// name them. The session attached to must itself be readable.
    pub fn attach_usage(
        &self,
        id: &str,
        receipt: crate::usage::Receipt,
    ) -> anyhow::Result<Vec<String>> {
        receipt.validate()?;
        let tx = self.db.begin_write()?;
        let mut skipped = Vec::new();
        {
            let mut table = tx.open_table(SESSIONS)?;
            for row in table.iter()? {
                let (key, bytes) = row?;
                if key.value() == id {
                    continue;
                }
                let Ok(other) = serde_json::from_slice::<Session>(bytes.value()) else {
                    skipped.push(key.value().to_owned());
                    continue;
                };
                let claimed = other
                    .usage
                    .iter()
                    .any(|r| r.agent == receipt.agent && r.session_id == receipt.session_id);
                anyhow::ensure!(
                    !claimed,
                    "native session already attributed to {}",
                    key.value()
                );
            }
            let mut session: Session = {
                let bytes = table
                    .get(id)?
                    .ok_or_else(|| anyhow::anyhow!("session {id} is not in the log"))?;
                serde_json::from_slice(bytes.value())
                    .map_err(|error| anyhow::anyhow!("session {id} is unreadable: {error}"))?
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
        Ok(skipped)
    }
    /// Open the log, creating it only where no file exists. An empty or
    /// unreadable file is reported, never replaced: redb would treat an empty
    /// file as a fresh database and the history would silently vanish.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        remove_left_beside(&made_at(path));
        retrying(|| Self::open_once(path))
    }

    fn open_once(path: &Path) -> Result<Store, StoreError> {
        let broken = |why: String| StoreError::Broken {
            path: path.to_path_buf(),
            why,
        };
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() == 0 => return Err(broken("the file is empty".to_owned())),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(other)?;
                }
                match create_whole(&made_at(path)) {
                    // Made here, or by another ohjaa first: opened as made.
                    Ok(Made::Whole) => {}
                    Ok(Made::InPlace(why)) => said_in_place(path, &why),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(other(error)),
                }
            }
            Err(error) => return Err(other(error)),
        }
        let db = Database::open(path).map_err(|error| match error {
            DatabaseError::DatabaseAlreadyOpen => StoreError::Locked,
            other => broken(other.to_string()),
        })?;
        let store = Store { db };
        // Every table exists from the first writable open, so a reader of a
        // log this ohjaa wrote never has to tell "empty" from "missing". A
        // log an older ohjaa wrote gains the newer tables here; its sessions
        // are read as they were.
        let tx = store.db.begin_write().map_err(other)?;
        tx.open_table(SESSIONS).map_err(other)?;
        tx.open_table(LATEST).map_err(other)?;
        tx.commit().map_err(other)?;
        Ok(store)
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
}

/// The log opened only to read it, for the commands that only read it. A
/// writable open changes the file even when no session is written (redb's
/// own bookkeeping as it opens and closes: its time and size move), so a
/// reader never makes one on the log: it opens it read-only, and never
/// creates it — where none exists, no session was recorded. Writers use
/// [`Store`].
pub struct Reader {
    db: Option<Box<dyn ReadableDatabase>>,
    /// The private copy read instead of a log that needs a repair, kept only
    /// to be removed with the reader. After `db`, so the copy is closed
    /// before it is removed.
    _copy: Option<PrivateCopy>,
}

impl Reader {
    /// Retried briefly while a writer holds the log, then said to be busy: a
    /// reader never waits on a writer for long.
    pub fn open(path: &Path) -> Result<Reader, StoreError> {
        retrying(|| Self::open_once(path))
    }

    /// Without waiting: [`StoreError::Locked`] at once while a writer holds
    /// the log. For a push a person waits on.
    pub fn open_now(path: &Path) -> Result<Reader, StoreError> {
        Self::open_once(path)
    }

    fn open_once(path: &Path) -> Result<Reader, StoreError> {
        let broken = |why: String| StoreError::Broken {
            path: path.to_path_buf(),
            why,
        };
        match std::fs::metadata(path) {
            Ok(meta) if meta.len() == 0 => return Err(broken("the file is empty".to_owned())),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Reader {
                    db: None,
                    _copy: None,
                })
            }
            Err(error) => return Err(other(error)),
        }
        match ReadOnlyDatabase::open(path) {
            Ok(db) => Ok(Reader {
                db: Some(Box::new(db)),
                _copy: None,
            }),
            Err(DatabaseError::DatabaseAlreadyOpen) => Err(StoreError::Locked),
            // A writer that stopped while it held the log (killed, crashed)
            // leaves it needing a repair, and only a writable open repairs.
            // The repair is made on a private copy, which is read instead;
            // the log itself is repaired by the next writer.
            Err(DatabaseError::RepairAborted) => {
                let unread = |error: &dyn std::fmt::Display| {
                    other(format!(
                        "it was left open by a writer that stopped, and a private copy of it \
                         could not be made and repaired to read ({error})"
                    ))
                };
                let copy = PrivateCopy::of(path).map_err(|error| unread(&error))?;
                let db = Database::open(&copy.0).map_err(|error| unread(&error))?;
                Ok(Reader {
                    db: Some(Box::new(db)),
                    _copy: Some(copy),
                })
            }
            Err(error) => Err(broken(error.to_string())),
        }
    }

    /// As [`Store::all`].
    pub fn all(&self) -> Result<(Vec<Session>, Vec<String>), StoreError> {
        self.db
            .as_deref()
            .map_or_else(|| Ok((Vec::new(), Vec::new())), all_in)
    }
}

/// A copy of the log that only this process reads, in the temporary
/// directory (never beside the log), removed when dropped.
struct PrivateCopy(PathBuf);

impl PrivateCopy {
    fn of(log: &Path) -> std::io::Result<PrivateCopy> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "ohjaa-log-copy-{}-{nanos}-{}.redb",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        // Never an existing file, nor one another user can read.
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        let copy = PrivateCopy(path);
        std::io::copy(&mut std::fs::File::open(log)?, &mut file)?;
        Ok(copy)
    }
}

impl Drop for PrivateCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Make a new log at `path`, which does not exist: whole beside it first,
/// then put in place, so the log is never at its path half made. redb makes
/// a database in the file it is given, which is empty until its header is
/// written, and another ohjaa opening the log in that moment (the first two
/// launches on a machine, at once) would take it for a broken one.
/// `AlreadyExists` when another ohjaa put its log there first.
fn create_whole(path: &Path) -> std::io::Result<Made> {
    create_whole_by(path, &PLACING)
}

/// How a new log came to be at its path.
#[derive(Debug, PartialEq)]
enum Made {
    /// Made whole beside it, then put in place.
    Whole,
    /// Made in place: the folder's filesystem can put no whole file at a
    /// path without the risk of replacing one (why).
    InPlace(String),
}

/// How a whole log made beside its path is put there, never over a file:
/// a hard link, else an exclusive rename (FAT has no hard links).
struct Placing {
    link: fn(&Path, &Path) -> std::io::Result<()>,
    rename: fn(&Path, &Path) -> std::io::Result<()>,
}

const PLACING: Placing = Placing {
    link: |from, to| std::fs::hard_link(from, to),
    rename: rename_exclusive,
};

/// rename(2) that fails with `AlreadyExists` where `to` exists, never
/// replacing it.
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let c = |path: &Path| {
            std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
        };
        let (from, to) = (c(from)?, c(to)?);
        // SAFETY: two NUL-terminated paths that outlive the call.
        #[cfg(target_vendor = "apple")]
        let done = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0;
        // The system call itself: glibc wraps it only from 2.28.
        // SAFETY: as above.
        #[cfg(target_os = "linux")]
        let done = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0;
        if done {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        let _ = (from, to);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// The name a log being made beside `path` has: its maker's pid, and a
/// count within that maker.
fn private_name(path: &Path, pid: u32, n: u64) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.new-{pid}-{n}"))
}

fn create_whole_by(path: &Path, placing: &Placing) -> std::io::Result<Made> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let fresh = private_name(
        path,
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    );
    // One left by an ohjaa stopped while making it, whose pid this one has.
    let _ = std::fs::remove_file(&fresh);
    let made = Database::create(&fresh)
        .map(drop)
        .map_err(std::io::Error::other)
        // Neither a link nor an exclusive rename is ever made over an
        // existing file: the first log made stays, and every other is dropped.
        .and_then(|()| match (placing.link)(&fresh, path) {
            Err(link) if link.kind() != std::io::ErrorKind::AlreadyExists => {
                match (placing.rename)(&fresh, path) {
                    Ok(()) => Ok(None),
                    Err(rename) if rename.kind() == std::io::ErrorKind::AlreadyExists => {
                        Err(rename)
                    }
                    Err(rename) => Ok(Some(format!(
                        "no hard link ({link}) and no exclusive rename ({rename})"
                    ))),
                }
            }
            linked => linked.map(|()| None),
        });
    let _ = std::fs::remove_file(&fresh);
    let Some(why) = made? else {
        return Ok(Made::Whole);
    };
    // Made in place, as redb would make it, but never over an existing file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    if let Err(error) = redb::Builder::new().create_file(file) {
        // Ours, and never a log: the next launch makes it again.
        let _ = std::fs::remove_file(path);
        return Err(std::io::Error::other(error));
    }
    Ok(Made::InPlace(why))
}

/// Say once in a launch why its log was made in place: a launch opening it
/// in that moment may have taken it for a broken one.
fn said_in_place(path: &Path, why: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "ohjaa: the session log {} was made in place: its filesystem has {why}, so a \
             launch opening it as it was made may have found it half made",
            path.display()
        );
    }
}

/// Where a log at `path` is made: `path`, or the end of the links there
/// when they name no file yet (as redb makes it, through them).
fn made_at(path: &Path) -> PathBuf {
    let mut at = path.to_path_buf();
    for _ in 0..40 {
        let Ok(next) = std::fs::read_link(&at) else {
            break;
        };
        at = match at.parent() {
            Some(folder) => folder.join(next),
            None => next,
        };
    }
    at
}

/// Remove what makers stopped while making a log left beside `path`: a
/// private log whose maker is gone, or one older than a minute (no making
/// takes that long, so its pid is another process's now). One a live maker
/// is making is never touched in its first minute. Whatever cannot be read
/// or removed is left.
fn remove_left_beside(path: &Path) {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!(".{}.new-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(rest) = file.to_str().and_then(|file| file.strip_prefix(&prefix)) else {
            continue;
        };
        let gone = rest
            .split('-')
            .next()
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(crate::process::gone);
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > Duration::from_secs(60)));
        if gone || old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Run `open` until it is not [`StoreError::Locked`], for at most about a
/// second and a half: whoever holds the log holds it for one short write.
fn retrying<T>(open: impl Fn() -> Result<T, StoreError>) -> Result<T, StoreError> {
    let mut delay = Duration::from_millis(20);
    let mut waited = Duration::ZERO;
    loop {
        match open() {
            Err(StoreError::Locked) if waited < Duration::from_millis(1500) => {
                std::thread::sleep(delay);
                waited += delay;
                delay = (delay * 2).min(Duration::from_millis(200));
            }
            result => return result,
        }
    }
}

// ---- reading, for a writer and a reader alike --------------------------------

/// A table of the log, or `None` where it was never made: a reader creates
/// nothing, and a missing table holds nothing.
fn read_table<K: redb::Key + 'static, V: redb::Value + 'static>(
    tx: &ReadTransaction,
    table: TableDefinition<'_, K, V>,
) -> Result<Option<redb::ReadOnlyTable<K, V>>, StoreError> {
    match tx.open_table(table) {
        Ok(table) => Ok(Some(table)),
        Err(TableError::TableDoesNotExist(_)) => Ok(None),
        Err(error) => Err(other(error)),
    }
}

fn all_in(db: &dyn ReadableDatabase) -> Result<(Vec<Session>, Vec<String>), StoreError> {
    let tx = db.begin_read().map_err(other)?;
    let Some(sessions) = read_table(&tx, SESSIONS)? else {
        return Ok((Vec::new(), Vec::new()));
    };
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

/// Keep `LATEST` pointing at each repository's latest *ended* session. A
/// session still running is never "the last session here": the perimeter
/// already shows who is live, and an unfinished record must not read as a
/// crash. "Here" is where a session was launched, every repository it
/// named with `--scope`, and every one leaving found it worked in: the one
/// that ended last wins, wherever it started (an orphan by its start: when it
/// ended was never observed).
fn note_latest(tx: &redb::WriteTransaction, session: &Session) -> Result<(), StoreError> {
    let Some(left) = &session.left else {
        return Ok(());
    };
    let sessions = tx.open_table(SESSIONS).map_err(other)?;
    let mut latest = tx.open_table(LATEST).map_err(other)?;
    let heres = std::iter::once(session.repo.as_str())
        .chain(session.scoped.iter().map(|scoped| scoped.repo.as_str()))
        // A repository only found at leaving is never its last session's: nothing
        // in it is attributed to this one.
        .chain(
            left.scoped
                .iter()
                .filter(|scoped| !scoped.found)
                .map(|scoped| scoped.repo.as_str()),
        );
    for here in heres {
        let newer = match latest.get(here).map_err(other)? {
            None => true,
            Some(id) if id.value() == session.id => true,
            Some(id) => {
                let current = sessions
                    .get(id.value())
                    .map_err(other)?
                    .and_then(|value| serde_json::from_slice::<Session>(value.value()).ok());
                let rank = session.ended_rank().unwrap_or(left.ended_ms);
                current
                    .and_then(|current| current.ended_rank())
                    .is_none_or(|ended| rank >= ended)
            }
        };
        if newer {
            latest.insert(here, session.id.as_str()).map_err(other)?;
        }
    }
    Ok(())
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
pub fn clock(ms: i64) -> String {
    let full = rfc3339(ms.div_euclid(1000));
    // "2026-09-26T10:02:03Z" → "2026-09-26 10:02 UTC"
    format!("{} {} UTC", &full[..10], &full[11..16])
}

pub fn describe_check(check: &Check) -> String {
    match check {
        Check::Passed => "verified: passed (sykli verify --commit)".to_owned(),
        Check::Failed { why } => format!("FAILED sykli verify: {why}"),
        Check::Unchecked => "unchecked: no sykli receipt for its tree".to_owned(),
        Check::UncheckedGone => {
            "unchecked: no sykli receipt, and the checkout it was made in is gone".to_owned()
        }
        Check::Unverified { why } => format!("unverified: {why}"),
        Check::Stale { why } => {
            format!("stale: sykli checked the checkout, not the commit ({why})")
        }
        Check::OtherContract { why } => {
            format!("unproven: it ran under another contract than the gate's ({why})")
        }
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

/// Something leaving observed that whoever comes next must hear first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    pub headline: String,
    /// What sykli said, or why it could not.
    pub why: String,
    /// The repository it is about.
    pub repo: String,
    /// For a commit that reached a branch: the branch and the commit, so a
    /// later briefing can tell whether the branch still sits there.
    pub tip: Option<(String, String)>,
    /// The session's own work that sykli refused: only these are lessons for
    /// intake. A broken tip of the default branch it is not shown to have put
    /// there, or one never checked, is said but is no lesson.
    pub own: bool,
}

/// The flags in a record, derived from what ohjaa observed, never from the
/// agent's words:
/// - the tip of the remote default branch as the session left it, when sykli
///   refused it or it has a contract and no run of it at all in a checkout
///   that still exists (gone with its receipts, it cannot be observed): a
///   broken main is the repository's state, whoever pushed it (said as
///   "observed, not attributed" when it is not shown to be the session's);
/// - every commit of the session's own that sykli refused on the default
///   branch, and the head of its open or merged pull request.
///
/// What sykli could not say (no --commit, no contract, a checkout that moved
/// on) is recorded, never shouted.
pub fn flags(session: &Session) -> Vec<Flag> {
    let Some(left) = &session.left else {
        return Vec::new();
    };
    let launch = (!session.outside_repo).then_some((
        session.repo.as_str(),
        &left.landed[..],
        &left.finished,
    ));
    let mut out = Vec::new();
    for (repo, landed, finished) in launch.into_iter().chain(
        left.scoped
            .iter()
            .map(|s| (s.repo.as_str(), &s.landed[..], &s.finished)),
    ) {
        for landed in landed {
            let Some(check) = &landed.check else {
                continue;
            };
            let refused = matches!(check, Check::Failed { .. });
            let mine = !landed.unattributed;
            // What others build on: the remote default branch where it is now.
            let broken_tip = landed.at_tip
                && is_remote(&landed.onto)
                && (refused || matches!(check, Check::Unchecked | Check::OtherContract { .. }));
            if !broken_tip && !(mine && refused) {
                continue;
            }
            let (commit, repo_name) = (short(&landed.commit), repo_name(repo));
            out.push(Flag {
                headline: if landed.at_tip {
                    format!(
                        "{} at {commit} in {repo_name} has no passing evidence{} — check before \
                         building on it",
                        landed.onto,
                        if mine {
                            ""
                        } else {
                            " (observed, not attributed)"
                        }
                    )
                } else {
                    format!(
                        "{commit} reached {} in {repo_name} without passing evidence — check \
                         {} before building on it",
                        landed.onto, landed.onto
                    )
                },
                why: describe_check(check),
                repo: repo.to_owned(),
                tip: Some((landed.onto.clone(), landed.commit.clone())),
                own: mine && refused,
            });
        }
        if let Finished::PullRequest {
            url,
            state,
            head: Some(head),
            head_check: Some(check),
            ..
        } = finished
        {
            let live = matches!(state.to_uppercase().as_str(), "OPEN" | "MERGED");
            if live && matches!(check, Check::Failed { .. }) {
                out.push(Flag {
                    headline: format!(
                        "pull request {url} at {} has no passing evidence — check before \
                         merging or building on it",
                        short(head)
                    ),
                    why: describe_check(check),
                    repo: repo.to_owned(),
                    tip: None,
                    own: true,
                });
            }
        }
    }
    out
}

/// `origin/main`, not a local `main`.
fn is_remote(onto: &str) -> bool {
    onto.contains('/')
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
            launched_ms: None,
            branch: Some("main".to_owned()),
            head_at_start: None,
            scoped: Vec::new(),
            delivered: vec![Delivered {
                id: "rust-only".to_owned(),
                scope: "ours".to_owned(),
                as_of: "2026-09-24".to_owned(),
                repo: None,
                via: None,
            }],
            readback: None,
            handover: None,
            left: None,
            drafted: false,
            usage: Vec::new(),
            usage_refused: None,
            intent_id: None,
            task: None,
            pgid: None,
            group_started_ms: None,
            native_env: Vec::new(),
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
            landed: Vec::new(),
            observed: Vec::new(),
            shared: Vec::new(),
            orphaned: false,
            native: None,
        });
        session
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

    /// The bytes and modification time of `path`, after setting that time an
    /// hour back: any write from here on moves it, however coarse the clock.
    fn settled(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
        let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        stamp(path)
    }

    fn stamp(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
        let modified = std::fs::metadata(path).unwrap().modified().unwrap();
        (std::fs::read(path).unwrap(), modified)
    }

    #[test]
    fn a_reader_reports_an_empty_or_corrupt_log_and_leaves_it_alone() {
        let dir = temp("reader-broken");
        let path = store_path(&dir);
        std::fs::write(&path, b"").unwrap();
        let error = Reader::open(&path).err().expect("an empty log is refused");
        assert!(matches!(error, StoreError::Broken { .. }), "{error}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        std::fs::write(&path, b"this is not a redb database at all, not even close").unwrap();
        let before = settled(&path);
        let error = Reader::open(&path).err().expect("a corrupt log is refused");
        assert!(matches!(error, StoreError::Broken { .. }), "{error}");
        assert!(stamp(&path) == before, "left untouched");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reader_beside_a_writer_is_told_locked_promptly() {
        let dir = temp("reader-locked");
        let path = store_path(&dir);
        let holder = Store::open(&path).unwrap();
        let started = std::time::Instant::now();
        let error = Reader::open(&path)
            .err()
            .expect("a held log is not read beside its writer");
        assert!(matches!(error, StoreError::Locked), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "retries are bounded"
        );
        drop(holder);
        assert!(Reader::open(&path).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn launches_that_make_the_log_together_never_find_it_half_made() {
        // The first launches on a machine make the log at once (two `ohjaa
        // run`s in one checkout). One that opens it while another is still
        // making it must never take the file for a broken log: an empty one.
        for round in 0..40 {
            let dir = temp(&format!("made-together-{round}"));
            let path = store_path(&dir.join("ohjaa"));
            let together = std::sync::Barrier::new(6);
            let found: Vec<String> = std::thread::scope(|scope| {
                let opens: Vec<_> = (0..6)
                    .map(|n| {
                        let (path, together) = (&path, &together);
                        scope.spawn(move || {
                            together.wait();
                            let error = if n % 2 == 0 {
                                Store::open(path).err()
                            } else {
                                Reader::open(path).err()
                            };
                            // Busy beside a writer is said as busy, not broken.
                            error.filter(|error| !matches!(error, StoreError::Locked))
                        })
                    })
                    .collect();
                opens
                    .into_iter()
                    .filter_map(|open| open.join().unwrap())
                    .map(|error| error.to_string())
                    .collect()
            });
            assert!(found.is_empty(), "round {round}: {found:?}");
            assert!(Reader::open(&path).is_ok(), "round {round}");
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn an_exclusive_rename_never_replaces_a_file() {
        let dir = temp("rename-exclusive");
        let (from, to) = (dir.join("from"), dir.join("to"));
        std::fs::write(&from, "new").unwrap();
        std::fs::write(&to, "old").unwrap();
        let refused = rename_exclusive(&from, &to).unwrap_err();
        assert_eq!(
            refused.kind(),
            std::io::ErrorKind::AlreadyExists,
            "{refused}"
        );
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "old");
        assert!(from.is_file());
        std::fs::remove_file(&to).unwrap();
        rename_exclusive(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "new");
        assert!(!from.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn usage_receipt(agent: &str, native: &str) -> crate::usage::Receipt {
        crate::usage::Receipt {
            schema: "agent-usage.v1".to_owned(),
            agent: agent.to_owned(),
            session_id: native.to_owned(),
            source: "test".to_owned(),
            observed_at_ms: 5_000,
            complete: true,
            limitations: Vec::new(),
            entries: Vec::new(),
        }
    }

    /// `ohjaa usage export` is `ohjaa-sessions.v1`: each session as stored.
    /// vessa and seula read these fields; renaming one breaks them silently.
    #[test]
    fn the_v1_export_keeps_the_field_names_its_readers_use() {
        let mut s = session("oh-7", "/r/projects", 1_000);
        s.delivered[0].repo = Some("/r/projects/a".to_owned());
        s.readback = Some(Readback {
            at_ms: 1_100,
            text: "goal".to_owned(),
        });
        s.handover = Some(Handover {
            at_ms: 1_900,
            next: Some("n".to_owned()),
            ..Default::default()
        });
        s.usage = vec![usage_receipt("claude", "native-7")];
        let phase = Phase {
            commit: "c".to_owned(),
            subject: "s".to_owned(),
            evidence: Some(Evidence {
                outcome: "passed".to_owned(),
                dirty: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut s = ended(s, 2_000);
        let left = s.left.as_mut().unwrap();
        left.phases = Some(vec![phase.clone()]);
        left.scoped = vec![ScopedLeft {
            repo: "/r/projects/a".to_owned(),
            phases: Some(vec![phase]),
            finished: Finished::NoPullRequest,
            found: false,
            landed: Vec::new(),
            observed: Vec::new(),
            shared: Vec::new(),
        }];
        let v1 = serde_json::to_value(&s).unwrap();
        for pointer in [
            "/id",
            "/agent",
            "/repo",
            "/intent",
            "/started_ms",
            "/delivered/0/id",
            "/delivered/0/scope",
            "/delivered/0/as_of",
            "/delivered/0/repo",
            "/readback/text",
            "/handover/next",
            "/left/ended_ms",
            "/left/exit/how",
            "/left/finished/kind",
            "/left/phases/0/commit",
            "/left/phases/0/evidence/outcome",
            "/left/scoped/0/repo",
            "/left/scoped/0/phases/0/evidence/outcome",
            "/left/scoped/0/phases/0/evidence/dirty",
            "/usage/0/agent",
            "/usage/0/session_id",
            "/usage/0/entries",
            "/drafted",
        ] {
            assert!(
                v1.pointer(pointer).is_some(),
                "{pointer} is gone from v1: {v1}"
            );
        }
    }
}
