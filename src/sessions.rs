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
//! Beside the sessions, two tables hold the Judgements `ohjaa judge`
//! recorded and the answers to them (see `judgement`): each row written once,
//! never changed or removed. A log an older ohjaa wrote has neither; it is
//! read as it was, and gains them at its next writable open.

use std::collections::HashSet;
use std::fmt::Write as _;
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
/// Judgement id → the Judgement (`ohjaa judge`), as JSON. Written once, and
/// never changed or removed: an answer is a record of its own.
pub const JUDGEMENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("judgements");
/// Response id → an answer to a Judgement (`ohjaa judge respond`), as JSON,
/// naming the Judgement it answers. Written once, like a Judgement.
pub const RESPONSES: TableDefinition<&str, &[u8]> = TableDefinition::new("judgement_responses");

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

/// Whether a validation is of work on its way to the observed branch, or of
/// what is already on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationKind {
    Candidate,
    Integrated,
}

/// The receipt sykli wrote for a validation: its path, and its sha256 as
/// sykli gave it (the name it keeps the receipt under, or its status).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// A validation requested from inside a session (`ohjaa gate`). It is a
/// reference, never an outcome: what became of the run is sykli's to say,
/// and while no receipt is recorded sykli is asked again whenever it is
/// shown. Written before sykli is asked; sykli's run id and the receipt are
/// filled in as sykli gives them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRef {
    /// ohjaa's id for the request (`rq-…`).
    pub request: String,
    /// sykli's id for the run, once sykli gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Integrated: HEAD was the observed branch's tip as this clone last saw
    /// it (`origin/<branch>`); candidate: anything else.
    pub kind: ValidationKind,
    /// The checkout it was requested in.
    pub repo: String,
    /// HEAD when it was requested, and its tree.
    pub commit: String,
    pub tree: String,
    /// The contract it was requested against.
    pub contract: String,
    /// The contract's hash: the one its `sykli.lock` pins, else the one sykli
    /// reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_digest: Option<String>,
    /// The lock's digest, as sykli reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_digest: Option<String>,
    pub requested_ms: i64,
    /// The receipt sykli wrote for it, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ReceiptRef>,
    /// The clone's git common dir: sykli keeps its runs there, so they can be
    /// asked about from any checkout of the clone once this one is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common: Option<String>,
    /// The branch the entry observes, as this clone last saw it when the
    /// validation was requested: never a statement about the remote now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_branch: Option<ObservedBranch>,
    /// What sykli says ran: the tree, and whether anything beside it was
    /// dirty. It may differ from `tree`: a run of uncommitted changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran: Option<Ran>,
    /// Recorded, then never handed to sykli, and why (an integrated
    /// validation whose checkout could not be made, say): said, never
    /// left to read as a run that is merely late.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_dispatched: Option<String>,
}

/// `origin/<branch>` in the requesting clone, as of its last update there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedBranch {
    pub name: String,
    /// Where `origin/<branch>` was; `None` when the clone has no such ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    /// When it last moved in this clone (its reflog, else its file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<i64>,
}

/// The subject of a run, in sykli's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ran {
    pub tree: String,
    pub dirty: bool,
}

impl ValidationRef {
    /// Still waiting on sykli's word: a run id and no receipt yet.
    pub fn pending(&self) -> bool {
        self.run.is_some() && self.receipt.is_none()
    }

    /// Integrated only when sykli says the run was of exactly the commit's
    /// tree, nothing dirty beside it, and the commit was the observed
    /// branch's tip as this clone saw it; a candidate otherwise, and until
    /// sykli has said what ran.
    pub fn settle_kind(&mut self) {
        let at_tip = self
            .observed_branch
            .as_ref()
            .and_then(|branch| branch.tip.as_deref())
            == Some(self.commit.as_str());
        let exact = self
            .ran
            .as_ref()
            .is_some_and(|ran| !ran.dirty && ran.tree == self.tree);
        self.kind = if at_tip && exact {
            ValidationKind::Integrated
        } else {
            ValidationKind::Candidate
        };
    }
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
}

impl Left {
    /// The leaving of a session whose ohjaa is gone, observed at `now_ms`.
    pub fn orphaned(now_ms: i64) -> Left {
        Left {
            ended_ms: now_ms,
            exit: Exit::Unknown,
            toimija_session: None,
            phases: None,
            finished: Finished::Unknown {
                why: "its ohjaa was gone before leaving".to_owned(),
            },
            interrupted: false,
            scoped: Vec::new(),
            landed: Vec::new(),
            observed: Vec::new(),
            shared: Vec::new(),
            orphaned: true,
        }
    }
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
    pub started_ms: i64,
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
    /// Validations requested from inside the session, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validations: Vec<ValidationRef>,
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
        retrying(|| Self::open_once(path))
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
        // Every table exists from the first writable open, so a reader of a
        // log this ohjaa wrote never has to tell "empty" from "missing". A
        // log an older ohjaa wrote gains the newer tables here; its sessions
        // are read as they were.
        let tx = store.db.begin_write().map_err(other)?;
        tx.open_table(SESSIONS).map_err(other)?;
        tx.open_table(LATEST).map_err(other)?;
        tx.open_table(JUDGEMENTS).map_err(other)?;
        tx.open_table(RESPONSES).map_err(other)?;
        tx.commit().map_err(other)?;
        Ok(store)
    }

    /// Write `bytes` as the new row `id` of `table`, in one transaction:
    /// refused when `id` is there already (a row of these tables is written
    /// once, never replaced), and, with `answers`, unless that row exists in
    /// the table named with it (an answer to a Judgement needs the Judgement).
    pub fn insert_new(
        &self,
        table: TableDefinition<&str, &[u8]>,
        id: &str,
        bytes: &[u8],
        answers: Option<(TableDefinition<&str, &[u8]>, &str)>,
    ) -> Result<(), StoreError> {
        let tx = self.db.begin_write().map_err(other)?;
        {
            if let Some((parent, parent_id)) = answers {
                let parent_table = tx.open_table(parent).map_err(other)?;
                if parent_table.get(parent_id).map_err(other)?.is_none() {
                    return Err(StoreError::Other(format!("no {parent_id} in the log")));
                }
            }
            let mut rows = tx.open_table(table).map_err(other)?;
            if rows.get(id).map_err(other)?.is_some() {
                return Err(StoreError::Other(format!(
                    "{id} is already recorded, and a record here is never replaced"
                )));
            }
            rows.insert(id, bytes).map_err(other)?;
        }
        tx.commit().map_err(other)
    }

    /// Every row of `table`, by id. Readers use [`Reader::rows`].
    #[cfg(test)]
    pub fn rows(
        &self,
        table: TableDefinition<&str, &[u8]>,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        rows_in(&self.db, table)
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
        get_in(&self.db, id)
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

    /// Only the tests ask a writer: whatever reads "the last session here"
    /// only reads, through a [`Reader`].
    #[cfg(test)]
    pub fn latest_for(&self, repo: &str) -> Result<Option<Session>, StoreError> {
        latest_in(&self.db, repo)
    }

    /// Every session, oldest first. Unreadable entries are skipped with their
    /// id reported, so one bad record cannot hide the rest.
    pub fn all(&self) -> Result<(Vec<Session>, Vec<String>), StoreError> {
        all_in(&self.db)
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

    pub fn get(&self, id: &str) -> Result<Option<Session>, StoreError> {
        self.db.as_deref().map_or(Ok(None), |db| get_in(db, id))
    }

    pub fn latest_for(&self, repo: &str) -> Result<Option<Session>, StoreError> {
        self.db
            .as_deref()
            .map_or(Ok(None), |db| latest_in(db, repo))
    }

    /// As [`Store::all`].
    pub fn all(&self) -> Result<(Vec<Session>, Vec<String>), StoreError> {
        self.db
            .as_deref()
            .map_or_else(|| Ok((Vec::new(), Vec::new())), all_in)
    }

    /// As [`Store::rows`]; none in a log an older ohjaa wrote.
    pub fn rows(
        &self,
        table: TableDefinition<&str, &[u8]>,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.db
            .as_deref()
            .map_or_else(|| Ok(Vec::new()), |db| rows_in(db, table))
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

fn get_in(db: &dyn ReadableDatabase, id: &str) -> Result<Option<Session>, StoreError> {
    let tx = db.begin_read().map_err(other)?;
    let Some(sessions) = read_table(&tx, SESSIONS)? else {
        return Ok(None);
    };
    let Some(value) = sessions.get(id).map_err(other)? else {
        return Ok(None);
    };
    serde_json::from_slice(value.value())
        .map(Some)
        .map_err(|error| other(format!("session {id} is unreadable: {error}")))
}

fn latest_in(db: &dyn ReadableDatabase, repo: &str) -> Result<Option<Session>, StoreError> {
    let id = {
        let tx = db.begin_read().map_err(other)?;
        let Some(latest) = read_table(&tx, LATEST)? else {
            return Ok(None);
        };
        match latest.get(repo).map_err(other)? {
            Some(id) => id.value().to_owned(),
            None => return Ok(None),
        }
    };
    get_in(db, &id)
}

/// Every row of `table`, by id; none where the table was never made (a log
/// an older ohjaa wrote, opened only to read).
fn rows_in(
    db: &dyn ReadableDatabase,
    table: TableDefinition<&str, &[u8]>,
) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let tx = db.begin_read().map_err(other)?;
    let Some(rows) = read_table(&tx, table)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in rows.iter().map_err(other)? {
        let (id, value) = entry.map_err(other)?;
        out.push((id.value().to_owned(), value.value().to_vec()));
    }
    Ok(out)
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

/// A fresh session id: when it started, and which ohjaa started it.
pub fn new_id(started_ms: i64) -> String {
    format!("oh-{started_ms}-{}", std::process::id())
}

/// A fresh Intent id, in the same style as a session's: when it was minted,
/// and by which ohjaa. Never a session's id: its prefix differs.
pub fn new_intent_id(at_ms: i64) -> String {
    format!("in-{at_ms}-{}", std::process::id())
}

/// A fresh id for a validation request (`ohjaa gate`), in the same style.
pub fn new_request_id(at_ms: i64) -> String {
    format!("rq-{at_ms}-{}", std::process::id())
}

/// The pid of the ohjaa that started a session, from its id.
pub fn pid_of(id: &str) -> Option<u32> {
    let (_, pid) = id.strip_prefix("oh-")?.rsplit_once('-')?;
    pid.parse().ok().filter(|pid| *pid > 0)
}

/// What a session still open in the log is, observed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// Its ohjaa still runs, or that cannot be told: it is left alone.
    Running,
    /// Its ohjaa is gone, but the session's own process group still runs (or
    /// that cannot be told): its agent outlived its ohjaa. Not an orphan;
    /// its leaving will not be recorded.
    OhjaaGone,
    /// Its ohjaa is gone, and so is its process group (or none was
    /// recorded): an orphan.
    Orphaned,
}

/// Observe an open session now: its ohjaa by the pid in its id, then its
/// process group (`pgid`). A process is never declared gone on a guess.
pub fn observe_open(session: &Session) -> Open {
    let Some(pid) = pid_of(&session.id) else {
        return Open::Running;
    };
    if pid == std::process::id()
        || crate::process::ohjaa_alive(pid, session.started_ms) != Some(false)
    {
        return Open::Running;
    }
    let started = session.group_started_ms.unwrap_or(session.started_ms);
    match session.pgid {
        Some(pgid) if crate::process::group_alive(pgid, started) != Some(false) => Open::OhjaaGone,
        _ => Open::Orphaned,
    }
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

fn describe_exit(exit: Exit) -> String {
    match exit {
        Exit::Code { code: 0 } => "clean (exit 0)".to_owned(),
        Exit::Code { code } => format!("exit {code}"),
        Exit::Signal { signal } => format!("killed by signal {signal}"),
        Exit::Unknown => "unknown".to_owned(),
    }
}

/// How a session ended, as its record says it.
fn describe_end(left: &Left) -> String {
    if left.orphaned {
        "orphaned (observed at next launch)".to_owned()
    } else {
        describe_exit(left.exit)
    }
}

/// A receipt's own word, said as its word: only `sykli verify --commit`
/// ([`describe_check`]) says a commit passed.
fn describe_evidence(evidence: Option<&Evidence>) -> String {
    match evidence {
        Some(e) if e.dirty => format!("receipt: {} (dirty tree)", e.outcome),
        Some(e) => format!("receipt: {}{}", e.outcome, describe_cached(e)),
        None => "no sykli receipt".to_owned(),
    }
}

/// How much of a run sykli reused from its cache instead of running it, said
/// apart from what ran: ` (1 of 3 tasks cached)`, ` (all 2 tasks cached)`,
/// or nothing when every task ran (or the receipt does not say).
pub fn describe_cached(evidence: &Evidence) -> String {
    match (evidence.cached, evidence.tasks) {
        (0, _) => String::new(),
        (cached, tasks) if cached >= tasks => format!(" (all {cached} tasks cached)"),
        (cached, tasks) => format!(" ({cached} of {tasks} tasks cached)"),
    }
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
            format!("unproven: it passed under another contract than the gate's ({why})")
        }
    }
}

/// What is known of a landed commit: sykli's word at a push's tip, its
/// receipt's word before it.
fn describe_landed(landed: &Landed) -> String {
    match &landed.check {
        // The receipt sykli accepted, when it reused its cache: said so.
        Some(Check::Passed) => format!(
            "{}{}",
            describe_check(&Check::Passed),
            landed
                .evidence
                .as_ref()
                .map(describe_cached)
                .unwrap_or_default()
        ),
        Some(check) => describe_check(check),
        None => format!(
            "{} (not verified: {})",
            describe_evidence(landed.evidence.as_ref()),
            if landed.unattributed {
                "not shown to be this session's"
            } else {
                "only a push's tip is"
            }
        ),
    }
}

fn landed_lines(label: &str, landed: &[Landed], out: &mut String) {
    for landed in landed {
        let _ = writeln!(
            out,
            "{label}{} reached {}{}: {}",
            short(&landed.commit),
            landed.onto,
            if landed.unattributed {
                " (not attributed)"
            } else {
                ""
            },
            describe_landed(landed)
        );
    }
}

/// Where a commit was made, when that is not the repository itself.
fn place_of(phase: &Phase) -> String {
    if phase.unreachable {
        " (on no branch now)".to_owned()
    } else {
        phase
            .worktree
            .as_deref()
            .map(|path| format!(" (in {})", repo_name(path)))
            .unwrap_or_default()
    }
}

/// Commits made during the session that are not shown to be its own, and
/// the sessions that shared its places: said, never credited.
fn observed_lines(
    label: &str,
    shared_label: &str,
    observed: &[Phase],
    shared: &[String],
    out: &mut String,
) {
    if !observed.is_empty() {
        let list = observed
            .iter()
            .map(|p| format!("{}{}", short(&p.commit), place_of(p)))
            .collect::<Vec<_>>()
            .join(" · ");
        if label.ends_with(": ") {
            let _ = writeln!(out, "{label}{list}");
        } else {
            let _ = writeln!(
                out,
                "{label}made during the session, not attributed: {list}"
            );
        }
    }
    for with in shared {
        let _ = writeln!(out, "{shared_label}{with}");
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
#[cfg(test)]
pub fn render_record(session: &Session) -> String {
    render_record_with(session, &|_| None)
}

/// The leaving record, with what sykli says now of each validation the
/// session requested (`said`, asked by the caller).
pub fn render_record_with(
    session: &Session,
    said: &dyn Fn(&ValidationRef) -> Option<Said>,
) -> String {
    let mut out = String::new();
    let ended = session.left.as_ref().map_or_else(
        || "still open".to_owned(),
        |left| format!("{} · ended: {}", clock(left.ended_ms), describe_end(left)),
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
    let flagged = flags(session);
    if !flagged.is_empty() {
        out.push_str("─ FLAGGED (observed by ohjaa; the next briefing opens with it) ─\n");
        for flag in flagged {
            let _ = writeln!(out, "! {}\n  ({})", flag.headline, flag.why);
        }
    }
    out.push_str("─ observed (written by ohjaa; the agent cannot change it) ─\n");
    let _ = writeln!(
        out,
        "intent        {}{}",
        session.intent,
        intent_note(session)
    );
    if let Some(task) = &session.task {
        let _ = writeln!(out, "task          {task}");
    }
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
                describe_phases(left.phases.as_ref(), left)
            );
            observed_lines(
                "observed      ",
                "shared        ",
                &left.observed,
                &left.shared,
                &mut out,
            );
            let _ = writeln!(out, "finished      {}", describe_finished(&left.finished));
            landed_lines("landed        ", &left.landed, &mut out);
        } else if left.scoped.is_empty() {
            // The folder has nothing to observe, and no --scope named a repository.
            out.push_str("phases        none to observe (not a Git repository)\n");
            out.push_str("finished      none to observe (not a Git repository)\n");
        }
        for scoped in &left.scoped {
            let label = format!("in {}", repo_name(&scoped.repo));
            if scoped.found {
                // Found, not named: nothing in it is this session's (law 3).
                observed_lines(
                    &format!("{label:<13} observed during the session, not attributed: "),
                    &format!("{label:<13} shared "),
                    &scoped.observed,
                    &scoped.shared,
                    &mut out,
                );
                landed_lines(&format!("{label:<13} landed "), &scoped.landed, &mut out);
                let _ = writeln!(
                    out,
                    "{label:<13} not named with --scope: found at leaving because a commit was \
                     made in it; nothing in it is attributed to this session"
                );
                continue;
            }
            let _ = writeln!(
                out,
                "{label:<13} phases {}",
                describe_phases(scoped.phases.as_ref(), left)
            );
            observed_lines(
                &format!("{label:<13} observed "),
                &format!("{label:<13} shared "),
                &scoped.observed,
                &scoped.shared,
                &mut out,
            );
            let _ = writeln!(
                out,
                "{label:<13} finished {}",
                describe_finished(&scoped.finished)
            );
            landed_lines(&format!("{label:<13} landed "), &scoped.landed, &mut out);
        }
        if let Some(toimija) = &left.toimija_session {
            let _ = writeln!(out, "toimija       {toimija}");
        }
    }
    out.push_str(&render_validations(
        &session.validations,
        "requested     ",
        said,
    ));
    out.push_str("─ usage (imported source assertions; estimates are not bills) ─\n");
    if let Some(why) = &session.usage_refused {
        let _ = writeln!(out, "refused: {why}");
    } else if session.usage.is_empty() {
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

/// Whether an ended session was here: launched here, or named here with
/// `--scope` (a repository only found at leaving is not its).
pub fn touches(session: &Session, here: &str) -> bool {
    session.left.as_ref().is_some_and(|left| {
        session.repo == here
            || session.scoped.iter().any(|s| s.repo == here)
            || left.scoped.iter().any(|s| s.repo == here && !s.found)
    })
}

/// Whether an ended session saw `here`: it was here ([`touches`]), or found
/// it at leaving. What it saw of a branch here can still hold.
pub fn observed_in(session: &Session, here: &str) -> bool {
    touches(session, here)
        || session
            .left
            .as_ref()
            .is_some_and(|left| left.scoped.iter().any(|s| s.repo == here))
}

/// Sessions closed as orphaned at this launch, for the very top of its
/// briefing: nothing they did was observed. With the validations each
/// requested, a pending one as sykli says it now (`said`).
pub fn render_orphaned(
    orphans: &[Session],
    said: &dyn Fn(&ValidationRef) -> Option<Said>,
) -> String {
    let mut out = String::new();
    for session in orphans {
        let _ = writeln!(
            out,
            "ORPHANED: session {} ({}, {}), started {}, was never closed: its ohjaa was \
             gone at this launch, so nothing it did was observed. Check `git log` and \
             the worktrees here before building on its work.",
            session.id,
            session.agent,
            session.intent,
            clock(session.started_ms)
        );
        out.push_str(&render_validations(&session.validations, "  - ", said));
    }
    out
}

/// Sessions whose ohjaa is gone while their own process group still runs:
/// not orphans. Their agent may still be at work, and their leaving will
/// not be recorded. With the validations each requested, as for an orphan.
pub fn render_still_running(
    sessions: &[Session],
    said: &dyn Fn(&ValidationRef) -> Option<Said>,
) -> String {
    let mut out = String::new();
    for session in sessions {
        let _ = writeln!(
            out,
            "STILL RUNNING: session {} ({}, {}), started {}: its process group {} still \
             runs, but the ohjaa that started it is gone, so its leaving will not be \
             recorded. Its work may still be changing; check `git log` and its worktree \
             before building on it.",
            session.id,
            session.agent,
            session.intent,
            clock(session.started_ms),
            session
                .pgid
                .map_or_else(|| "?".to_owned(), |pgid| pgid.to_string())
        );
        out.push_str(&render_validations(&session.validations, "  - ", said));
    }
    out
}

/// What sykli says of a pending validation now, asked by the caller: the
/// words to show, labelled as sykli's, and the receipt sykli named, if it
/// did. Shown, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub text: String,
    pub receipt: Option<ReceiptRef>,
    /// sykli says the run is over (in whatever state).
    pub ended: bool,
}

/// ` (intent <id>)` when the session carries an Intent id.
fn intent_note(session: &Session) -> String {
    session
        .intent_id
        .as_ref()
        .map_or_else(String::new, |id| format!(" (intent {id})"))
}

/// `; receipt <path> (sha256 <12>)`.
fn describe_receipt(receipt: &ReceiptRef) -> String {
    match &receipt.digest {
        Some(digest) => format!(
            "; receipt {} (sha256 {})",
            receipt.path,
            digest.get(..12).unwrap_or(digest)
        ),
        None => format!("; receipt {} (no digest)", receipt.path),
    }
}

/// One line per validation, each after `prefix`: what ohjaa recorded when it
/// asked (its request, kind, commit and tree), then sykli's run and receipt
/// as recorded. A pending one (a run id, no receipt yet) is said as sykli
/// says it now (`said`, asked by the caller), labelled as sykli's. Nothing
/// here is a verdict of ohjaa's.
pub fn render_validations(
    refs: &[ValidationRef],
    prefix: &str,
    said: &dyn Fn(&ValidationRef) -> Option<Said>,
) -> String {
    let mut out = String::new();
    for validation in refs {
        let kind = match validation.kind {
            ValidationKind::Candidate => "candidate",
            ValidationKind::Integrated => "integrated",
        };
        let answer = match (&validation.run, &validation.receipt) {
            (None, None) => match &validation.not_dispatched {
                Some(why) => format!("recorded, never handed to sykli: {why}"),
                None => "no sykli run id; no receipt recorded".to_owned(),
            },
            (None, Some(receipt)) => format!("no sykli run id{}", describe_receipt(receipt)),
            (Some(run), Some(receipt)) => format!("sykli run {run}{}", describe_receipt(receipt)),
            (Some(run), None) => match said(validation) {
                Some(Said { text, receipt, .. }) => format!(
                    "{text}{}",
                    receipt.as_ref().map(describe_receipt).unwrap_or_default()
                ),
                None => format!("sykli run {run}, no receipt recorded (sykli not asked)"),
            },
        };
        // What sykli ran, when it is not exactly the commit's tree.
        let ran = match &validation.ran {
            Some(ran) if ran.dirty || ran.tree != validation.tree => format!(
                "; sykli ran tree {}{}",
                short(&ran.tree),
                if ran.dirty { ", dirty" } else { "" }
            ),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "{prefix}validation {} ({kind}, {} tree {}{ran}): {answer}",
            validation.request,
            short(&validation.commit),
            short(&validation.tree)
        );
    }
    out
}

/// The flags of the last session here, and of earlier ones that still hold,
/// for the very top of the next briefing: before the knowledge, and before a
/// handover can say "ship it". Every flag of `last` about its own work is
/// said ("<branch> at <sha>" only while the branch still sits there). Any
/// other flag about a branch — an earlier session's, a broken tip it only
/// observed, or one a session found working in `here` saw — is said while
/// `still_at(repo, branch, commit)`: the branch still sits at the commit it
/// flagged, so a quiet session in between cannot hide a broken main. Each tip
/// is said once. `earlier` is newest first. Empty when nothing is flagged.
pub fn render_warnings(
    last: Option<&Session>,
    earlier: &[Session],
    here: &str,
    still_at: impl Fn(&str, &str, &str) -> bool,
) -> String {
    let mut out = String::new();
    let mut said = HashSet::new();
    let mut block = |session: &Session, whose: &str, still: &str, flags: Vec<Flag>| {
        let flags: Vec<Flag> = flags
            .into_iter()
            .filter(|flag| {
                said.insert(flag.tip.as_ref().map_or_else(
                    || flag.headline.clone(),
                    |(branch, commit)| format!("{}\u{1f}{branch}\u{1f}{commit}", flag.repo),
                ))
            })
            .collect();
        if flags.is_empty() {
            return;
        }
        let ended = session.left.as_ref().map_or_else(String::new, |left| {
            format!(", ended {}", clock(left.ended_ms))
        });
        let _ = writeln!(
            out,
            "WARNING from {whose} ({}, {}{ended}){still}:",
            session.id, session.agent
        );
        for flag in flags {
            let _ = writeln!(out, "- {} ({})", flag.headline, flag.why);
        }
    };
    let standing = |flag: &Flag| {
        flag.tip
            .as_ref()
            .is_some_and(|(branch, commit)| still_at(&flag.repo, branch, commit))
    };
    if let Some(last) = last {
        let last_flags = flags(last)
            .into_iter()
            .filter_map(|mut flag| {
                if flag.tip.is_none() || standing(&flag) {
                    return Some(flag);
                }
                // Its own refused work once the branch has moved on: said as
                // reached, never as where the branch is.
                let (branch, commit) = flag.tip.clone()?;
                flag.own.then(|| {
                    flag.headline = format!(
                        "{} reached {branch} in {} without passing evidence; {branch} has moved \
                         on since — check what is on it before building on it",
                        short(&commit),
                        repo_name(&flag.repo)
                    );
                    flag
                })
            })
            .collect();
        block(last, "the last session here", "", last_flags);
    }
    for session in earlier {
        let whose = if touches(session, here) {
            "an earlier session here"
        } else {
            "a session that observed this repository"
        };
        let standing = flags(session)
            .into_iter()
            .filter(|flag| flag.repo == here && standing(flag))
            .collect();
        block(session, whose, ", still true", standing);
    }
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

fn describe_phases(phases: Option<&Vec<Phase>>, left: &Left) -> String {
    match phases {
        None if left.orphaned => "not observed (its ohjaa was gone before leaving)".to_owned(),
        None if left.interrupted => "not observed (leaving was interrupted)".to_owned(),
        None => "not observed".to_owned(),
        Some(phases) if phases.is_empty() => "no commits".to_owned(),
        Some(phases) => phases
            .iter()
            .map(|p| {
                format!(
                    "{} {}{}",
                    short(&p.commit),
                    describe_evidence(p.evidence.as_ref()),
                    place_of(p)
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
            head: Some(head),
            head_check: Some(check),
            ..
        } => format!(
            "{url} ({}) · head {}: {}",
            state.to_lowercase(),
            short(head),
            describe_check(check)
        ),
        // Recorded before ohjaa asked sykli.
        Finished::PullRequest {
            url,
            state,
            head_evidence,
            ..
        } => format!(
            "{url} ({}) · its head, unverified: {}",
            state.to_lowercase(),
            describe_evidence(head_evidence.as_ref())
        ),
    }
}

fn summarise_work(phases: Option<&Vec<Phase>>) -> String {
    match phases {
        Some(phases) => {
            let says = |outcome: &str| {
                phases
                    .iter()
                    .filter_map(|p| p.evidence.as_ref())
                    .filter(|e| e.outcome == outcome)
                    .collect::<Vec<_>>()
            };
            let passed = says("passed");
            let reusing = passed.iter().filter(|e| e.cached > 0).count();
            let cached = says("cached").len();
            let mut said = format!(
                "{} commit(s), {} whose sykli receipt says passed",
                phases.len(),
                passed.len()
            );
            if reusing > 0 {
                let _ = write!(said, " ({reusing} reusing cached tasks)");
            }
            if cached > 0 {
                let _ = write!(said, ", {cached} whose receipt says cached (no task run)");
            }
            said
        }
        None => "commits not observed".to_owned(),
    }
}

fn summarise_finished(finished: &Finished) -> String {
    match finished {
        Finished::PullRequest {
            url,
            state,
            head: Some(head),
            head_check: Some(check),
            ..
        } => format!(
            "{url} ({}), head {} {}",
            state.to_lowercase(),
            short(head),
            describe_check(check)
        ),
        Finished::PullRequest { url, state, .. } => format!("{url} ({})", state.to_lowercase()),
        Finished::NoPullRequest => "no pull request".to_owned(),
        Finished::Unknown { .. } => "unknown".to_owned(),
    }
}

/// "Last session here", for the next session's entering briefing in `here`
/// (a `LATEST` key). A session launched elsewhere that named `here` with
/// `--scope` says where it was launched, so it never reads as launched here.
#[cfg(test)]
pub fn render_last(session: &Session, here: &str) -> String {
    render_last_with(session, here, &|_| None)
}

/// The same, with what sykli says now of each validation it requested.
pub fn render_last_with(
    session: &Session,
    here: &str,
    said: &dyn Fn(&ValidationRef) -> Option<Said>,
) -> String {
    let mut out = String::new();
    // Only ended sessions are "the last session here" (see `note_latest`);
    // a record without its leaving is said as exactly that, never as a crash.
    let ended = session.left.as_ref().map_or_else(
        || "leaving not recorded".to_owned(),
        |left| format!("ended {}, {}", clock(left.ended_ms), describe_end(left)),
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
    let _ = writeln!(out, "- intent: {}{}", session.intent, intent_note(session));
    let scoped = session
        .left
        .as_ref()
        .map_or(&[][..], |left| &left.scoped[..]);
    // What was made during it but is not shown to be its own: said as such.
    let observed = |observed: &[Phase], shared: &[String], out: &mut String| {
        if !observed.is_empty() {
            let _ = writeln!(
                out,
                "  ({} more commit(s) made during it are not attributed to it{})",
                observed.len(),
                if shared.is_empty() {
                    String::new()
                } else {
                    format!(": it shared its place, {}", shared.join("; "))
                }
            );
        }
    };
    if session.left.as_ref().is_some_and(|left| left.orphaned) {
        out.push_str(
            "- work: not observed: its ohjaa was gone before leaving; check `git log` and \
             the worktrees before building on it\n",
        );
    } else if session.outside_repo && scoped.is_empty() {
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
            observed(&left.observed, &left.shared, &mut out);
            for landed in left.landed.iter().filter(|l| !l.unattributed) {
                let _ = writeln!(
                    out,
                    "- {} reached {}: {}",
                    short(&landed.commit),
                    landed.onto,
                    describe_landed(landed)
                );
            }
        }
        for scoped in scoped {
            if scoped.found {
                let _ = writeln!(
                    out,
                    "- in {}: {} commit(s) observed during the session, not attributed (found \
                     at leaving)",
                    repo_name(&scoped.repo),
                    scoped.observed.len()
                );
                continue;
            }
            let _ = writeln!(
                out,
                "- work in {}: {}; finished: {}",
                repo_name(&scoped.repo),
                summarise_work(scoped.phases.as_ref()),
                summarise_finished(&scoped.finished),
            );
            observed(&scoped.observed, &scoped.shared, &mut out);
            for landed in scoped.landed.iter().filter(|l| !l.unattributed) {
                let _ = writeln!(
                    out,
                    "- in {}, {} reached {}: {}",
                    repo_name(&scoped.repo),
                    short(&landed.commit),
                    landed.onto,
                    describe_landed(landed)
                );
            }
        }
    }
    // Observed, not testimony: the validations it requested, as sykli says now.
    out.push_str(&render_validations(&session.validations, "- ", said));
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
                repo: None,
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
            validations: Vec::new(),
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
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    finished: Finished::NoPullRequest,
                    found: false,
                    landed: Vec::new(),
                    observed: Vec::new(),
                    shared: Vec::new(),
                },
                ScopedLeft {
                    repo: "/r/projects/toimija".to_owned(),
                    phases: Some(Vec::new()),
                    finished: Finished::NoPullRequest,
                    found: true,
                    landed: Vec::new(),
                    observed: Vec::new(),
                    shared: Vec::new(),
                },
            ],
            landed: Vec::new(),
            observed: Vec::new(),
            shared: Vec::new(),
            orphaned: false,
        });
        let record = render_record(&s);
        assert!(
            !record.contains("none to observe"),
            "a root session with scoped repositories has work to show: {record}"
        );
        assert!(
            record.contains("in ohjaa      phases a1b2c3d receipt: passed"),
            "{record}"
        );
        assert!(
            !record.contains("in toimija    phases") && !record.contains("in toimija    finished"),
            "a found repository has no phases or pull request of this session's: {record}"
        );
        let last = render_last(&s, &s.repo);
        assert!(
            last.contains("- work in ohjaa: 1 commit(s), 1 whose sykli receipt says passed"),
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
        // Found at leaving, it is never said as named, nor credited.
        assert!(
            record.contains("in toimija    not named with --scope: found at leaving because a commit was made in it; nothing in it is attributed to this session"),
            "{record}"
        );
        assert!(
            last.contains("- in toimija: 0 commit(s) observed during the session, not attributed (found at leaving)"),
            "{last}"
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

    /// KATANA Stage 5: the Judgement tables are additive. A log an older
    /// ohjaa wrote (sessions and latest only) is read as it was, with no
    /// Judgement, opened only to read; the first writable open adds the
    /// tables, and its sessions are untouched.
    #[test]
    fn a_log_from_before_judgements_still_reads_and_gains_the_tables() {
        let dir = temp("oldlog");
        let path = store_path(&dir);
        {
            let db = Database::create(&path).unwrap();
            let tx = db.begin_write().unwrap();
            {
                let mut sessions = tx.open_table(SESSIONS).unwrap();
                let bytes = serde_json::to_vec(&session("oh-1-1", "/r/a", 1_000)).unwrap();
                sessions.insert("oh-1-1", bytes.as_slice()).unwrap();
                tx.open_table(LATEST).unwrap();
            }
            tx.commit().unwrap();
        }
        let reader = Reader::open(&path).unwrap();
        assert_eq!(reader.rows(JUDGEMENTS).unwrap(), Vec::new());
        assert_eq!(reader.rows(RESPONSES).unwrap(), Vec::new());
        assert_eq!(reader.all().unwrap().0.len(), 1);
        drop(reader);
        let store = Store::open(&path).unwrap();
        store.insert_new(JUDGEMENTS, "jd-1-1", b"{}", None).unwrap();
        assert_eq!(store.get("oh-1-1").unwrap().unwrap().repo, "/r/a");
        drop(store);
        let reader = Reader::open(&path).unwrap();
        assert_eq!(reader.rows(JUDGEMENTS).unwrap().len(), 1);
    }

    /// D5: a Judgement is written once and never replaced; an answer is a
    /// row of its own, refused for a Judgement the log does not hold.
    #[test]
    fn a_judgement_row_is_never_replaced_and_an_answer_needs_its_judgement() {
        let dir = temp("judgementrows");
        let store = Store::open(&store_path(&dir)).unwrap();
        store
            .insert_new(JUDGEMENTS, "jd-1-1", b"{\"v\":1}", None)
            .unwrap();
        let again = store
            .insert_new(JUDGEMENTS, "jd-1-1", b"{\"v\":2}", None)
            .unwrap_err()
            .to_string();
        assert!(again.contains("never replaced"), "{again}");
        assert_eq!(
            store.rows(JUDGEMENTS).unwrap(),
            vec![("jd-1-1".to_owned(), b"{\"v\":1}".to_vec())]
        );
        let orphan = store
            .insert_new(RESPONSES, "rs-1-1", b"{}", Some((JUDGEMENTS, "jd-9-9")))
            .unwrap_err()
            .to_string();
        assert!(orphan.contains("no jd-9-9 in the log"), "{orphan}");
        store
            .insert_new(RESPONSES, "rs-1-1", b"{}", Some((JUDGEMENTS, "jd-1-1")))
            .unwrap();
        assert_eq!(store.rows(RESPONSES).unwrap().len(), 1);
        assert_eq!(
            store.rows(JUDGEMENTS).unwrap()[0].1,
            b"{\"v\":1}".to_vec(),
            "the Judgement is unchanged"
        );
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
    fn an_orphan_never_outranks_a_session_that_really_ended_after_it_started() {
        let dir = temp("orphan-rank");
        let store = Store::open(&store_path(&dir)).unwrap();
        // Started first, then its ohjaa was gone; found so only much later.
        let mut orphan = session("oh-1", "/r/a", 1_000);
        orphan.left = Some(Left::orphaned(9_000));
        store
            .put(&ended(session("oh-2", "/r/a", 2_000), 3_000))
            .unwrap();
        store.put(&orphan).unwrap();
        assert_eq!(
            store.latest_for("/r/a").unwrap().unwrap().id,
            "oh-2",
            "when it was found gone is not when it ended"
        );
        let record = render_record(&orphan);
        assert!(
            record.contains("phases        not observed (its ohjaa was gone before leaving)"),
            "it says what was observed, never a cause: {record}"
        );
        assert!(!record.contains("killed"), "{record}");
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
    fn a_reader_never_writes_the_log_and_never_creates_one() {
        let dir = temp("reader");
        let path = store_path(&dir);
        let none = Reader::open(&path).unwrap();
        assert_eq!(none.all().unwrap(), (Vec::new(), Vec::new()));
        assert!(none.latest_for("/r/a").unwrap().is_none());
        assert!(!path.exists(), "reading never creates the log");
        {
            let store = Store::open(&path).unwrap();
            store
                .put(&ended(session("oh-1", "/r/a", 1_000), 1_500))
                .unwrap();
            store.put(&session("oh-2", "/r/a", 2_000)).unwrap();
        }
        let before = settled(&path);
        let reader = Reader::open(&path).unwrap();
        let (all, unreadable) = reader.all().unwrap();
        assert_eq!(
            all.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["oh-1", "oh-2"]
        );
        assert!(unreadable.is_empty());
        assert_eq!(reader.latest_for("/r/a").unwrap().unwrap().id, "oh-1");
        assert_eq!(reader.get("oh-2").unwrap().unwrap().id, "oh-2");
        drop(reader);
        assert!(stamp(&path) == before, "a reader wrote the log");
        // A writer opens it as before, after a reader let it go.
        Store::open(&path).unwrap();
        // A log with no tables yet holds no sessions.
        let bare = dir.join("bare.redb");
        drop(Database::create(&bare).unwrap());
        let reader = Reader::open(&bare).unwrap();
        assert_eq!(reader.all().unwrap(), (Vec::new(), Vec::new()));
        assert!(reader.get("oh-1").unwrap().is_none());
        assert!(reader.latest_for("/r/a").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
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
    fn a_log_a_writer_left_unclean_is_read_from_a_private_copy_never_repaired_in_place() {
        let dir = temp("reader-unclean");
        let path = store_path(&dir);
        let unclean = dir.join("unclean.redb");
        {
            let store = Store::open(&path).unwrap();
            store
                .put(&ended(session("oh-1", "/r/a", 1_000), 1_500))
                .unwrap();
            // Its bytes while a writer still holds it: what a writer killed
            // there leaves behind.
            std::fs::copy(&path, &unclean).unwrap();
        }
        assert!(
            matches!(
                redb::ReadOnlyDatabase::open(&unclean),
                Err(DatabaseError::RepairAborted)
            ),
            "the fixture needs a repair"
        );
        let before = settled(&unclean);
        let reader = Reader::open(&unclean).unwrap();
        let copy = reader._copy.as_ref().expect("read from a copy").0.clone();
        assert!(
            copy.is_file() && !copy.starts_with(&dir),
            "{}",
            copy.display()
        );
        assert_eq!(reader.all().unwrap().0[0].id, "oh-1");
        assert_eq!(reader.latest_for("/r/a").unwrap().unwrap().id, "oh-1");
        drop(reader);
        assert!(!copy.exists(), "the private copy is removed");
        assert!(
            stamp(&unclean) == before,
            "the log itself is never repaired"
        );
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
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Phase {
                    commit: "0718abcdef".to_owned(),
                    subject: "two".to_owned(),
                    evidence: None,
                    ..Default::default()
                },
            ]),
            finished: Finished::NoPullRequest,
            interrupted: false,
            scoped: Vec::new(),
            landed: Vec::new(),
            observed: Vec::new(),
            shared: Vec::new(),
            orphaned: false,
        });
        let record = render_record(&s);
        let observed = record.find("─ observed").unwrap();
        let testimony = record.find("─ handover").unwrap();
        assert!(observed < testimony, "{record}");
        assert!(record.contains("ended: clean (exit 0)"), "{record}");
        assert!(
            record.contains("a1b2c3d receipt: passed · 0718abc no sykli receipt"),
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
            last.contains("2 commit(s), 1 whose sykli receipt says passed"),
            "{last}"
        );
        assert!(last.contains("- unfinished: windows paths"), "{last}");
        assert!(
            last.contains("- it was surprised by: the fixture skips"),
            "{last}"
        );
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

    #[test]
    fn one_unreadable_row_never_blocks_an_import_and_is_named() {
        let dir = temp("unreadable-row");
        let store = Store::open(&store_path(&dir)).unwrap();
        store.put(&session("oh-1", "/r/a", 1_000)).unwrap();
        {
            let tx = store.db.begin_write().unwrap();
            tx.open_table(SESSIONS)
                .unwrap()
                .insert("oh-broken", b"\"not a session\"".as_slice())
                .unwrap();
            tx.commit().unwrap();
        }
        let skipped = store
            .attach_usage("oh-1", usage_receipt("claude", "native-1"))
            .expect("an unrelated unreadable row is skipped, not fatal");
        assert_eq!(skipped, ["oh-broken"], "and it is named");
        assert_eq!(store.get("oh-1").unwrap().unwrap().usage.len(), 1);
        let error = store
            .attach_usage("oh-broken", usage_receipt("claude", "native-2"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("session oh-broken is unreadable"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn only_sykli_says_a_commit_passed_never_its_receipt() {
        let mut s = session("oh-11", "/r/selko", 1_790_000_000_000);
        let mut left = ended(s.clone(), 1_790_003_600_000).left.unwrap();
        left.finished = Finished::PullRequest {
            url: "https://github.com/o/r/pull/7".to_owned(),
            state: "OPEN".to_owned(),
            head_evidence: Some(Evidence {
                outcome: "passed".to_owned(),
                dirty: false,
                ..Default::default()
            }),
            head: Some("feedface00".to_owned()),
            head_check: Some(Check::Failed {
                why: "error: tree mismatch".to_owned(),
            }),
        };
        left.landed = vec![
            Landed {
                commit: "c0ffee1234".to_owned(),
                subject: "pushed".to_owned(),
                onto: "origin/main".to_owned(),
                check: Some(Check::Unverified {
                    why: "sykli has no --commit".to_owned(),
                }),
                evidence: None,
                at_tip: false,
                unattributed: false,
            },
            Landed {
                commit: "abcdef1234".to_owned(),
                subject: "checked".to_owned(),
                onto: "origin/main".to_owned(),
                check: Some(Check::Passed),
                evidence: None,
                at_tip: true,
                unattributed: false,
            },
        ];
        s.left = Some(left);
        let record = render_record(&s);
        for said in [
            "finished      https://github.com/o/r/pull/7 (open) · head feedfac: FAILED sykli verify: error: tree mismatch",
            "landed        c0ffee1 reached origin/main: unverified: sykli has no --commit",
            "landed        abcdef1 reached origin/main: verified: passed (sykli verify --commit)",
        ] {
            assert!(record.contains(said), "missing `{said}`: {record}");
        }
        let last = render_last(&s, &s.repo);
        assert!(
            last.contains("finished: https://github.com/o/r/pull/7 (open), head feedfac FAILED"),
            "{last}"
        );
        assert!(
            last.contains("- c0ffee1 reached origin/main: unverified: sykli has no --commit"),
            "{last}"
        );
        // A record written before ohjaa asked sykli says its receipt was not verified.
        let old: Finished = serde_json::from_str(
            r#"{"kind":"pull_request","url":"u","state":"MERGED","head_evidence":{"outcome":"passed","dirty":false}}"#,
        )
        .unwrap();
        assert_eq!(
            describe_finished(&old),
            "u (merged) · its head, unverified: receipt: passed"
        );
    }

    #[test]
    fn a_record_is_flagged_only_where_sykli_refused_the_commit() {
        let pr = |state: &str, check: Check| Finished::PullRequest {
            url: "u".to_owned(),
            state: state.to_owned(),
            head_evidence: None,
            head: Some("feedface00".to_owned()),
            head_check: Some(check),
        };
        let landed = |check: Check, at_tip: bool| Landed {
            commit: "c0ffee1234".to_owned(),
            subject: "s".to_owned(),
            onto: "origin/main".to_owned(),
            check: Some(check),
            evidence: None,
            at_tip,
            unattributed: false,
        };
        let with = |finished: Finished, landed: Vec<Landed>| {
            let mut s = ended(session("oh-12", "/r/selko", 1_000), 2_000);
            let left = s.left.as_mut().unwrap();
            left.finished = finished;
            left.landed = landed;
            s
        };
        let failed = || Check::Failed {
            why: "refused: outcome expected passed".to_owned(),
        };
        let headlines = |s: &Session| flags(s).into_iter().map(|f| f.headline).collect::<Vec<_>>();
        // What could not be checked is recorded, never flagged (unchecked
        // only where it is not the branch's tip: see below).
        let superseded = with(
            Finished::NoPullRequest,
            vec![landed(Check::Unchecked, false)],
        );
        assert!(headlines(&superseded).is_empty());
        for quiet in [
            Check::Passed,
            Check::Unverified {
                why: "sykli has no --commit".to_owned(),
            },
            Check::Stale {
                why: "refused: tree expected a but got b".to_owned(),
            },
        ] {
            let s = with(pr("OPEN", quiet.clone()), vec![landed(quiet.clone(), true)]);
            assert!(headlines(&s).is_empty(), "{quiet:?}");
            assert!(
                render_warnings(Some(&s), &[], "/r/selko", |_, _, _| false).is_empty(),
                "{quiet:?}"
            );
        }
        assert!(
            flags(&with(pr("CLOSED", failed()), Vec::new())).is_empty(),
            "a closed pull request reaches nothing"
        );
        assert_eq!(
            headlines(&with(pr("OPEN", failed()), vec![landed(failed(), true)])),
            [
                "origin/main at c0ffee1 in selko has no passing evidence — check before building on it",
                "pull request u at feedfac has no passing evidence — check before merging or building on it",
            ]
        );
        assert_eq!(
            headlines(&with(Finished::NoPullRequest, vec![landed(failed(), false)])),
            ["c0ffee1 reached origin/main in selko without passing evidence — check origin/main before building on it"],
            "\"at\" only of the commit the branch sits at"
        );
        let s = with(pr("MERGED", failed()), Vec::new());
        assert!(render_warnings(Some(&s), &[], "/r/selko", |_, _, _| false)
            .starts_with("WARNING from the last session here (oh-12, claude"));

        // Once the branch has moved on, the briefing no longer says it is at
        // the flagged commit; the last session's flag is still said.
        let moved = with(Finished::NoPullRequest, vec![landed(failed(), true)]);
        let said = render_warnings(Some(&moved), &[], "/r/selko", |_, _, _| false);
        assert!(
            said.contains(
                "- c0ffee1 reached origin/main in selko without passing evidence; origin/main \
                 has moved on since — check what is on it before building on it ("
            ),
            "{said}"
        );
        assert!(!said.contains("origin/main at c0ffee1"), "{said}");
        let still = render_warnings(Some(&moved), &[], "/r/selko", |_, _, _| true);
        assert!(
            still.contains("- origin/main at c0ffee1 in selko has no passing evidence"),
            "{still}"
        );

        // A landing not shown to be the session's is never asked of sykli,
        // and its record says why, not that only a push's tip is.
        let mut theirs = landed(Check::Passed, true);
        theirs.check = None;
        theirs.unattributed = true;
        let record = render_record(&with(Finished::NoPullRequest, vec![theirs]));
        assert!(
            record.contains(
                "c0ffee1 reached origin/main (not attributed): no sykli receipt (not verified: \
                 not shown to be this session's)"
            ),
            "{record}"
        );
    }

    #[test]
    fn a_broken_tip_of_main_is_flagged_whoever_put_it_there() {
        let landed = |check: Check, onto: &str, at_tip: bool, unattributed: bool| Landed {
            commit: "c0ffee1234".to_owned(),
            subject: "s".to_owned(),
            onto: onto.to_owned(),
            check: Some(check),
            evidence: None,
            at_tip,
            unattributed,
        };
        let with = |landed: Landed| {
            let mut s = ended(session("oh-12", "/r/selko", 1_000), 2_000);
            s.left.as_mut().unwrap().landed = vec![landed];
            s
        };
        let failed = || Check::Failed {
            why: "refused: outcome expected passed".to_owned(),
        };
        let said = |s: &Session| {
            flags(s)
                .into_iter()
                .map(|f| (f.headline, f.own))
                .collect::<Vec<_>>()
        };
        let state = "origin/main at c0ffee1 in selko has no passing evidence (observed, not \
                     attributed) — check before building on it";
        // Refused, or never run where there is a contract: the tip of main is
        // broken whoever pushed it; only the session's own refusal is a lesson.
        assert_eq!(
            said(&with(landed(failed(), "origin/main", true, true))),
            [(state.to_owned(), false)]
        );
        assert_eq!(
            said(&with(landed(Check::Unchecked, "origin/main", true, true))),
            [(state.to_owned(), false)]
        );
        assert_eq!(
            said(&with(landed(Check::Unchecked, "origin/main", true, false))),
            [(
                "origin/main at c0ffee1 in selko has no passing evidence — check before \
                 building on it"
                    .to_owned(),
                false
            )]
        );
        assert!(said(&with(landed(failed(), "origin/main", true, false)))[0].1);
        // What sykli could not say, a commit main has moved on from, and a
        // local branch nobody else builds on raise nothing unless the session's
        // own commit was refused.
        for quiet in [
            landed(
                Check::Unverified {
                    why: "sykli has no --commit".to_owned(),
                },
                "origin/main",
                true,
                true,
            ),
            landed(
                Check::Unverified {
                    why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned(),
                },
                "origin/main",
                true,
                false,
            ),
            landed(
                Check::Stale {
                    why: "refused: tree expected a but got b".to_owned(),
                },
                "origin/main",
                true,
                true,
            ),
            landed(failed(), "origin/main", false, true),
            landed(failed(), "main", true, true),
            landed(Check::Unchecked, "main", true, false),
        ] {
            assert!(said(&with(quiet.clone())).is_empty(), "{quiet:?}");
        }
        // Two records of one broken tip say it once.
        let mut theirs = with(landed(failed(), "origin/main", true, false));
        theirs.id = "oh-13".to_owned();
        let warnings = render_warnings(
            Some(&with(landed(failed(), "origin/main", true, true))),
            &[theirs],
            "/r/selko",
            |_, _, _| true,
        );
        assert_eq!(warnings.matches("c0ffee1").count(), 1, "{warnings}");
    }

    #[test]
    fn session_ids_are_names_not_paths() {
        assert!(is_valid_id(&new_id(1_790_000_000_000)));
        for bad in ["", "../x", "a/b", "a b", ".", &"x".repeat(65)] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }

    /// A validation is said as recorded, and a pending one (a run id, no
    /// receipt) as sykli says it now: asked only for that one.
    #[test]
    fn validations_are_references_and_only_a_pending_one_is_asked_about() {
        let base = ValidationRef {
            request: "rq-1-2".to_owned(),
            run: None,
            kind: ValidationKind::Candidate,
            repo: "/r/a".to_owned(),
            commit: "c0mmit0000".to_owned(),
            tree: "7ree000000".to_owned(),
            contract: "/h/sykli.json".to_owned(),
            contract_digest: None,
            lock_digest: None,
            requested_ms: 1,
            receipt: None,
            common: None,
            observed_branch: None,
            ran: None,
            not_dispatched: None,
        };
        let receipt = ReceiptRef {
            path: "/r/a/.sykli/receipts/rcpt_abc.json".to_owned(),
            digest: Some("0123456789abcdef".to_owned()),
        };
        let asked = std::cell::RefCell::new(Vec::new());
        let said = |v: &ValidationRef| {
            asked.borrow_mut().push(v.request.clone());
            Some(Said {
                text: "sykli says run r9 is running: 1 running".to_owned(),
                receipt: None,
                ended: false,
            })
        };
        let unknown = base.clone();
        let foreground = ValidationRef {
            request: "rq-2-2".to_owned(),
            receipt: Some(ReceiptRef {
                path: "/r/a/.sykli/receipt.json".to_owned(),
                digest: None,
            }),
            ..base.clone()
        };
        let done = ValidationRef {
            request: "rq-3-2".to_owned(),
            run: Some("r8".to_owned()),
            kind: ValidationKind::Integrated,
            receipt: Some(receipt),
            ..base.clone()
        };
        let pending = ValidationRef {
            request: "rq-4-2".to_owned(),
            run: Some("r9".to_owned()),
            ..base
        };
        assert!(pending.pending() && !done.pending() && !unknown.pending());
        let lines = render_validations(&[unknown, foreground, done, pending], "- ", &said);
        assert_eq!(
            lines,
            "- validation rq-1-2 (candidate, c0mmit0 tree 7ree000): no sykli run id; no receipt \
             recorded\n\
             - validation rq-2-2 (candidate, c0mmit0 tree 7ree000): no sykli run id; receipt \
             /r/a/.sykli/receipt.json (no digest)\n\
             - validation rq-3-2 (integrated, c0mmit0 tree 7ree000): sykli run r8; receipt \
             /r/a/.sykli/receipts/rcpt_abc.json (sha256 0123456789ab)\n\
             - validation rq-4-2 (candidate, c0mmit0 tree 7ree000): sykli says run r9 is \
             running: 1 running\n"
        );
        assert_eq!(
            *asked.borrow(),
            ["rq-4-2"],
            "only the pending one is asked about"
        );
    }
}
