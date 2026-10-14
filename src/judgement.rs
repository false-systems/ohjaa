//! Independent Judgement (faija-ontology.v2 *Judgement*; law 2: the Agent
//! never judges its own Change), KATANA D5 and D8.
//!
//! `ohjaa judge` records a Judgement of a Change (`base..head`) and its exact
//! State (the head's tree) in the session log's `judgements` table, which
//! stays authoritative for the write:
//!
//! - who made the Change: its commits' authors (name and email), and the
//!   ohjaa sessions the log attributes them to;
//! - the reviewer, as observed: the live session whose process group holds
//!   the judging process (or its ancestors), whatever its environment says;
//!   outside every one, the person git names here;
//! - the Intent, the evidence considered (sykli's run and receipt references
//!   from `sykli status --commit`, never a verdict copied from sykli), the
//!   verdict, findings, severity, blocking, uncertainty, time and provenance;
//! - two independence facts, kept apart: the judge's, procedural, which is
//!   established only under the rule [`JudgeIndependence`] states (and said
//!   not to be, with each reason, otherwise), and the evidence's, which is
//!   not established today and is said so.
//!
//! A reviewer shown to have made a commit of the Change is refused, and
//! nothing is recorded. Any other is recorded, and never claims more than
//! was shown: ohjaa cannot tell a person from an agent, and a reflog is the
//! agent's own testimony. A Judgement is never changed or removed; an answer
//! (`ohjaa judge respond`) is a record of its own beside it.
//!
//! After recording, the Judgement is attached to its head as a git note under
//! `refs/notes/ohjaa/judgements`: one compact, canonical JSON line, in a
//! notes commit by `ohjaa <ohjaa@invalid>` with no time. Notes are append-
//! only: a push publishes the union of this clone's lines, origin's and the
//! session log's, never what git's notes merge would choose, and two lines
//! claiming one id are a conflict `ohjaa judgements` shows. Notes are refs,
//! never files, hooks or config. They leave the machine only with `--push`
//! (or the entry's `push_judgements = true`), only for an entry that shares
//! them (its `sharing.toml`): a private one by default, a public one (or one
//! that does not say) only with `share_judgements = true`; and the notes of
//! any entry not private name no one, at the tip or anywhere in the history
//! a push would publish.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::gitconfig;
use crate::home::{self, Entry, RepoId, Visibility};
use crate::intake::{now_seconds, rfc3339};
use crate::lifecycle;
use crate::process;
use crate::sessions::{self, ReceiptRef, Session};

/// Where Judgements are shared, as git notes on the heads they judge.
pub const NOTES_REF: &str = "refs/notes/ohjaa/judgements";
/// Where another clone's shared notes are fetched to (`--fetch`, `--push`).
pub const FETCHED_REF: &str = "refs/notes/ohjaa/fetched/judgements";
pub const SCHEMA: &str = "ohjaa-judgement.v1";
pub const RESPONSE_SCHEMA: &str = "ohjaa-judgement-response.v1";
pub const NOTE_SCHEMA: &str = "ohjaa-judgement-note.v1";
pub const RESPONSE_NOTE_SCHEMA: &str = "ohjaa-judgement-response-note.v1";
pub const LIST_SCHEMA: &str = "ohjaa-judgements.v1";

/// The most commits a Change records; a longer one says it was cut.
const MAX_COMMITS: usize = 200;
/// Recorded, but not shared: `--push` did not reach origin.
const NOT_SHARED: i32 = 1;
const CANNOT: i32 = 2;
const REFUSED: i32 = 3;
/// Not recorded: the session log could not be read or written (busy, or
/// broken). Never 1, which says it was recorded.
const NOT_RECORDED: i32 = 4;

/// How long `ohjaa judge` waits for a busy session log, and for this clone's
/// notes, while other ohjaa write them: a person is waiting, and the
/// Judgement must not be lost to a moment's contention.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(20);

/// The identity of every notes commit ohjaa writes: no person's name, email
/// or time zone, and no time (the line itself carries its own). A notes
/// commit is published with the ref, whatever the line says.
const NEUTRAL: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "ohjaa"),
    ("GIT_AUTHOR_EMAIL", "ohjaa@invalid"),
    ("GIT_AUTHOR_DATE", "946684800 +0000"),
    ("GIT_COMMITTER_NAME", "ohjaa"),
    ("GIT_COMMITTER_EMAIL", "ohjaa@invalid"),
    ("GIT_COMMITTER_DATE", "946684800 +0000"),
];
const NEUTRAL_IDENT: &str = "ohjaa <ohjaa@invalid> 946684800 +0000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    Reject,
}

impl Verdict {
    fn words(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::RequestChanges => "request changes",
            Verdict::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Minor,
    Major,
    Critical,
}

/// A git identity, as a commit or `git var` names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    fn shown(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }

    /// The same identity: the same email (in any case), or the same name.
    fn same(&self, other: &Identity) -> bool {
        (!self.email.is_empty() && self.email.eq_ignore_ascii_case(&other.email))
            || (!self.name.is_empty() && self.name == other.name)
    }
}

/// Who acts: a Person by their git identity, or an Agent by the ohjaa
/// session it runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    Person { name: String, email: String },
    Agent { session: String, agent: String },
}

impl Actor {
    fn shown(&self) -> String {
        match self {
            Actor::Person { name, email } => format!("person {name} <{email}>"),
            Actor::Agent { session, agent } => format!("agent session {session} ({agent})"),
        }
    }
}

/// One commit of a Change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCommit {
    pub commit: String,
    pub tree: String,
    pub author: Identity,
    pub committer: Identity,
    /// Its commit time, unix seconds.
    pub committed: i64,
    pub subject: String,
    /// The ohjaa session the log attributes it to (a phase of its record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// The Change judged, and its exact State.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub base: String,
    pub head: String,
    /// The head's tree: the exact State judged.
    pub state: String,
    /// Newest first, as `git log base..head` lists them.
    pub commits: Vec<ChangeCommit>,
    /// More than [`MAX_COMMITS`]: the oldest are left out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// An ohjaa session that made commits of the Change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorSession {
    pub session: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    pub commits: usize,
}

/// Who made the Change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authors {
    /// Its commits' authors, `Name <email>`, each once.
    pub people: Vec<String>,
    /// The ended ohjaa sessions whose records hold its commits.
    pub sessions: Vec<AuthorSession>,
    /// Commits no ended session's record holds: made outside ohjaa, or by a
    /// session still open (named in `open`).
    pub unattributed: usize,
    /// Sessions open in this clone now, which may have made those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open: Vec<String>,
}

/// One piece of evidence considered: sykli's run, and its receipt, by
/// reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ReceiptRef>,
}

/// The evidence considered: what sykli was asked, and the references it
/// named; never its verdict, which is asked again when shown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<EvidenceRef>,
    /// Why sykli was not asked, or could not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_asked: Option<String>,
}

/// The judge's independence of the authors: procedural. `established` only
/// when every commit of the Change is held by an ended session's record,
/// none of them the reviewer's; the judging process runs inside no live
/// session but the reviewer's own (its process group and its ancestors');
/// and `OHJAA_SESSION` is unset (a person, whose git identity is on no
/// commit of the Change) or names the reviewer's own session. Otherwise not,
/// with each reason: ohjaa cannot tell a person from an agent, and a reflog
/// is the agent's own testimony, so it never claims more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeIndependence {
    pub established: bool,
    pub kind: String,
    /// What was shown.
    pub basis: Vec<String>,
    /// Why it is not established; empty when it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_established: Vec<String>,
}

/// Whether the evidence was produced independently of the authors: not
/// established by anything ohjaa or sykli does today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceIndependence {
    pub established: bool,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Independence {
    pub judge: JudgeIndependence,
    pub evidence: EvidenceIndependence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// `ohjaa <version>`.
    pub recorded_by: String,
    /// The session it was recorded from, for an Agent reviewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// stdin and stdout were a terminal.
    pub terminal: bool,
    /// `OHJAA_SESSION` as the judging process was given it: testimony.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_env: Option<String>,
    /// The live sessions the judging process was observed to run inside
    /// (its process group, or an ancestor's, or their ohjaa an ancestor of
    /// it), innermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inside: Vec<String>,
}

/// A Judgement of a Change and its exact State.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judgement {
    pub schema: String,
    pub id: String,
    /// The checkout it was recorded in, and its clone's common git dir.
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common: Option<String>,
    /// `owner/name`, from origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub change: Change,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    pub authors: Authors,
    pub reviewer: Actor,
    pub evidence: Evidence,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    pub at_ms: i64,
    pub independence: Independence,
    pub provenance: Provenance,
}

/// An answer to a Judgement: a record of its own, beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub schema: String,
    pub id: String,
    pub judgement: String,
    /// The head the Judgement judged: where its note goes.
    pub head: String,
    pub by: Actor,
    /// The responder is one of the Judgement's authors (its session, or an
    /// identity on its commits).
    pub author: bool,
    pub text: String,
    pub at_ms: i64,
}

// ---- reading and writing the log -----------------------------------------------

fn log_path() -> Result<PathBuf> {
    Ok(sessions::store_path(
        &crate::knowledge::state_home()?.join("ohjaa"),
    ))
}

/// Every Judgement and answer in the log, and every session (for who made
/// what). One short read; rows that cannot be read are skipped and named.
struct Logged {
    sessions: Vec<Session>,
    judgements: Vec<Judgement>,
    responses: Vec<Response>,
    unreadable: Vec<String>,
}

fn read_log() -> Result<Logged> {
    let reader =
        sessions::Reader::open(&log_path()?).map_err(|error| anyhow::anyhow!("{error}"))?;
    let (sessions, mut unreadable) = reader.all().map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut judgements = Vec::new();
    for (id, bytes) in reader
        .rows(sessions::JUDGEMENTS)
        .map_err(|error| anyhow::anyhow!("{error}"))?
    {
        match serde_json::from_slice(&bytes) {
            Ok(judgement) => judgements.push(judgement),
            Err(_) => unreadable.push(id),
        }
    }
    let mut responses = Vec::new();
    for (id, bytes) in reader
        .rows(sessions::RESPONSES)
        .map_err(|error| anyhow::anyhow!("{error}"))?
    {
        match serde_json::from_slice(&bytes) {
            Ok(response) => responses.push(response),
            Err(_) => unreadable.push(id),
        }
    }
    judgements.sort_by_key(|j: &Judgement| j.at_ms);
    responses.sort_by_key(|r: &Response| r.at_ms);
    Ok(Logged {
        sessions,
        judgements,
        responses,
        unreadable,
    })
}

/// Every Judgement and answer in the log, for the briefing and the report:
/// empty where the log cannot be read (said by the caller).
pub fn logged() -> Result<(Vec<Judgement>, Vec<Response>)> {
    let logged = read_log()?;
    Ok((logged.judgements, logged.responses))
}

// ---- git -----------------------------------------------------------------------

fn git(top: &Path, args: &[&str], input: Option<&[u8]>) -> Result<process::Output, String> {
    git_with(top, args, input, &[])
}

/// [`git`], with `env` set: [`NEUTRAL`] for a notes commit.
fn git_with(
    top: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    env: &[(&str, &str)],
) -> Result<process::Output, String> {
    let mut command = Command::new("git");
    // git's own words, whatever the locale: some are read (`fetch`).
    command.arg("-C").arg(top).args(args).env("LC_ALL", "C");
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    process::run(&mut command, input, process::LOCAL).map_err(|error| format!("{error:#}"))
}

fn git_ok(top: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let output = git(top, args, input)?;
    if output.status.success() {
        Ok(output.stdout_text())
    } else {
        Err(output.failure_detail())
    }
}

/// `Name <email> <time> <zone>` (`git var GIT_COMMITTER_IDENT`): the
/// person acting here, as git would name them in a commit.
fn identity_here(top: &Path) -> Result<Identity, String> {
    let line = git_ok(top, &["var", "GIT_COMMITTER_IDENT"], None)?;
    let line = line.trim();
    let (name, rest) = line
        .split_once(" <")
        .ok_or_else(|| format!("git names no identity here ({line})"))?;
    let email = rest.split_once('>').map_or("", |(email, _)| email);
    Ok(Identity {
        name: name.trim().to_owned(),
        email: email.to_owned(),
    })
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The repository here: its checkout, its clone's common dir, origin's
/// `owner/name` and the entry for it (one git process: its config).
struct Here {
    top: PathBuf,
    common: PathBuf,
    id: Option<RepoId>,
    entry: Option<Entry>,
}

fn here() -> Result<Here> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = gitconfig::repo_at(&cwd)?;
    let listing = gitconfig::clone_listing(&top)?;
    let id = gitconfig::push_url(&listing, "origin")
        .as_deref()
        .and_then(RepoId::parse);
    let entry = match &id {
        Some(id) => home::find_entry(&home::absolute(home::ohjaa_home()?), id)?,
        None => None,
    };
    Ok(Here {
        top: canonical(&top),
        common: canonical(&common),
        id,
        entry,
    })
}

// ---- sharing -------------------------------------------------------------------

/// Whether a repository's Judgements leave this machine, and whether its
/// notes may name people (D8 point 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sharing {
    /// Pushed when asked (`--push`), or by the entry's `push_judgements`.
    pub shared: bool,
    /// The note holds names, emails, findings and paths: a private entry
    /// only. Any other holds ids, hashes and the verdict.
    pub names: bool,
    /// `push_judgements = true`.
    pub push: bool,
    /// Why, for people.
    pub why: String,
}

/// The sharing of an entry's Judgements: shared for a private one unless it
/// says `share_judgements = false`; for a public one, or one that does not
/// say, only with `share_judgements = true`. Names only where private.
pub fn sharing_of(entry: Option<&Entry>) -> Sharing {
    let Some(entry) = entry else {
        return Sharing {
            shared: false,
            names: false,
            push: false,
            why: "origin names no entry in the workflow home, so its visibility is unknown"
                .to_owned(),
        };
    };
    let said = entry.sharing;
    let names = said.visibility == Visibility::Private;
    let (shared, why) = match (said.visibility, said.share_judgements) {
        (Visibility::Private, None) => (true, "a private repository shares them".to_owned()),
        (_, Some(true)) => (
            true,
            format!(
                "its entry says share_judgements = true ({} visibility)",
                said.visibility.word()
            ),
        ),
        (_, Some(false)) => (false, "its entry says share_judgements = false".to_owned()),
        (Visibility::Public, None) => (
            false,
            "a public repository keeps them on this machine unless its entry says \
             share_judgements = true"
                .to_owned(),
        ),
        (Visibility::Unknown, None) => (
            false,
            "its entry does not say whether it is private (visibility = \"private\"), so it is \
             treated as public: they stay on this machine unless it says share_judgements = true"
                .to_owned(),
        ),
    };
    Sharing {
        shared,
        names,
        push: shared && said.push_judgements,
        why,
    }
}

// ---- notes ---------------------------------------------------------------------

/// `value` with every object's keys in order, at every depth: the same
/// Judgement is always the same bytes, whatever serde_json keeps.
fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), canonical_json(&map[key]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

/// `in-<ms>-<pid>`, as ohjaa mints an Intent id: nothing else is one.
fn intent_id_shaped(id: &str) -> bool {
    id.strip_prefix("in-")
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(ms, pid)| {
            !ms.is_empty()
                && !pid.is_empty()
                && ms.chars().all(|c| c.is_ascii_digit())
                && pid.chars().all(|c| c.is_ascii_digit())
        })
}

/// What a person may safely see of a text from a note or the log: every
/// control character (a terminal escape among them) as `?`.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The agent's program name, never the path it was run by.
fn program(agent: &str) -> String {
    Path::new(agent).file_name().map_or_else(
        || agent.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The note line of a Judgement: compact, canonical JSON. With `names`
/// (a private repository) it says who and what; without, only ids, hashes,
/// the verdict and counts.
pub fn note_line(judgement: &Judgement, names: bool) -> String {
    let evidence: Vec<Value> = judgement
        .evidence
        .refs
        .iter()
        .map(|r| {
            json!({
                "run": r.run,
                "digest": r.receipt.as_ref().and_then(|receipt| receipt.digest.clone()),
            })
        })
        .collect();
    let mut note = json!({
        "schema": NOTE_SCHEMA,
        "id": judgement.id,
        "base": judgement.change.base,
        "head": judgement.change.head,
        "state": judgement.change.state,
        "verdict": judgement.verdict,
        "blocking": judgement.blocking,
        "severity": judgement.severity,
        "intent_id": judgement.intent_id.as_deref().filter(|id| intent_id_shaped(id)),
        "at": rfc3339(judgement.at_ms.div_euclid(1000)),
        "evidence": evidence,
        "independence": {
            "judge": if judgement.independence.judge.established { "procedural" } else { "not_established" },
            "evidence": if judgement.independence.evidence.established { "established" } else { "not_established" },
        },
    });
    let sessions: Vec<&str> = judgement
        .authors
        .sessions
        .iter()
        .map(|s| s.session.as_str())
        .collect();
    if names {
        note["reviewer"] = match &judgement.reviewer {
            Actor::Agent { session, agent } => {
                json!({"kind": "agent", "session": session, "agent": program(agent)})
            }
            person => serde_json::to_value(person).unwrap_or(Value::Null),
        };
        note["authors"] = json!({"people": judgement.authors.people, "sessions": sessions});
        note["findings"] = json!(judgement.findings);
        note["uncertainty"] = json!(judgement.uncertainty);
        note["slug"] = json!(judgement.slug);
    } else {
        note["reviewer"] = match &judgement.reviewer {
            Actor::Agent { session, .. } => json!({"kind": "agent", "session": session}),
            Actor::Person { .. } => json!({"kind": "person"}),
        };
        note["authors"] = json!({"people": judgement.authors.people.len(), "sessions": sessions});
        note["findings"] = json!(judgement.findings.len());
        note["redacted"] = json!(true);
    }
    canonical_json(&note).to_string()
}

/// The note line of an answer, as [`note_line`] says one.
pub fn response_line(response: &Response, names: bool) -> String {
    let mut note = json!({
        "schema": RESPONSE_NOTE_SCHEMA,
        "id": response.id,
        "judgement": response.judgement,
        "head": response.head,
        "author": response.author,
        "at": rfc3339(response.at_ms.div_euclid(1000)),
    });
    if names {
        note["by"] = serde_json::to_value(&response.by).unwrap_or(Value::Null);
        note["text"] = json!(response.text);
    } else {
        note["by"] = match &response.by {
            Actor::Agent { session, .. } => json!({"kind": "agent", "session": session}),
            Actor::Person { .. } => json!({"kind": "person"}),
        };
        note["redacted"] = json!(true);
    }
    canonical_json(&note).to_string()
}

/// Attach `line` to `head`'s note under [`NOTES_REF`] (one git process), as
/// a notes commit by [`NEUTRAL`]. The caller holds this clone's notes lock
/// (see [`notes_lock`]): two writers never lose each other's line.
fn attach(top: &Path, head: &str, line: &str) -> Result<(), String> {
    let output = git_with(
        top,
        &["notes", "--ref", NOTES_REF, "append", "-F", "-", head],
        Some(format!("{line}\n").as_bytes()),
        &NEUTRAL,
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(output.failure_detail())
    }
}

/// This clone's lock on its notes ref (`$XDG_STATE_HOME/ohjaa/notes/
/// <clone>.lock`): every write of ohjaa's to the ref, and every push of it,
/// holds it. Outside the repository, like everything else ohjaa keeps.
fn notes_lock(common: &Path) -> Result<process::FileLock, String> {
    let state = crate::knowledge::state_home()
        .map_err(|error| format!("{error:#}"))?
        .join("ohjaa");
    process::lock_file(
        &state
            .join("notes")
            .join(format!("{}.lock", crate::integrated::clone_id(common))),
        PATIENCE,
    )
}

/// Notes by the object they annotate: each note's non-empty lines.
type Notes = BTreeMap<String, Vec<String>>;

/// Each `refs`' notes (none for a ref that does not exist): `git notes list`
/// per ref, then every note in one `cat-file --batch`.
fn notes_of(top: &Path, refs: &[&str]) -> Result<Vec<Notes>, String> {
    let mut listed: Vec<Vec<(String, String)>> = Vec::new();
    for reference in refs {
        let output = git(top, &["notes", "--ref", reference, "list"], None)?;
        listed.push(if output.status.success() {
            output
                .stdout_text()
                .lines()
                .filter_map(|line| {
                    let (blob, object) = line.split_once(' ')?;
                    Some((blob.to_owned(), object.trim().to_owned()))
                })
                .collect()
        } else {
            Vec::new()
        });
    }
    let wanted: BTreeSet<&str> = listed
        .iter()
        .flatten()
        .map(|(blob, _)| blob.as_str())
        .collect();
    let read = blobs(top, wanted.into_iter())?;
    Ok(listed
        .into_iter()
        .map(|notes| {
            notes
                .into_iter()
                .map(|(blob, object)| (object, read.get(&blob).cloned().unwrap_or_default()))
                .collect()
        })
        .collect())
}

/// The non-empty lines of each blob asked for, by its id (one `cat-file
/// --batch`); none for one that is missing or not a blob.
fn blobs<'a>(
    top: &Path,
    oids: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, Vec<String>>, String> {
    let asked: Vec<&str> = oids.collect();
    if asked.is_empty() {
        return Ok(HashMap::new());
    }
    let input = format!("{}\n", asked.join("\n"));
    let output = git(top, &["cat-file", "--batch"], Some(input.as_bytes()))?;
    if !output.status.success() {
        return Err(format!(
            "the notes cannot be read: {}",
            output.failure_detail()
        ));
    }
    // `<oid> <type> <size>\n<content>\n`, or `<oid> missing\n`.
    let bytes = &output.stdout;
    let mut read = HashMap::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(end) = bytes[at..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&bytes[at..at + end]).into_owned();
        at += end + 1;
        let fields: Vec<&str> = header.split_whitespace().collect();
        let [oid, kind, size] = fields[..] else {
            continue;
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("the notes cannot be read: {header}"))?;
        let content = bytes
            .get(at..at + size)
            .ok_or_else(|| "the notes were cut short".to_owned())?;
        if kind == "blob" {
            read.insert(
                oid.to_owned(),
                String::from_utf8_lossy(content)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .collect(),
            );
        }
        at += size + 1;
    }
    Ok(read)
}

/// A full object id: what a notes tree may be keyed by here.
fn object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Write `notes` as one notes commit by [`NEUTRAL`] on [`NOTES_REF`] (one
/// `git fast-import`): exactly these notes, with `parents` (none: a new
/// history; `force` then lets it replace the ref's). Notes are written as
/// paths, so a note on a commit this clone has not fetched is kept as it is.
fn write_notes(
    top: &Path,
    notes: &Notes,
    parents: &[&str],
    force: bool,
    message: &str,
) -> Result<(), String> {
    let mut stream: Vec<u8> = Vec::new();
    let _ = write!(
        stream,
        "commit {NOTES_REF}\ncommitter {NEUTRAL_IDENT}\ndata {}\n{message}\n",
        message.len() + 1
    );
    for (n, parent) in parents.iter().enumerate() {
        if !object_id(parent) {
            return Err(format!("{parent} is not a commit id"));
        }
        let _ = writeln!(stream, "{} {parent}", if n == 0 { "from" } else { "merge" });
    }
    stream.extend_from_slice(b"deleteall\n");
    for (object, lines) in notes {
        if !object_id(object) || lines.is_empty() {
            continue;
        }
        let text = format!("{}\n", lines.join("\n"));
        let _ = write!(
            stream,
            "M 100644 inline {object}\ndata {}\n{text}\n",
            text.len()
        );
    }
    stream.extend_from_slice(b"done\n");
    let mut args = vec!["fast-import", "--quiet", "--done"];
    if force {
        args.push("--force");
    }
    let output = git(top, &args, Some(&stream))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "the notes could not be written: {}",
            output.failure_detail()
        ))
    }
}

/// Every line in `notes`, sorted, each once per object: the order git's
/// `cat_sort_uniq` keeps.
fn tidy(notes: &Notes) -> Notes {
    notes
        .iter()
        .filter_map(|(object, lines)| {
            let lines: BTreeSet<&String> = lines.iter().collect();
            (!lines.is_empty()).then(|| (object.clone(), lines.into_iter().cloned().collect()))
        })
        .collect()
}

/// The union of `sides`, line by line, per object: no line of any side is
/// dropped, and two different lines are both kept, whatever they claim.
fn union(sides: &[&Notes]) -> Notes {
    let mut all: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for side in sides {
        for (object, lines) in side.iter() {
            all.entry(object.clone())
                .or_default()
                .extend(lines.iter().cloned());
        }
    }
    all.into_iter()
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(object, lines)| (object, lines.into_iter().collect()))
        .collect()
}

/// The note lines this machine's log holds for the clone whose common dir
/// is `common` (`names`: as a private entry's notes say them), by the head
/// each is attached to: every Judgement recorded there, and every answer to
/// one of them.
fn logged_lines(logged: &Logged, common: &Path, names: bool) -> Notes {
    let common = common.to_string_lossy();
    let mut notes = Notes::new();
    let here: Vec<&Judgement> = logged
        .judgements
        .iter()
        .filter(|j| j.common.as_deref() == Some(&*common))
        .collect();
    for judgement in &here {
        notes
            .entry(judgement.change.head.clone())
            .or_default()
            .push(note_line(judgement, names));
    }
    for response in &logged.responses {
        if here.iter().any(|j| j.id == response.judgement) {
            notes
                .entry(response.head.clone())
                .or_default()
                .push(response_line(response, names));
        }
    }
    tidy(&notes)
}

/// Push [`NOTES_REF`] to origin, and nothing else, as the union of this
/// clone's notes, origin's (fetched into [`FETCHED_REF`] first) and this
/// machine's log: no line here or in the log is ever dropped, whatever git's
/// three-way notes merge would choose (a line changed on one side only wins
/// there); two lines claiming one id are both kept, and shown as a conflict
/// by `ohjaa judgements`. The commit pushed has origin's as a parent, so the
/// push never rewrites origin's. For an entry that is not private, nothing
/// that is not redacted is published, at the tip or anywhere in the history
/// the push would send ([`public_check`]). The caller holds the notes lock.
fn push(here: &Here, names: bool, logged: &Logged) -> Result<String, String> {
    let mut tries = 0;
    loop {
        tries += 1;
        fetch(&here.top)?;
        let local_tip = gitconfig::read_ref(&here.common, NOTES_REF);
        let fetched_tip = gitconfig::read_ref(&here.common, FETCHED_REF);
        let read = notes_of(&here.top, &[NOTES_REF, FETCHED_REF])?;
        let (local, fetched) = (tidy(&read[0]), tidy(&read[1]));
        let ours = logged_lines(logged, &here.common, names);
        let on_origin: BTreeSet<&str> = fetched.values().flatten().map(String::as_str).collect();
        if !names {
            public_check(
                &here.top,
                local_tip.as_deref(),
                fetched_tip.as_deref(),
                &[&local, &ours],
                &on_origin,
            )?;
        }
        let merged = union(&[&local, &fetched, &ours]);
        let count = |notes: &Notes| notes.values().map(Vec::len).sum::<usize>();
        let from_origin = count(&merged) - count(&union(&[&local, &ours]));
        let from_log = count(&union(&[&local, &ours])) - count(&local);
        let behind = match (&local_tip, &fetched_tip) {
            (_, None) => false,
            (Some(local), Some(fetched)) if local == fetched => false,
            (Some(local), Some(fetched)) => !git(
                &here.top,
                &["merge-base", "--is-ancestor", fetched, local],
                None,
            )?
            .status
            .success(),
            (None, Some(_)) => true,
        };
        if merged != local || behind {
            let mut parents: Vec<&str> = local_tip.iter().map(String::as_str).collect();
            if behind {
                parents.extend(fetched_tip.as_deref());
            }
            write_notes(
                &here.top,
                &merged,
                &parents,
                false,
                "ohjaa: Judgements, the union of this clone's notes, origin's and the session log",
            )?;
        }
        let tip = gitconfig::read_ref(&here.common, NOTES_REF);
        if tip.is_none() || tip == fetched_tip {
            return Ok(format!(
                "origin already holds every line of {NOTES_REF}: nothing to push"
            ));
        }
        let output = git(
            &here.top,
            &["push", "-q", "origin", &format!("{NOTES_REF}:{NOTES_REF}")],
            None,
        )?;
        if output.status.success() {
            let lines = count(&merged);
            return Ok(format!(
                "pushed {NOTES_REF} to origin: {lines} line(s) on {} commit(s), the union of \
                 this clone's, origin's ({from_origin} new here) and the session log's \
                 ({from_log} re-attached); none dropped",
                merged.len()
            ));
        }
        let detail = output.failure_detail();
        // Origin moved between the fetch and the push: once more.
        if tries < 2 && (detail.contains("rejected") || detail.contains("fetch first")) {
            continue;
        }
        return Err(format!("git push origin {NOTES_REF} failed: {detail}"));
    }
}

/// Whether a note line may be published for an entry that is not private:
/// exactly what [`note_line`] or [`response_line`] write without names, and
/// nothing more. A line that only says `"redacted": true` is not enough.
pub fn publishable(line: &str) -> bool {
    let Ok(Value::Object(note)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let only = |value: &Value, keys: &[&str]| {
        value
            .as_object()
            .is_some_and(|map| map.keys().all(|key| keys.contains(&key.as_str())))
    };
    let hex = |value: &Value| {
        value
            .as_str()
            .is_some_and(|text| !text.is_empty() && text.chars().all(|c| c.is_ascii_hexdigit()))
    };
    let token = |value: &Value| {
        value.as_str().is_some_and(|text| {
            !text.is_empty()
                && text.len() <= 128
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    };
    let id = |value: &Value, prefix: &str| {
        value.as_str().is_some_and(|text| {
            text.strip_prefix(prefix)
                .and_then(|rest| rest.split_once('-'))
                .is_some_and(|(a, b)| {
                    !a.is_empty()
                        && !b.is_empty()
                        && a.chars().all(|c| c.is_ascii_digit())
                        && b.chars().all(|c| c.is_ascii_digit())
                })
        })
    };
    let at = |value: &Value| {
        value.as_str().is_some_and(|text| {
            text.len() <= 32
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z'))
        })
    };
    let actor = |value: &Value| match value["kind"].as_str() {
        Some("person") => only(value, &["kind"]),
        Some("agent") => {
            only(value, &["kind", "session"])
                && sessions::is_valid_id(value["session"].as_str().unwrap_or(""))
        }
        _ => false,
    };
    let note = Value::Object(note);
    if note["redacted"] != true {
        return false;
    }
    match note["schema"].as_str() {
        Some(NOTE_SCHEMA) => {
            only(
                &note,
                &[
                    "at",
                    "authors",
                    "base",
                    "blocking",
                    "evidence",
                    "findings",
                    "head",
                    "id",
                    "independence",
                    "intent_id",
                    "redacted",
                    "reviewer",
                    "schema",
                    "severity",
                    "state",
                    "verdict",
                ],
            ) && id(&note["id"], "jd-")
                && hex(&note["base"])
                && hex(&note["head"])
                && hex(&note["state"])
                && at(&note["at"])
                && note["blocking"].is_boolean()
                && matches!(
                    note["verdict"].as_str(),
                    Some("approve" | "request_changes" | "reject")
                )
                && (note["severity"].is_null()
                    || matches!(
                        note["severity"].as_str(),
                        Some("info" | "minor" | "major" | "critical")
                    ))
                && (note["intent_id"].is_null()
                    || note["intent_id"].as_str().is_some_and(intent_id_shaped))
                && note["findings"].is_u64()
                && actor(&note["reviewer"])
                && only(&note["authors"], &["people", "sessions"])
                && note["authors"]["people"].is_u64()
                && note["authors"]["sessions"].as_array().is_some_and(|ids| {
                    ids.iter()
                        .all(|id| id.as_str().is_some_and(sessions::is_valid_id))
                })
                && note["evidence"].as_array().is_some_and(|refs| {
                    refs.iter().all(|r| {
                        only(r, &["run", "digest"])
                            && (r["run"].is_null() || token(&r["run"]))
                            && (r["digest"].is_null() || hex(&r["digest"]))
                    })
                })
                && only(&note["independence"], &["judge", "evidence"])
                && matches!(
                    note["independence"]["judge"].as_str(),
                    Some("procedural" | "not_established")
                )
                && matches!(
                    note["independence"]["evidence"].as_str(),
                    Some("established" | "not_established")
                )
        }
        Some(RESPONSE_NOTE_SCHEMA) => {
            only(
                &note,
                &[
                    "at",
                    "author",
                    "by",
                    "head",
                    "id",
                    "judgement",
                    "redacted",
                    "schema",
                ],
            ) && id(&note["id"], "rs-")
                && id(&note["judgement"], "jd-")
                && hex(&note["head"])
                && at(&note["at"])
                && note["author"].is_boolean()
                && actor(&note["by"])
        }
        _ => false,
    }
}

/// Before a push of an entry that is not private: nothing it would publish
/// names anyone. Every line of `tips` (this clone's notes, and the lines the
/// log re-attaches) must be [`publishable`] or on origin already; and so
/// must every line of every note in the history the push would send (each
/// notes commit reachable from this clone's tip and not from origin's), each
/// of those commits by [`NEUTRAL`]. A note removed with `git notes remove`
/// is still in that history. Two git processes (`log --raw`, `cat-file`).
fn public_check(
    top: &Path,
    local_tip: Option<&str>,
    fetched_tip: Option<&str>,
    tips: &[&Notes],
    on_origin: &BTreeSet<&str>,
) -> Result<(), String> {
    let named = |line: &&String| !on_origin.contains(line.as_str()) && !publishable(line);
    let at_tip = tips
        .iter()
        .flat_map(|notes| notes.values().flatten())
        .filter(named)
        .count();
    let (mut in_history, mut by_people) = (0, 0);
    if let Some(local_tip) = local_tip {
        let mut args = vec![
            "log",
            "--format=%x1e%H%x1f%an%x1f%ae%x1f%ad%x1f%cn%x1f%ce%x1f%cd",
            "--date=raw",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            "-m",
            "--root",
            local_tip,
        ];
        if let Some(fetched_tip) = fetched_tip {
            args.extend(["--not", fetched_tip]);
        }
        let text = git_ok(top, &args, None)
            .map_err(|why| format!("the history of {NOTES_REF} cannot be read: {why}"))?;
        let mut introduced: BTreeSet<String> = BTreeSet::new();
        for line in text.lines() {
            if let Some(header) = line.strip_prefix('\u{1e}') {
                let fields: Vec<&str> = header.split('\u{1f}').collect();
                let neutral = fields.len() == 7
                    && fields[1] == "ohjaa"
                    && fields[2] == "ohjaa@invalid"
                    && fields[3].ends_with(" +0000")
                    && fields[4] == "ohjaa"
                    && fields[5] == "ohjaa@invalid"
                    && fields[6].ends_with(" +0000");
                if !neutral {
                    by_people += 1;
                }
            } else if let Some(raw) = line.strip_prefix(':') {
                // `:<mode> <mode> <old> <new> <status>\t<path>`
                if let Some(new) = raw.split_whitespace().nth(3) {
                    if !new.chars().all(|c| c == '0') {
                        introduced.insert(new.to_owned());
                    }
                }
            }
        }
        in_history = blobs(top, introduced.iter().map(String::as_str))?
            .values()
            .flatten()
            .filter(named)
            .count();
    }
    if at_tip + in_history + by_people == 0 {
        return Ok(());
    }
    Err(format!(
        "{NOTES_REF} may name people, so it stays on this machine: {at_tip} line(s) at its tip \
         and {in_history} in the history a push would publish are not redacted, and \
         {by_people} of its commit(s) carry a person's name or time zone (written while the \
         entry was private, by git itself, or not by ohjaa). A note removed with `git notes \
         remove` is still in that history. `ohjaa judgements --rebuild-public` rewrites this \
         clone's notes ref as one commit of redacted lines with no history; the session log \
         keeps every Judgement"
    ))
}

/// Fetch origin's shared notes into [`FETCHED_REF`] (one git process).
/// Origin having none is no failure.
fn fetch(top: &Path) -> Result<(), String> {
    let output = git(
        top,
        &[
            "fetch",
            "-q",
            "origin",
            &format!("+{NOTES_REF}:{FETCHED_REF}"),
        ],
        None,
    )?;
    if output.status.success() {
        return Ok(());
    }
    let detail = output.failure_detail();
    if detail.contains("couldn't find remote ref") {
        return Ok(());
    }
    Err(format!("git fetch origin {NOTES_REF} failed: {detail}"))
}

/// The non-empty lines of `commit`'s note under `reference`, as written.
fn note_lines(top: &Path, reference: &str, commit: &str) -> Vec<String> {
    let Ok(text) = git_ok(top, &["notes", "--ref", reference, "show", commit], None) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

// ---- the Change ----------------------------------------------------------------

/// `base..head`, or `head` alone (its base: where it leaves origin's copy of
/// the entry's branch).
fn split_range(range: &str) -> (Option<&str>, &str) {
    match range.split_once("..") {
        Some((base, head)) => (
            Some(base).filter(|base| !base.is_empty()),
            if head.is_empty() { "HEAD" } else { head },
        ),
        None => (None, range),
    }
}

/// Resolve the Change's ends: the head, its tree, and the base (one git
/// process; a second, `merge-base`, when the base is not named).
fn ends(here: &Here, range: &str) -> Result<(String, String, String), String> {
    let (base, head) = split_range(range);
    let mut asked = format!("{head}^{{commit}}\n{head}^{{tree}}\n");
    if let Some(base) = base {
        asked.push_str(&format!("{base}^{{commit}}\n"));
    }
    let output = git_ok(
        &here.top,
        &["cat-file", "--batch-check=%(objectname)"],
        Some(asked.as_bytes()),
    )?;
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    // An object id, never `<rev> missing` or `<rev> ambiguous`.
    let oid = |n: usize, what: &str| -> Result<String, String> {
        lines
            .get(n)
            .filter(|line| line.len() >= 40 && line.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|line| (*line).to_owned())
            .ok_or_else(|| format!("{what} cannot be resolved here"))
    };
    let head_oid = oid(0, head)?;
    let tree = oid(1, &format!("{head}'s tree"))?;
    let base_oid = match base {
        Some(base) => oid(2, base)?,
        None => {
            let Some(branch) = here.entry.as_ref().map(|entry| entry.branch.clone()) else {
                return Err(
                    "origin names no entry, so there is no observed branch to judge from: name \
                     the base, ohjaa judge <base>..<head>"
                        .to_owned(),
                );
            };
            git_ok(
                &here.top,
                &[
                    "merge-base",
                    &head_oid,
                    &format!("refs/remotes/origin/{branch}"),
                ],
                None,
            )
            .map(|base| base.trim().to_owned())
            .map_err(|why| format!("where {head} leaves origin/{branch} cannot be found: {why}"))?
        }
    };
    Ok((base_oid, head_oid, tree))
}

/// Every commit of `base..head`, newest first (one git process): law 2 is
/// checked over all of them; the record keeps [`MAX_COMMITS`].
fn commits(top: &Path, base: &str, head: &str) -> Result<Vec<ChangeCommit>, String> {
    let text = git_ok(
        top,
        &[
            "log",
            "--format=%H%x1f%T%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%ct%x1f%s",
            &format!("{base}..{head}"),
            "--",
        ],
        None,
    )?;
    let mut out = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.splitn(8, '\u{1f}').collect();
        let [commit, tree, an, ae, cn, ce, ct, subject] = fields[..] else {
            continue;
        };
        out.push(ChangeCommit {
            commit: commit.to_owned(),
            tree: tree.to_owned(),
            author: Identity {
                name: an.to_owned(),
                email: ae.to_owned(),
            },
            committer: Identity {
                name: cn.to_owned(),
                email: ce.to_owned(),
            },
            committed: ct.parse().unwrap_or(0),
            subject: subject.to_owned(),
            session: None,
        });
    }
    Ok(out)
}

/// Every phase commit an ended session's record holds, to its session.
fn attributions(all: &[Session]) -> HashMap<String, &Session> {
    let mut made = HashMap::new();
    for session in all {
        let Some(left) = &session.left else {
            continue;
        };
        let scoped = left
            .scoped
            .iter()
            .filter(|s| !s.found)
            .flat_map(|s| s.phases.iter().flatten());
        for phase in left.phases.iter().flatten().chain(scoped) {
            made.entry(phase.commit.clone()).or_insert(session);
        }
    }
    made
}

/// Who made the Change, from its commits and the log; each commit takes
/// the session the log attributes it to.
fn authors_of(
    change: &mut Change,
    all: &[Session],
    clone: &[PathBuf],
) -> (Authors, Option<String>) {
    let made = attributions(all);
    let mut people: Vec<String> = Vec::new();
    let mut sessions: Vec<AuthorSession> = Vec::new();
    let mut unattributed = 0;
    for commit in &mut change.commits {
        let shown = commit.author.shown();
        if !people.contains(&shown) {
            people.push(shown);
        }
        match made.get(&commit.commit) {
            Some(session) => {
                commit.session = Some(session.id.clone());
                match sessions.iter_mut().find(|s| s.session == session.id) {
                    Some(known) => known.commits += 1,
                    None => sessions.push(AuthorSession {
                        session: session.id.clone(),
                        agent: program(&session.agent),
                        intent_id: session.intent_id.clone(),
                        commits: 1,
                    }),
                }
            }
            None => unattributed += 1,
        }
    }
    // Sessions open now in a checkout of this clone: they may have made
    // what no record holds yet.
    let open: Vec<String> = all
        .iter()
        .filter(|s| s.left.is_none() && clone.iter().any(|c| Path::new(&s.repo) == c))
        .map(|s| s.id.clone())
        .collect();
    let intents: BTreeSet<&String> = sessions
        .iter()
        .filter_map(|s| s.intent_id.as_ref())
        .collect();
    let intent = (intents.len() == 1)
        .then(|| intents.into_iter().next().cloned())
        .flatten();
    (
        Authors {
            people,
            sessions,
            unattributed,
            open: if unattributed > 0 { open } else { Vec::new() },
        },
        intent,
    )
}

// ---- law 2 ---------------------------------------------------------------------

/// A commit of the Change an open reviewing session may have made, and why
/// it is the session's to answer for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    pub commit: String,
    /// Open sessions launched in the reviewer's checkout that were there
    /// too when it was made: who made it cannot be shown.
    pub shared_with: Vec<String>,
}

/// The commits of the Change an open reviewing `session` made, from the
/// reflogs (`own`: its checkout's HEAD; `branches`: the clone's branches),
/// newest first as git lists them. A commit the log gives to another
/// session is that session's: an ended session's record is the authority on
/// what it made, never the clock (sessions start and end in whole seconds).
/// Otherwise:
/// - one its own checkout shows created since it started is its own: a
///   reviewer must be shown not to have made a commit, and in a checkout it
///   shared with another open session that cannot be shown (said, with whom);
/// - one a branch shows created since it started, in another checkout, is
///   its own unless another session in the clone was there that second: an
///   open one by the clock, an ended one by its record (it saw the commit
///   made beside it, `observed`).
pub fn reviewer_made(
    session: &Session,
    change: &Change,
    all: &[Session],
    clone: &[PathBuf],
    own: &[lifecycle::Move],
    branches: &[lifecycle::Move],
) -> Vec<Made> {
    let wanted: HashMap<&str, Option<&str>> = change
        .commits
        .iter()
        .map(|c| (c.commit.as_str(), c.session.as_deref()))
        .collect();
    let since = session.started_ms.div_euclid(1000);
    let own_checkout = canonical(Path::new(&session.repo));
    let in_clone = |s: &&Session| clone.iter().any(|c| Path::new(&s.repo) == c);
    let others: Vec<&Session> = all.iter().filter(|s| s.id != session.id).collect();
    // Open sessions' time so far: their records do not say yet.
    let now = now_ms() + 999;
    let open: Vec<&Session> = others
        .iter()
        .copied()
        .filter(|s| s.left.is_none())
        .filter(in_clone)
        .collect();
    let live = |s: &Session, at: i64| s.started_ms <= at * 1000 + 999 && now >= at * 1000;
    // What ended sessions saw made beside them, and could not credit.
    let seen_beside: BTreeSet<&str> = others
        .iter()
        .filter_map(|s| s.left.as_ref())
        .flat_map(|left| {
            left.observed
                .iter()
                .chain(left.scoped.iter().flat_map(|scoped| scoped.observed.iter()))
        })
        .map(|phase| phase.commit.as_str())
        .collect();
    let mut made: Vec<Made> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut consider = |entry: &lifecycle::Move, in_own: bool| {
        if entry.at < since || !lifecycle::created_here(&entry.subject) {
            return;
        }
        let Some(attributed) = wanted.get(entry.commit.as_str()) else {
            return;
        };
        if attributed.is_some_and(|id| id != session.id) || !seen.insert(entry.commit.clone()) {
            return;
        }
        if in_own {
            let shared_with = open
                .iter()
                .filter(|s| canonical(Path::new(&s.repo)) == own_checkout && live(s, entry.at))
                .map(|s| s.id.clone())
                .collect();
            made.push(Made {
                commit: entry.commit.clone(),
                shared_with,
            });
            return;
        }
        let contested =
            seen_beside.contains(entry.commit.as_str()) || open.iter().any(|s| live(s, entry.at));
        if !contested {
            made.push(Made {
                commit: entry.commit.clone(),
                shared_with: Vec::new(),
            });
        }
    };
    for entry in own {
        consider(entry, true);
    }
    for entry in branches
        .iter()
        .filter(|e| e.reference.starts_with("refs/heads/"))
    {
        consider(entry, false);
    }
    made
}

/// The reflogs [`reviewer_made`] reads, each once: every branch of the
/// clone, and each session's checkout's HEAD.
struct Reflogs<'a> {
    top: &'a Path,
    branches: Option<Vec<lifecycle::Move>>,
    own: HashMap<PathBuf, Vec<lifecycle::Move>>,
}

impl<'a> Reflogs<'a> {
    fn new(top: &'a Path) -> Self {
        Reflogs {
            top,
            branches: None,
            own: HashMap::new(),
        }
    }

    /// [`reviewer_made`] for `session`, its reflogs read: two git processes
    /// the first time (its checkout's HEAD, and every branch of the clone),
    /// one for another session's checkout after.
    fn made(
        &mut self,
        session: &Session,
        change: &Change,
        all: &[Session],
        clone: &[PathBuf],
    ) -> Result<Vec<Made>, String> {
        let own_checkout = canonical(Path::new(&session.repo));
        if !self.own.contains_key(&own_checkout) {
            let own = lifecycle::reflog(
                &own_checkout,
                &[
                    "log",
                    "--walk-reflogs",
                    "--date=unix",
                    lifecycle::REFLOG_FORMAT,
                    "HEAD",
                ],
            )
            .ok_or_else(|| format!("the reflog of {} cannot be read", own_checkout.display()))?;
            self.own.insert(own_checkout.clone(), own);
        }
        if self.branches.is_none() {
            self.branches = Some(
                lifecycle::reflog(
                    self.top,
                    &[
                        "reflog",
                        "show",
                        "--all",
                        "--date=unix",
                        lifecycle::REFLOG_FORMAT,
                    ],
                )
                .ok_or_else(|| "the clone's branch reflogs cannot be read".to_owned())?,
            );
        }
        Ok(reviewer_made(
            session,
            change,
            all,
            clone,
            &self.own[&own_checkout],
            self.branches.as_deref().unwrap_or_default(),
        ))
    }
}

/// The live sessions (open in the log) that the process `chain` (this one
/// and its ancestors, nearest first, from the process table `table`) runs
/// inside, innermost first: one whose recorded process group holds a
/// process of the chain, or whose ohjaa (the pid in its id) is one of them.
/// A group or pid the table shows started after the session did is another
/// process, the id reused (two seconds of slack for the clock tick).
pub fn sessions_around<'a>(
    all: &'a [Session],
    table: &[process::Proc],
    chain: &[process::Proc],
    now_s: i64,
) -> Vec<&'a Session> {
    let began_by = |p: &process::Proc, at_ms: i64| now_s - p.elapsed <= at_ms.div_euclid(1000) + 2;
    let mut found: Vec<(usize, &Session)> = Vec::new();
    for session in all.iter().filter(|s| s.left.is_none()) {
        let by_group = session
            .pgid
            .filter(|group| {
                table.iter().find(|p| p.pid == *group).is_none_or(|leader| {
                    began_by(
                        leader,
                        session.group_started_ms.unwrap_or(session.started_ms),
                    )
                })
            })
            .and_then(|group| chain.iter().position(|p| p.pgid == group));
        let by_ohjaa = sessions::pid_of(&session.id).and_then(|pid| {
            chain
                .iter()
                .position(|p| p.pid == pid && began_by(p, session.started_ms))
        });
        if let Some(at) = by_group.into_iter().chain(by_ohjaa).min() {
            found.push((at, session));
        }
    }
    found.sort_by_key(|(at, session)| (*at, std::cmp::Reverse(session.started_ms)));
    found.into_iter().map(|(_, session)| session).collect()
}

/// Who judges, as observed. The process, not its environment, decides: a
/// process inside a live session's process group (or below its ohjaa) is
/// that session's, whatever `OHJAA_SESSION` says or omits; outside every
/// one, `OHJAA_SESSION` names the session it claims (not shown), and without
/// it the reviewer is the person git names here. ohjaa cannot tell a person
/// from an agent running outside every ohjaa session.
struct Observed {
    reviewer: Reviewer,
    /// Every live session the process runs inside, innermost first.
    inside: Vec<Session>,
    /// `OHJAA_SESSION`, as given.
    claimed: Option<String>,
    /// The session `OHJAA_SESSION` names, when it is not the reviewer.
    claimed_other: Option<Session>,
    /// Why where the process runs could not be seen.
    unseen: Option<String>,
}

enum Reviewer {
    /// `shown`: the process runs inside its process group.
    Agent {
        session: Box<Session>,
        shown: bool,
    },
    Person(Identity),
}

/// Observe who judges: `ps` once, then the log's live sessions. Err: a
/// refusal (an `OHJAA_SESSION` that names no live session in the log, or no
/// identity git can name).
fn observe(top: &Path, all: &[Session]) -> Result<Observed, String> {
    let claimed = std::env::var(sessions::SESSION_ENV).ok();
    let claimed_session = match &claimed {
        Some(id) => {
            if !sessions::is_valid_id(id) {
                return Err(format!(
                    "{} is not a session id: `{id}`",
                    sessions::SESSION_ENV
                ));
            }
            let session = all.iter().find(|s| &s.id == id).cloned().ok_or_else(|| {
                format!("session {id} is not in the session log, so what it made cannot be shown")
            })?;
            if session.left.is_some() {
                return Err(format!(
                    "session {id} has ended; a Judgement is recorded from a live session (or by a \
                     person, outside one)"
                ));
            }
            Some(session)
        }
        None => None,
    };
    let (inside, unseen) = match process::table() {
        Ok(table) => {
            let chain = process::ancestry(&table);
            let around = sessions_around(all, &table, &chain, now_ms().div_euclid(1000));
            (around.into_iter().cloned().collect::<Vec<Session>>(), None)
        }
        Err(why) => (Vec::new(), Some(why)),
    };
    let reviewer = match (inside.first(), &claimed_session) {
        (Some(innermost), _) => Reviewer::Agent {
            session: Box::new(innermost.clone()),
            shown: true,
        },
        (None, Some(session)) => Reviewer::Agent {
            session: Box::new(session.clone()),
            shown: false,
        },
        (None, None) => Reviewer::Person(identity_here(top)?),
    };
    let claimed_other = match (&reviewer, claimed_session) {
        (Reviewer::Agent { session, .. }, Some(claimed)) if claimed.id != session.id => {
            Some(claimed)
        }
        _ => None,
    };
    Ok(Observed {
        reviewer,
        inside,
        claimed,
        claimed_other,
        unseen,
    })
}

/// Law 2, for every session the judging process is observed or claims to
/// be: refused when one of them made a commit of the Change (`made`, see
/// [`reviewer_made`]), or the log gives one to it. Ok when none did.
fn refuse_own_change(
    observed: &Observed,
    change: &Change,
    all: &[Session],
    reflogs: &mut Reflogs<'_>,
    clone: &[PathBuf],
) -> Result<(), String> {
    let mut asked: Vec<(&Session, &'static str)> = Vec::new();
    if let Reviewer::Agent { session, .. } = &observed.reviewer {
        asked.push((session, "this session"));
    }
    for session in &observed.inside {
        asked.push((session, "a session this process runs inside"));
    }
    if let Some(session) = &observed.claimed_other {
        asked.push((session, "the session OHJAA_SESSION names"));
    }
    let mut seen = BTreeSet::new();
    for (session, which) in asked {
        if !seen.insert(session.id.clone()) {
            continue;
        }
        let made = reflogs
            .made(session, change, all, clone)
            .map_err(|why| format!("what {} made cannot be read: {why}", session.id))?;
        if let Some(why) = made_refusal(session, which, change, &made) {
            return Err(why);
        }
    }
    Ok(())
}

/// Why `session` (`which` it is, to the judging process) may not judge the
/// Change: the commits of it the log gives to it, or it made (`made`, see
/// [`reviewer_made`]). `None` when there are none.
fn made_refusal(session: &Session, which: &str, change: &Change, made: &[Made]) -> Option<String> {
    let theirs: Vec<&str> = change
        .commits
        .iter()
        .filter(|c| {
            c.session.as_deref() == Some(session.id.as_str())
                || made.iter().any(|m| m.commit == c.commit)
        })
        .map(|c| short(&c.commit))
        .collect();
    if theirs.is_empty() {
        return None;
    }
    let shared: BTreeSet<&str> = made
        .iter()
        .flat_map(|m| m.shared_with.iter().map(String::as_str))
        .collect();
    Some(format!(
        "{} of the Change ({}) {} made by {which}, {}{}: the Agent never judges its own Change",
        if theirs.len() == 1 {
            "a commit"
        } else {
            "commits"
        },
        theirs.join(", "),
        if theirs.len() == 1 { "was" } else { "were" },
        session.id,
        if shared.is_empty() {
            String::new()
        } else {
            format!(
                ", or in its checkout while {} shared it (who made it cannot be shown)",
                shared.into_iter().collect::<Vec<_>>().join(", ")
            )
        }
    ))
}

/// Law 2, for a Person: refused when their identity is on a commit of the
/// Change (as author or committer) that no ended session's record holds.
fn refuse_persons_own(person: &Identity, change: &Change) -> Result<(), String> {
    let own: Vec<&str> = change
        .commits
        .iter()
        .filter(|c| c.session.is_none() && (c.author.same(person) || c.committer.same(person)))
        .map(|c| short(&c.commit))
        .collect();
    if own.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} made {} of the Change ({}): no reviewer judges a Change they made",
        person.shown(),
        if own.len() == 1 {
            "a commit"
        } else {
            "commits"
        },
        own.join(", ")
    ))
}

/// The judge's procedural independence (see [`JudgeIndependence`]): what
/// was shown, and each reason it is not established. `identity`: the git
/// identity the reviewer acts under (a person's, or the agent's own).
fn judge_independence(
    observed: &Observed,
    change: &Change,
    authors: &Authors,
    identity: Option<&Identity>,
) -> JudgeIndependence {
    let mut basis = Vec::new();
    let mut not = Vec::new();
    let n = change.commits.len();
    // Who made it: the log's records of ended sessions, and nothing else.
    if authors.unattributed == 0 {
        let by: Vec<&str> = authors
            .sessions
            .iter()
            .map(|s| s.session.as_str())
            .collect();
        basis.push(format!(
            "every one of the Change's {n} commit(s) is held by the record of an ended session \
             ({}), none of them the reviewer",
            by.join(", ")
        ));
    } else {
        not.push(format!(
            "{} of the Change's {n} commit(s) no ended session's record holds (made outside \
             ohjaa, in another clone, or by a session still open{}): who made them is not shown",
            authors.unattributed,
            if authors.open.is_empty() {
                String::new()
            } else {
                format!(": {}", authors.open.join(", "))
            }
        ));
        if let Some(identity) = identity {
            let theirs = change
                .commits
                .iter()
                .filter(|c| {
                    c.session.is_none() && (c.author.same(identity) || c.committer.same(identity))
                })
                .count();
            if theirs > 0 {
                not.push(format!(
                    "{theirs} of those carry the git identity the reviewer acts under, {}",
                    identity.shown()
                ));
            }
        }
    }
    if let Some(why) = &observed.unseen {
        not.push(format!(
            "whether this process runs inside a live ohjaa session could not be seen ({why})"
        ));
    }
    match &observed.reviewer {
        Reviewer::Person(person) => {
            let under: Vec<&ChangeCommit> = change
                .commits
                .iter()
                .filter(|c| c.author.same(person) || c.committer.same(person))
                .collect();
            if under.is_empty() {
                basis.push(format!(
                    "the reviewer is the person git names here, {}, on none of the Change's \
                     commits; ohjaa cannot tell a person from an agent running under that \
                     identity outside every ohjaa session",
                    person.shown()
                ));
            } else {
                let by: BTreeSet<&str> =
                    under.iter().filter_map(|c| c.session.as_deref()).collect();
                not.push(format!(
                    "{} of the Change's {n} commit(s) carry the reviewer's git identity, {} (made \
                     by agent session(s) {}, the log says): ohjaa cannot tell this person from \
                     an agent running under that identity",
                    under.len(),
                    person.shown(),
                    by.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
            if observed.unseen.is_none() {
                basis.push(
                    "this process runs inside no live ohjaa session (no process group or ohjaa \
                     of one among it and its ancestors, observed with ps), and OHJAA_SESSION is \
                     not set"
                        .to_owned(),
                );
            }
        }
        Reviewer::Agent { session, shown } => {
            basis.push(format!(
                "the reviewing session {} made none of the Change's {n} commit(s): its \
                 checkout's reflog and the clone's branches show none created by it, and the \
                 session log gives none to it (a reflog is the session's own record, which it \
                 can rewrite or expire)",
                session.id
            ));
            let others: Vec<&str> = observed
                .inside
                .iter()
                .filter(|s| s.id != session.id)
                .map(|s| s.id.as_str())
                .collect();
            match (&observed.claimed, *shown) {
                (None, true) => not.push(format!(
                    "OHJAA_SESSION is not set, yet this process runs inside live session {}'s \
                     process group: that session's agent, not shown to be a person",
                    session.id
                )),
                (Some(claimed), true) if *claimed != session.id => not.push(format!(
                    "OHJAA_SESSION names {claimed}, but this process runs inside session {}'s \
                     process group",
                    session.id
                )),
                (_, false) => not.push(format!(
                    "this process is not shown to run in session {}: it is in none of the live \
                     sessions' process groups, nor below their ohjaa",
                    session.id
                )),
                _ => {
                    if others.is_empty() {
                        basis.push(format!(
                            "this process runs inside session {}'s process group, as \
                             OHJAA_SESSION says, and inside no other live session's (observed \
                             with ps)",
                            session.id
                        ));
                    }
                }
            }
            if !others.is_empty() {
                not.push(format!(
                    "this process also runs inside live session(s) {} (a session started inside \
                     another): what those made is the reviewer's to answer for",
                    others.join(", ")
                ));
            }
            let agent = program(&session.agent);
            let same: Vec<&str> = authors
                .sessions
                .iter()
                .filter(|s| s.agent == agent)
                .map(|s| s.session.as_str())
                .collect();
            if !same.is_empty() {
                basis.push(format!(
                    "the reviewer runs the same agent ({agent}) as authoring session(s) {}: two \
                     instances of one agent are independent only procedurally",
                    same.join(", ")
                ));
            }
        }
    }
    JudgeIndependence {
        established: not.is_empty(),
        kind: "procedural".to_owned(),
        basis,
        not_established: not,
    }
}

/// Why the evidence is not shown to be independent of the authors.
fn evidence_independence(evidence: &Evidence) -> EvidenceIndependence {
    let why = if evidence.refs.is_empty() {
        "not established: no evidence was considered (sykli named no run or receipt of this \
         State)"
            .to_owned()
    } else {
        "not established: the evidence is sykli's runs and receipts on this machine, unsigned \
         and not reproduced by the reviewer; sykli verifies their schema, contract, source and \
         consistency, not their provenance, producer or execution"
            .to_owned()
    };
    EvidenceIndependence {
        established: false,
        why,
    }
}

// ---- evidence ------------------------------------------------------------------

/// What sykli keeps for the head: its run and receipt references, from
/// `sykli status --commit <head> --contract <gate> --json`. Never its
/// verdict: that is asked again when shown.
fn evidence_for(here: &Here, head: &str) -> Evidence {
    let contract = match &here.entry {
        Some(entry) if entry.has_contract() => home::absolute(entry.contract()),
        _ if here.top.join("sykli.json").is_file() => here.top.join("sykli.json"),
        _ => {
            return Evidence {
                not_asked: Some(
                    "no gate here: origin names no entry with one, and there is no sykli.json"
                        .to_owned(),
                ),
                ..Evidence::default()
            }
        }
    };
    let help = lifecycle::sykli_status_help();
    if !help.commit {
        return Evidence {
            not_asked: Some("this sykli cannot be asked (no `sykli status --commit`)".to_owned()),
            ..Evidence::default()
        };
    }
    let gate = lifecycle::Gate {
        slug: here.id.as_ref().map(RepoId::slug).unwrap_or_default(),
        branch: here
            .entry
            .as_ref()
            .map(|e| e.branch.clone())
            .unwrap_or_default(),
        contract: contract.clone(),
        top: here.top.clone(),
        common: here.common.clone(),
    };
    let asked = format!(
        "sykli status --commit {head} --contract {} --json",
        contract.display()
    );
    let status = match crate::integrated::commit_status(&gate, help, head) {
        Ok(status) => status,
        Err(why) => {
            return Evidence {
                asked: Some(asked),
                not_asked: Some(format!("sykli could not say: {why}")),
                ..Evidence::default()
            }
        }
    };
    let mut refs: Vec<EvidenceRef> = Vec::new();
    let mut add = |run: Option<&str>, path: Option<&str>, digest: Option<&str>| {
        let reference = EvidenceRef {
            run: run.map(str::to_owned),
            receipt: path.map(|path| ReceiptRef {
                path: path.to_owned(),
                digest: digest.map(str::to_owned),
            }),
        };
        let same = |r: &EvidenceRef| {
            r.run.is_some() && r.run == reference.run
                || r.receipt.as_ref().and_then(|r| r.digest.as_ref()).is_some()
                    && r.receipt.as_ref().and_then(|r| r.digest.as_ref())
                        == reference.receipt.as_ref().and_then(|r| r.digest.as_ref())
        };
        match refs.iter_mut().find(|r| same(r)) {
            Some(known) => {
                if known.receipt.is_none() {
                    known.receipt = reference.receipt;
                }
                if known.run.is_none() {
                    known.run = reference.run;
                }
            }
            None => refs.push(reference),
        }
    };
    for current in status["current"].as_array().map_or(&[][..], Vec::as_slice) {
        add(
            current["run"].as_str(),
            current["receipt"].as_str(),
            current["digest"].as_str(),
        );
    }
    for run in status["runs"].as_array().map_or(&[][..], Vec::as_slice) {
        add(
            run["run"].as_str(),
            run["receipt"]["path"].as_str(),
            run["receipt"]["digest"].as_str(),
        );
    }
    Evidence {
        asked: Some(asked),
        refs,
        not_asked: None,
    }
}

// ---- the commands --------------------------------------------------------------

/// What `ohjaa judge` was asked.
pub struct JudgeArgs<'a> {
    pub range: &'a str,
    pub verdict: Verdict,
    pub findings: &'a [String],
    pub severity: Option<Severity>,
    pub blocking: bool,
    pub uncertainty: Option<&'a str>,
    pub intent_id: Option<&'a str>,
    pub push: bool,
}

/// [`read_log`], waiting up to [`PATIENCE`] while writers hold the log.
fn read_log_patiently() -> Result<Logged> {
    let started = std::time::Instant::now();
    loop {
        match read_log() {
            Err(error) if error.to_string().contains("busy") && started.elapsed() < PATIENCE => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// Write a new row, waiting up to [`PATIENCE`] while writers hold the log
/// (each open already retries briefly).
fn insert_patiently(
    table: redb::TableDefinition<&str, &[u8]>,
    id: &str,
    bytes: &[u8],
    answers: Option<(redb::TableDefinition<&str, &[u8]>, &str)>,
) -> Result<(), sessions::StoreError> {
    let path = log_path().map_err(|error| sessions::StoreError::Other(format!("{error:#}")))?;
    let started = std::time::Instant::now();
    loop {
        match sessions::Store::open(&path) {
            Ok(store) => return store.insert_new(table, id, bytes, answers),
            Err(sessions::StoreError::Locked) if started.elapsed() < PATIENCE => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

/// The ids the process was observed inside, for the provenance.
fn inside_ids(observed: &Observed) -> Vec<String> {
    observed.inside.iter().map(|s| s.id.clone()).collect()
}

/// `ohjaa judge`: record a Judgement, attach its note, and push it when
/// asked. 0 recorded; 1 recorded, not shared; 2 nothing to judge; 3
/// refused (law 2, a session that is not live, or sharing); 4 not recorded.
pub fn judge(args: &JudgeArgs<'_>) -> Result<i32> {
    let here = match here() {
        Ok(here) => here,
        Err(error) => {
            eprintln!("ohjaa judge: not recorded: {error:#}");
            return Ok(NOT_RECORDED);
        }
    };
    let sharing = sharing_of(here.entry.as_ref());
    if args.push && !sharing.shared {
        eprintln!(
            "ohjaa judge: refused to share: {}. Nothing was recorded; judge without --push to \
             keep it on this machine, or set share_judgements = true in the entry's {}",
            sharing.why,
            home::SHARING_TOML
        );
        return Ok(REFUSED);
    }
    // An Intent id is an id (`in-<ms>-<pid>`): never free text, which a
    // note would carry where no name may go.
    if let Some(id) = args.intent_id {
        if !intent_id_shaped(id) {
            eprintln!("ohjaa judge: {id:?} is not an intent id (`in-…`)");
            return Ok(CANNOT);
        }
    }
    let logged = match read_log_patiently() {
        Ok(logged) => logged,
        Err(error) => {
            eprintln!("ohjaa judge: not recorded: the session log cannot be read ({error:#})");
            return Ok(NOT_RECORDED);
        }
    };
    let observed = match observe(&here.top, &logged.sessions) {
        Ok(observed) => observed,
        Err(why) => {
            eprintln!("ohjaa judge: refused: {why}");
            return Ok(REFUSED);
        }
    };
    let (base, head, state) = match ends(&here, args.range) {
        Ok(ends) => ends,
        Err(why) => {
            eprintln!("ohjaa judge: {why}");
            return Ok(CANNOT);
        }
    };
    let commits = match commits(&here.top, &base, &head) {
        Ok(found) => found,
        Err(why) => {
            eprintln!("ohjaa judge: the Change's commits cannot be read: {why}");
            return Ok(CANNOT);
        }
    };
    if commits.is_empty() {
        eprintln!(
            "ohjaa judge: nothing to judge: {}..{} holds no commit",
            short(&base),
            short(&head)
        );
        return Ok(CANNOT);
    }
    // Every commit of the Change is checked; the record keeps the newest.
    let mut change = Change {
        base,
        head,
        state,
        commits,
        truncated: false,
    };
    let clone = lifecycle::layout_checkouts(&here.common);
    let (authors, intent) = authors_of(&mut change, &logged.sessions, &clone);
    let refused = |why: String| {
        eprintln!("ohjaa judge: refused (law 2: the Agent never judges its own Change): {why}. Nothing was recorded.");
        Ok(REFUSED)
    };
    if let Reviewer::Person(person) = &observed.reviewer {
        if let Err(why) = refuse_persons_own(person, &change) {
            return refused(why);
        }
    }
    let mut reflogs = Reflogs::new(&here.top);
    if let Err(why) = refuse_own_change(&observed, &change, &logged.sessions, &mut reflogs, &clone)
    {
        return refused(why);
    }
    let (actor, session) = match &observed.reviewer {
        Reviewer::Person(person) => (
            Actor::Person {
                name: person.name.clone(),
                email: person.email.clone(),
            },
            None,
        ),
        Reviewer::Agent { session, .. } => (
            Actor::Agent {
                session: session.id.clone(),
                agent: program(&session.agent),
            },
            Some(session.id.clone()),
        ),
    };
    // The git identity the reviewer acts under, when it matters: a
    // person's is who they are; an agent's is asked only when some commit
    // no record holds might carry it.
    let identity = match &observed.reviewer {
        Reviewer::Person(person) => Some(person.clone()),
        Reviewer::Agent { .. } if authors.unattributed > 0 => identity_here(&here.top).ok(),
        Reviewer::Agent { .. } => None,
    };
    let independence = judge_independence(&observed, &change, &authors, identity.as_ref());
    if change.commits.len() > MAX_COMMITS {
        change.commits.truncate(MAX_COMMITS);
        change.truncated = true;
    }
    let evidence = evidence_for(&here, &change.head);
    let at_ms = now_ms();
    let judgement = Judgement {
        schema: SCHEMA.to_owned(),
        id: format!("jd-{at_ms}-{}", std::process::id()),
        repo: here.top.to_string_lossy().into_owned(),
        common: Some(here.common.to_string_lossy().into_owned()),
        slug: here.id.as_ref().map(RepoId::slug),
        intent_id: args.intent_id.map(str::to_owned).or(intent),
        authors,
        reviewer: actor,
        independence: Independence {
            judge: independence,
            evidence: evidence_independence(&evidence),
        },
        evidence,
        verdict: args.verdict,
        findings: args
            .findings
            .iter()
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty())
            .collect(),
        severity: args.severity,
        blocking: args.blocking,
        uncertainty: args
            .uncertainty
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
        at_ms,
        provenance: Provenance {
            recorded_by: format!("ohjaa {}", env!("CARGO_PKG_VERSION")),
            session,
            terminal: {
                use std::io::IsTerminal;
                std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
            },
            session_env: observed.claimed.clone(),
            inside: inside_ids(&observed),
        },
        change,
    };
    let bytes = serde_json::to_vec(&judgement)?;
    if let Err(error) = insert_patiently(sessions::JUDGEMENTS, &judgement.id, &bytes, None) {
        eprintln!("ohjaa judge: not recorded: the Judgement could not be recorded: {error}");
        return Ok(NOT_RECORDED);
    }
    println!(
        "ohjaa judge: recorded {} ({}) of {}..{} by {}",
        judgement.id,
        judgement.verdict.words(),
        short(&judgement.change.base),
        short(&judgement.change.head),
        judgement.reviewer.shown()
    );
    let judge = &judgement.independence.judge;
    if judge.established {
        println!("ohjaa judge: the judge's independence of the authors: procedural, established");
    } else {
        println!(
            "ohjaa judge: the judge's independence of the authors is not established: {}",
            judge.not_established.join("; ")
        );
    }
    let pushing = args.push || sharing.push;
    // Every write of the notes ref, and its push, under this clone's lock.
    let lock = match notes_lock(&here.common) {
        Ok(lock) => Some(lock),
        Err(why) => {
            eprintln!(
                "ohjaa judge: the note could not be attached ({why}); the log holds the \
                 Judgement, and a push re-attaches it"
            );
            None
        }
    };
    if lock.is_some() {
        match attach(
            &here.top,
            &judgement.change.head,
            &note_line(&judgement, sharing.names),
        ) {
            Ok(()) => println!(
                "ohjaa judge: attached to {} as a note under {NOTES_REF}{}",
                short(&judgement.change.head),
                if sharing.names {
                    ""
                } else {
                    " (ids, hashes and the verdict only)"
                }
            ),
            Err(why) => eprintln!(
                "ohjaa judge: the note could not be attached ({why}); the log holds the \
                 Judgement, and a push re-attaches it"
            ),
        }
    }
    if pushing {
        if lock.is_none() {
            eprintln!("ohjaa judge: not shared: this clone's notes are held by another ohjaa; it is recorded on this machine");
            return Ok(NOT_SHARED);
        }
        // The log as it is now: this Judgement in it.
        let mut logged = logged;
        logged.judgements.push(judgement.clone());
        match push(&here, sharing.names, &logged) {
            Ok(said) => println!("ohjaa judge: {said}"),
            Err(why) => {
                eprintln!("ohjaa judge: not shared: {why}; it is recorded on this machine");
                return Ok(NOT_SHARED);
            }
        }
    } else if sharing.shared {
        println!(
            "ohjaa judge: not pushed; share it with --push ({})",
            sharing.why
        );
    } else {
        println!("ohjaa judge: local only: {}", sharing.why);
    }
    drop(lock);
    Ok(0)
}

/// `ohjaa judge respond <judgement> <text>`: an answer beside a Judgement,
/// never a change to it.
pub fn respond(judgement: &str, text: &str) -> Result<i32> {
    let text = text.trim();
    if text.is_empty() {
        bail!("say what you answer");
    }
    let here = match here() {
        Ok(here) => here,
        Err(error) => {
            eprintln!("ohjaa judge respond: not recorded: {error:#}");
            return Ok(NOT_RECORDED);
        }
    };
    let logged = match read_log_patiently() {
        Ok(logged) => logged,
        Err(error) => {
            eprintln!(
                "ohjaa judge respond: not recorded: the session log cannot be read ({error:#})"
            );
            return Ok(NOT_RECORDED);
        }
    };
    let Some(judged) = logged.judgements.iter().find(|j| j.id == judgement) else {
        eprintln!("ohjaa judge respond: no Judgement {judgement} in this machine's session log");
        return Ok(CANNOT);
    };
    let observed = match observe(&here.top, &logged.sessions) {
        Ok(observed) => observed,
        Err(why) => {
            eprintln!("ohjaa judge respond: refused: {why}");
            return Ok(REFUSED);
        }
    };
    let (by, author) = match &observed.reviewer {
        Reviewer::Agent { session, .. } => {
            let author = judged
                .authors
                .sessions
                .iter()
                .any(|s| s.session == session.id);
            (
                Actor::Agent {
                    session: session.id.clone(),
                    agent: program(&session.agent),
                },
                author,
            )
        }
        Reviewer::Person(person) => {
            let author = judged
                .change
                .commits
                .iter()
                .any(|c| c.author.same(person) || c.committer.same(person));
            (
                Actor::Person {
                    name: person.name.clone(),
                    email: person.email.clone(),
                },
                author,
            )
        }
    };
    let at_ms = now_ms();
    let response = Response {
        schema: RESPONSE_SCHEMA.to_owned(),
        id: format!("rs-{at_ms}-{}", std::process::id()),
        judgement: judged.id.clone(),
        head: judged.change.head.clone(),
        by,
        author,
        text: text.to_owned(),
        at_ms,
    };
    let bytes = serde_json::to_vec(&response)?;
    if let Err(error) = insert_patiently(
        sessions::RESPONSES,
        &response.id,
        &bytes,
        Some((sessions::JUDGEMENTS, &response.judgement)),
    ) {
        eprintln!("ohjaa judge respond: not recorded: the answer could not be recorded: {error}");
        return Ok(NOT_RECORDED);
    }
    println!(
        "ohjaa judge respond: recorded {} beside {}; the Judgement is unchanged",
        response.id, response.judgement
    );
    let sharing = sharing_of(here.entry.as_ref());
    let attached = notes_lock(&here.common).and_then(|_lock| {
        attach(
            &here.top,
            &response.head,
            &response_line(&response, sharing.names),
        )
    });
    if let Err(why) = attached {
        eprintln!(
            "ohjaa judge respond: the note could not be attached ({why}); the log holds it, and \
             a push re-attaches it"
        );
    }
    Ok(0)
}

/// `ohjaa judgements --rebuild-public`: this clone's notes ref rewritten as
/// one commit by [`NEUTRAL`], with no history, holding only redacted lines:
/// every line of its notes that is [`publishable`], and this machine's
/// Judgements of the clone (and their answers) in their redacted form. Every
/// other line is dropped from the ref (the session log keeps this
/// machine's); the old history is no longer reachable from the ref, so a
/// push never publishes it (git's reflog keeps it on this machine). Nothing
/// is pushed.
pub fn rebuild_public() -> Result<i32> {
    let here = here()?;
    let _lock = notes_lock(&here.common).map_err(|why| anyhow::anyhow!("{why}"))?;
    let logged = read_log_patiently().context(
        "the session log cannot be read, so its Judgements could not be kept: nothing was rebuilt",
    )?;
    let read = notes_of(&here.top, &[NOTES_REF]).map_err(|why| anyhow::anyhow!("{why}"))?;
    let local = tidy(&read[0]);
    let count = |notes: &Notes| notes.values().map(Vec::len).sum::<usize>();
    let kept: Notes = local
        .iter()
        .map(|(object, lines)| {
            (
                object.clone(),
                lines.iter().filter(|l| publishable(l)).cloned().collect(),
            )
        })
        .collect();
    let dropped = count(&local) - count(&kept);
    let ours = logged_lines(&logged, &here.common, false);
    let rebuilt = union(&[&kept, &ours]);
    let added = count(&rebuilt) - count(&union(&[&kept]));
    if rebuilt.is_empty() && gitconfig::read_ref(&here.common, NOTES_REF).is_none() {
        println!("ohjaa judgements: {NOTES_REF} does not exist here, and the log holds no Judgement of this clone: nothing to rebuild");
        return Ok(0);
    }
    write_notes(
        &here.top,
        &rebuilt,
        &[],
        true,
        "ohjaa: Judgements, redacted, rebuilt with no history",
    )
    .map_err(|why| anyhow::anyhow!("{why}"))?;
    println!(
        "ohjaa judgements: rebuilt {NOTES_REF} as one commit by ohjaa <ohjaa@invalid> with no \
         history: {} redacted line(s) on {} commit(s) ({added} from this machine's session log); \
         {dropped} line(s) that were not redacted were dropped from the ref (this machine's \
         Judgements stay in its session log). The old history is no longer on the ref, so no \
         push publishes it (git's reflog keeps it on this machine only). Nothing was pushed: \
         the next `ohjaa judge … --push` pushes it",
        count(&rebuilt),
        rebuilt.len()
    );
    if sharing_of(here.entry.as_ref()).names {
        println!(
            "ohjaa judgements: this entry is private: its next push re-attaches its Judgements \
             with names"
        );
    }
    Ok(0)
}

/// A note line as a Judgement would read redacted: its names, words and
/// paths taken out, as [`note_line`] writes one without names.
fn redacted_form(value: &Value) -> Value {
    let mut note = value.clone();
    if note["redacted"] == true {
        return note;
    }
    let Some(map) = note.as_object_mut() else {
        return note;
    };
    let reviewer = match map.get("reviewer") {
        Some(r) if r["kind"] == "agent" => json!({"kind": "agent", "session": r["session"]}),
        Some(r) if r["kind"] == "person" => json!({"kind": "person"}),
        other => other.cloned().unwrap_or(Value::Null),
    };
    map.insert("reviewer".to_owned(), reviewer);
    if let Some(by) = map.get("by").cloned() {
        let by = match by["kind"].as_str() {
            Some("agent") => json!({"kind": "agent", "session": by["session"]}),
            _ => json!({"kind": "person"}),
        };
        map.insert("by".to_owned(), by);
    }
    if let Some(people) = map.get("authors").and_then(|a| a["people"].as_array()) {
        let sessions = map["authors"]["sessions"].clone();
        map.insert(
            "authors".to_owned(),
            json!({"people": people.len(), "sessions": sessions}),
        );
    }
    if let Some(findings) = map.get("findings").and_then(Value::as_array) {
        map.insert("findings".to_owned(), json!(findings.len()));
    }
    for key in ["uncertainty", "slug", "text"] {
        map.remove(key);
    }
    map.insert("redacted".to_owned(), json!(true));
    canonical_json(&note)
}

/// Two note lines say the same: the same bytes, or one is the other with
/// its names taken out.
fn agree(a: &Value, b: &Value) -> bool {
    a == b
        || redacted_form(a) == redacted_form(b) && (a["redacted"] == true || b["redacted"] == true)
}

/// One record of a Judgement or an answer, as `ohjaa judgements` lists it.
struct Listed {
    value: Value,
    /// The note lines that are this record (a log record: both its forms).
    lines: Vec<String>,
    /// `log`, `note` (this clone's ref, a line written from this machine's
    /// log), `merged` (this clone's ref, a line not from this machine's log:
    /// origin's, merged in by a push, or written another way), `fetched`
    /// (origin's notes as last fetched).
    sources: BTreeSet<&'static str>,
    /// From this machine's log: authoritative here. Any other is a claim.
    log: bool,
}

/// Where a note line read under `reference` came from, given whether this
/// machine's log holds it.
fn source_of(fetched: bool, from_log: bool) -> &'static str {
    match (fetched, from_log) {
        (true, _) => "fetched",
        (false, true) => "note",
        (false, false) => "merged",
    }
}

/// Add a note line read under a ref to `list`: to the record that is the
/// same line, else as a record of its own.
fn add_line(list: &mut Vec<Listed>, line: &str, value: Value, fetched: bool) {
    if let Some(known) = list.iter_mut().find(|l| l.lines.iter().any(|x| x == line)) {
        known.sources.insert(source_of(fetched, known.log));
        return;
    }
    list.push(Listed {
        value,
        lines: vec![line.to_owned()],
        sources: BTreeSet::from([source_of(fetched, false)]),
        log: false,
    });
}

/// The records as listed: each with where it was read, whether this
/// machine's log is its authority (`trust`: `log` or `foreign`), and whether
/// another record claims the same id and says something else (`conflict`).
fn listed_values(list: &[Listed]) -> Vec<Value> {
    list.iter()
        .map(|record| {
            let mut value = record.value.clone();
            let ordered: Vec<&str> = ["log", "note", "merged", "fetched"]
                .into_iter()
                .filter(|s| record.sources.contains(s))
                .collect();
            let note_of = |record: &Listed| -> Value {
                record
                    .lines
                    .first()
                    .and_then(|line| serde_json::from_str(line).ok())
                    .unwrap_or(Value::Null)
            };
            let mine = note_of(record);
            let conflict = list.iter().any(|other| {
                !std::ptr::eq(other, record)
                    && other.value["id"] == record.value["id"]
                    && !agree(&note_of(other), &mine)
            });
            value["sources"] = json!(ordered);
            value["trust"] = json!(if record.log { "log" } else { "foreign" });
            value["conflict"] = json!(conflict);
            value
        })
        .collect()
}

/// `ohjaa judgements <commit>`: every Judgement of it, from the log and the
/// notes (this clone's, and those fetched from origin), each marked with
/// where it was read, whether it is authoritative here, and whether another
/// record of the same id says otherwise.
pub fn list(commit: &str, json_out: bool, fetch_first: bool) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = gitconfig::repo_at(&cwd)?;
    if fetch_first {
        if let Err(why) = fetch(&top) {
            eprintln!("ohjaa judgements: {why}; the notes already here are read");
        }
    }
    let sha = match git_ok(
        &top,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{commit}^{{commit}}"),
        ],
        None,
    ) {
        Ok(sha) => sha.trim().to_owned(),
        Err(_) => {
            eprintln!("ohjaa judgements: {commit} is not a commit here");
            return Ok(CANNOT);
        }
    };
    let logged = match read_log() {
        Ok(logged) => logged,
        Err(error) => {
            eprintln!(
                "ohjaa judgements: the session log cannot be read ({error:#}); the notes only, \
                 none of them authoritative"
            );
            Logged {
                sessions: Vec::new(),
                judgements: Vec::new(),
                responses: Vec::new(),
                unreadable: Vec::new(),
            }
        }
    };
    let mut found: Vec<Listed> = Vec::new();
    let mut answers: Vec<Listed> = Vec::new();
    for judgement in &logged.judgements {
        if judgement.change.head == sha || judgement.change.commits.iter().any(|c| c.commit == sha)
        {
            found.push(Listed {
                value: serde_json::to_value(judgement)?,
                lines: vec![note_line(judgement, true), note_line(judgement, false)],
                sources: BTreeSet::from(["log"]),
                log: true,
            });
        }
    }
    for response in &logged.responses {
        if found
            .iter()
            .any(|j| j.value["id"].as_str() == Some(&response.judgement))
        {
            answers.push(Listed {
                value: serde_json::to_value(response)?,
                lines: vec![
                    response_line(response, true),
                    response_line(response, false),
                ],
                sources: BTreeSet::from(["log"]),
                log: true,
            });
        }
    }
    let fetched_ref = gitconfig::read_ref(&common, FETCHED_REF).is_some();
    let sources: &[(&str, bool)] = if fetched_ref {
        &[(NOTES_REF, false), (FETCHED_REF, true)]
    } else {
        &[(NOTES_REF, false)]
    };
    for (reference, fetched) in sources {
        for line in note_lines(&top, reference, &sha) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match value["schema"].as_str() {
                Some(NOTE_SCHEMA) => add_line(&mut found, &line, value, *fetched),
                Some(RESPONSE_NOTE_SCHEMA) => add_line(&mut answers, &line, value, *fetched),
                _ => {}
            }
        }
    }
    let judgements = listed_values(&found);
    let responses = listed_values(&answers);
    if json_out {
        let document = json!({
            "schema": LIST_SCHEMA,
            "commit": sha,
            "judgements": judgements,
            "responses": responses,
            "unreadable": logged.unreadable,
        });
        println!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(0);
    }
    // Notes may come from any clone: no terminal escape of theirs reaches
    // the person.
    let shown = render_list(&sha, &judgements, &responses);
    print!(
        "{}",
        shown.lines().map(printable).collect::<Vec<_>>().join("\n") + "\n"
    );
    Ok(0)
}

/// `ohjaa judgements` for a person.
fn render_list(sha: &str, judgements: &[Value], responses: &[Value]) -> String {
    let mut out = String::new();
    if judgements.is_empty() {
        let _ = writeln!(
            out,
            "No Judgement of {} here (the log, and its notes).",
            short(sha)
        );
        return out;
    }
    let _ = writeln!(out, "Judgements of {}:", short(sha));
    for judgement in judgements {
        let sources: Vec<&str> = judgement["sources"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let foreign = judgement["trust"] != "log";
        let verdict = match judgement["verdict"].as_str() {
            Some("request_changes") => "request changes",
            Some(other) => other,
            None => "?",
        };
        let claimed = if foreign { "(claimed) " } else { "" };
        let reviewer = match judgement["reviewer"]["kind"].as_str() {
            Some("agent") => format!(
                "{claimed}agent session {}{}",
                judgement["reviewer"]["session"].as_str().unwrap_or("?"),
                judgement["reviewer"]["agent"]
                    .as_str()
                    .map_or_else(String::new, |agent| format!(" ({agent})"))
            ),
            Some("person") => match (
                judgement["reviewer"]["name"].as_str(),
                judgement["reviewer"]["email"].as_str(),
            ) {
                (Some(name), Some(email)) => format!("{claimed}person {name} <{email}>"),
                _ => "a person (not named: a shared note of a repository that is not private)"
                    .to_owned(),
            },
            _ => "?".to_owned(),
        };
        let (base, head) = (
            judgement["change"]["base"]
                .as_str()
                .or(judgement["base"].as_str())
                .unwrap_or("?"),
            judgement["change"]["head"]
                .as_str()
                .or(judgement["head"].as_str())
                .unwrap_or("?"),
        );
        let at = judgement["at_ms"]
            .as_i64()
            .map(sessions::clock)
            .or_else(|| judgement["at"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "- {} {verdict} of {}..{} by {reviewer}, {at} [{}] ({})",
            judgement["id"].as_str().unwrap_or("?"),
            short(base),
            short(head),
            sources.join(", "),
            if foreign {
                "foreign: a shared note's claim, not verified here"
            } else {
                "this machine's session log: authoritative here"
            }
        );
        if judgement["conflict"] == true {
            let _ = writeln!(
                out,
                "  CONFLICT: another record claims {} and says otherwise{}",
                judgement["id"].as_str().unwrap_or("?"),
                if foreign {
                    ""
                } else {
                    "; this one, from this machine's session log, is authoritative here"
                }
            );
        }
        if let Some(findings) = judgement["findings"].as_array() {
            for finding in findings.iter().filter_map(Value::as_str) {
                let _ = writeln!(out, "  finding: {finding}");
            }
        }
        let severity = judgement["severity"].as_str();
        if severity.is_some() || judgement["blocking"] == true {
            let _ = writeln!(
                out,
                "  {}{}",
                severity.map_or_else(String::new, |s| format!("severity {s}")),
                if judgement["blocking"] == true {
                    ", blocking"
                } else {
                    ""
                }
            );
        }
        if let Some(uncertainty) = judgement["uncertainty"].as_str() {
            let _ = writeln!(out, "  uncertainty: {uncertainty}");
        }
        if let Some(people) = judgement["authors"]["people"].as_array() {
            let people: Vec<&str> = people.iter().filter_map(Value::as_str).collect();
            let sessions: Vec<String> = judgement["authors"]["sessions"]
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(|s| s["session"].as_str().or(s.as_str()).map(str::to_owned))
                .collect();
            let _ = writeln!(
                out,
                "  authors: {}{}",
                people.join(", "),
                if sessions.is_empty() {
                    String::new()
                } else {
                    format!("; sessions {}", sessions.join(", "))
                }
            );
        }
        for evidence in judgement["evidence"]["refs"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            let _ = writeln!(
                out,
                "  evidence considered: {}{} (ask sykli for its word now)",
                evidence["run"]
                    .as_str()
                    .map_or_else(String::new, |run| format!("sykli run {run}")),
                evidence["receipt"]["path"]
                    .as_str()
                    .map_or_else(String::new, |path| format!(", receipt {path}"))
            );
        }
        if let Some(why) = judgement["evidence"]["not_asked"].as_str() {
            let _ = writeln!(out, "  evidence considered: none ({why})");
        }
        let judge = &judgement["independence"]["judge"];
        let established = judge["established"] == true || *judge == "procedural";
        let evidence_why = judgement["independence"]["evidence"]["why"]
            .as_str()
            .unwrap_or("not established");
        let _ = writeln!(
            out,
            "  independence: {}; evidence not independently produced ({evidence_why})",
            if established {
                "judge independent of the authors (procedural)".to_owned()
            } else {
                let why: Vec<&str> = judge["not_established"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .filter_map(Value::as_str)
                    .collect();
                if why.is_empty() {
                    "judge's independence not established".to_owned()
                } else {
                    format!("judge's independence not established ({})", why.join("; "))
                }
            }
        );
        for response in responses
            .iter()
            .filter(|r| r["judgement"] == judgement["id"])
        {
            let by = match response["by"]["kind"].as_str() {
                Some("agent") => format!(
                    "session {}",
                    response["by"]["session"].as_str().unwrap_or("?")
                ),
                _ => response["by"]["name"]
                    .as_str()
                    .map_or_else(|| "a person".to_owned(), str::to_owned),
            };
            let _ = writeln!(
                out,
                "  answered by {by}{}: {}{}",
                if response["author"] == true {
                    " (an author)"
                } else {
                    ""
                },
                response["text"].as_str().unwrap_or("(not shared)"),
                if response["trust"] == "log" {
                    ""
                } else {
                    " (foreign: a shared note's claim)"
                }
            );
        }
    }
    out
}

// ---- the briefing and the report -----------------------------------------------

/// How far back a briefing looks for Changes with no Judgement.
const PENDING_FOR_MS: i64 = 7 * 86_400_000;

/// A session's Change that no Judgement covers: its commits, newest last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub session: String,
    pub agent: String,
    pub repo: String,
    pub intent_id: Option<String>,
    pub ended_ms: i64,
    pub commits: Vec<String>,
}

/// The ended sessions (since `since_ms`, in `here` when given) whose
/// commits some Judgement in the log does not cover, newest first.
pub fn pending(
    all: &[Session],
    judgements: &[Judgement],
    here: Option<&[String]>,
    since_ms: i64,
) -> Vec<Pending> {
    let judged: BTreeSet<&str> = judgements
        .iter()
        .flat_map(|j| j.change.commits.iter().map(|c| c.commit.as_str()))
        .collect();
    let mut out: Vec<Pending> = all
        .iter()
        .filter_map(|session| {
            let left = session.left.as_ref()?;
            if left.orphaned || left.ended_ms < since_ms || session.outside_repo {
                return None;
            }
            if here.is_some_and(|here| !here.contains(&session.repo)) {
                return None;
            }
            let commits: Vec<String> = left
                .phases
                .iter()
                .flatten()
                .filter(|p| !p.unreachable)
                .map(|p| p.commit.clone())
                .collect();
            let unjudged = commits.iter().any(|c| !judged.contains(c.as_str()));
            (!commits.is_empty() && unjudged).then(|| Pending {
                session: session.id.clone(),
                agent: program(&session.agent),
                repo: session.repo.clone(),
                intent_id: session.intent_id.clone(),
                ended_ms: left.ended_ms,
                commits,
            })
        })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.ended_ms));
    out
}

/// The briefing's Judgement part for the clone whose checkouts are `here`
/// (canonical paths): its Judgements, newest first, and its Changes with no
/// Judgement. Empty when there is neither.
pub fn briefing(
    all: &[Session],
    judgements: &[Judgement],
    responses: &[Response],
    here: &[String],
) -> String {
    let now = now_seconds() * 1000;
    let mut out = String::new();
    let mine: Vec<&Judgement> = judgements
        .iter()
        .rev()
        .filter(|j| here.contains(&j.repo) && now - j.at_ms <= PENDING_FOR_MS)
        .take(5)
        .collect();
    if !mine.is_empty() {
        out.push_str(
            "Judgements here (ohjaa's log; the evidence's independence is not established):\n",
        );
        for judgement in mine {
            let answered = responses
                .iter()
                .filter(|r| r.judgement == judgement.id)
                .count();
            let _ = writeln!(
                out,
                "- {} {} of {}..{} by {}, {}{}{}{}; {}",
                judgement.id,
                judgement.verdict.words(),
                short(&judgement.change.base),
                short(&judgement.change.head),
                judgement.reviewer.shown(),
                sessions::clock(judgement.at_ms),
                if judgement.findings.is_empty() {
                    String::new()
                } else {
                    format!("; {} finding(s)", judgement.findings.len())
                },
                if judgement.blocking { "; blocking" } else { "" },
                if answered > 0 {
                    format!("; {answered} answer(s)")
                } else {
                    String::new()
                },
                if judgement.independence.judge.established {
                    "the judge's independence: procedural, established"
                } else {
                    "the judge's independence is not established (ohjaa judgements says why)"
                }
            );
        }
    }
    let waiting = pending(all, judgements, Some(here), now - PENDING_FOR_MS);
    if !waiting.is_empty() {
        out.push_str("Changes here with no independent Judgement (request one: a person, or another agent session, runs `ohjaa judge <base>..<head>`):\n");
        for change in waiting.iter().take(5) {
            let _ = writeln!(
                out,
                "- session {} ({}{}), ended {}: {} commit(s), newest {}",
                change.session,
                change.agent,
                change
                    .intent_id
                    .as_ref()
                    .map_or_else(String::new, |id| format!(", intent {id}")),
                sessions::clock(change.ended_ms),
                change.commits.len(),
                short(change.commits.last().map_or("", String::as_str))
            );
        }
        if waiting.len() > 5 {
            let _ = writeln!(out, "- {} more", waiting.len() - 5);
        }
    }
    out
}

/// The report's `judgements` part (`ohjaa-marks.v1`, additive): the
/// Judgements recorded since `since_ms`, and the Changes with none.
pub fn report_json(all: &[Session], since_ms: i64) -> Value {
    let (judgements, responses) = match logged() {
        Ok(found) => found,
        Err(error) => {
            return json!({"listed": false, "why": format!("{error:#}"), "recorded": [], "pending": []})
        }
    };
    let rfc = |ms: i64| rfc3339(ms.div_euclid(1000));
    let recorded: Vec<Value> = judgements
        .iter()
        .filter(|j| j.at_ms >= since_ms)
        .map(|j| {
            json!({
                "id": j.id,
                "repo": j.repo,
                "slug": j.slug,
                "base": j.change.base,
                "head": j.change.head,
                "state": j.change.state,
                "commits": j.change.commits.len(),
                "verdict": j.verdict,
                "blocking": j.blocking,
                "severity": j.severity,
                "reviewer": j.reviewer,
                "at": rfc(j.at_ms),
                "judge_independent": j.independence.judge.established,
                "evidence_independent": j.independence.evidence.established,
                "answers": responses.iter().filter(|r| r.judgement == j.id).count(),
            })
        })
        .collect();
    let waiting: Vec<Value> = pending(all, &judgements, None, since_ms)
        .into_iter()
        .map(|p| {
            json!({
                "session": p.session,
                "agent": p.agent,
                "repo": p.repo,
                "intent_id": p.intent_id,
                "ended": rfc(p.ended_ms),
                "commits": p.commits,
            })
        })
        .collect();
    json!({"listed": true, "recorded": recorded, "pending": waiting})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, email: &str) -> Identity {
        Identity {
            name: name.to_owned(),
            email: email.to_owned(),
        }
    }

    fn commit(sha: &str, by: &Identity, session: Option<&str>) -> ChangeCommit {
        ChangeCommit {
            commit: sha.to_owned(),
            tree: format!("t{sha}"),
            author: by.clone(),
            committer: by.clone(),
            committed: 1,
            subject: "s".to_owned(),
            session: session.map(str::to_owned),
        }
    }

    fn change(commits: Vec<ChangeCommit>) -> Change {
        Change {
            base: "b".repeat(40),
            head: commits[0].commit.clone(),
            state: "e".repeat(40),
            commits,
            truncated: false,
        }
    }

    fn judgement() -> Judgement {
        let ann = identity("Ann Author", "ann@example.invalid");
        Judgement {
            schema: SCHEMA.to_owned(),
            id: "jd-1-2".to_owned(),
            repo: "/home/someone/private-thing".to_owned(),
            common: None,
            slug: Some("acme/secret".to_owned()),
            change: change(vec![commit(&"c".repeat(40), &ann, Some("oh-1-1"))]),
            intent_id: Some("in-1-1".to_owned()),
            authors: Authors {
                people: vec![ann.shown()],
                sessions: vec![AuthorSession {
                    session: "oh-1-1".to_owned(),
                    agent: "claude".to_owned(),
                    intent_id: None,
                    commits: 1,
                }],
                unattributed: 0,
                open: Vec::new(),
            },
            reviewer: Actor::Person {
                name: "Rae Reviewer".to_owned(),
                email: "rae@example.invalid".to_owned(),
            },
            evidence: Evidence {
                asked: Some("sykli status --commit c --contract /home/someone/x.json".to_owned()),
                refs: vec![EvidenceRef {
                    run: Some("run-1".to_owned()),
                    receipt: Some(ReceiptRef {
                        path: "/home/someone/.sykli/rcpt.json".to_owned(),
                        digest: Some("ab".repeat(32)),
                    }),
                }],
                not_asked: None,
            },
            verdict: Verdict::RequestChanges,
            findings: vec!["the secret-thing leaks".to_owned()],
            severity: Some(Severity::Major),
            blocking: true,
            uncertainty: Some("did not run it".to_owned()),
            at_ms: 1_790_000_000_000,
            independence: Independence {
                judge: JudgeIndependence {
                    established: true,
                    kind: "procedural".to_owned(),
                    basis: vec!["x".to_owned()],
                    not_established: Vec::new(),
                },
                evidence: evidence_independence(&Evidence::default()),
            },
            provenance: Provenance {
                recorded_by: "ohjaa 0".to_owned(),
                session: None,
                terminal: false,
                session_env: None,
                inside: Vec::new(),
            },
        }
    }

    /// D8 point 3: a note is one compact, canonical line: the same
    /// Judgement, the same bytes; keys in order at every depth.
    #[test]
    fn a_note_is_one_canonical_line() {
        let line = note_line(&judgement(), true);
        assert!(!line.contains('\n'));
        assert_eq!(line, note_line(&judgement(), true));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value.to_string(), line);
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(value["schema"], NOTE_SCHEMA);
        assert_eq!(value["verdict"], "request_changes");
        assert_eq!(value["reviewer"]["name"], "Rae Reviewer");
        assert_eq!(value["findings"], json!(["the secret-thing leaks"]));
    }

    /// A note of a repository that is not private names no one: only ids,
    /// hashes, the verdict and counts.
    #[test]
    fn a_note_that_may_not_name_people_holds_ids_hashes_and_the_verdict() {
        let line = note_line(&judgement(), false);
        for word in [
            "Rae",
            "rae@",
            "Ann",
            "ann@",
            "secret",
            "/home/someone",
            "claude",
            "did not run",
        ] {
            assert!(!line.contains(word), "{word:?} in {line}");
        }
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["redacted"], true);
        assert_eq!(value["id"], "jd-1-2");
        assert_eq!(value["head"], "c".repeat(40));
        assert_eq!(value["verdict"], "request_changes");
        assert_eq!(value["findings"], 1);
        assert_eq!(value["authors"]["sessions"], json!(["oh-1-1"]));
        assert_eq!(value["evidence"][0]["digest"], "ab".repeat(32));
    }

    fn authors_by(sessions: &[&str], unattributed: usize) -> Authors {
        Authors {
            people: Vec::new(),
            sessions: sessions
                .iter()
                .map(|id| AuthorSession {
                    session: (*id).to_owned(),
                    agent: "claude".to_owned(),
                    intent_id: None,
                    commits: 1,
                })
                .collect(),
            unattributed,
            open: Vec::new(),
        }
    }

    fn person(who: &Identity) -> Observed {
        Observed {
            reviewer: Reviewer::Person(who.clone()),
            inside: Vec::new(),
            claimed: None,
            claimed_other: None,
            unseen: None,
        }
    }

    /// Law 2 for a person: refused on their own commit, one no ended
    /// session's record holds (as author or committer). Their agent's work
    /// under their identity is recorded, never claimed independent: ohjaa
    /// cannot tell the person from an agent with that identity.
    #[test]
    fn law_two_for_a_person() {
        let rae = identity("Rae Reviewer", "RAE@example.invalid");
        let ann = identity("Ann Author", "ann@example.invalid");
        let theirs = change(vec![commit(&"a".repeat(40), &ann, Some("oh-1-1"))]);
        assert!(refuse_persons_own(&rae, &theirs).is_ok());
        let independent = judge_independence(
            &person(&rae),
            &theirs,
            &authors_by(&["oh-1-1"], 0),
            Some(&rae),
        );
        assert!(independent.established, "{independent:?}");
        assert!(independent.not_established.is_empty());
        assert!(
            independent
                .basis
                .iter()
                .any(|b| b.contains("cannot tell a person from an agent")),
            "{independent:?}"
        );
        let own = change(vec![
            commit(&"a".repeat(40), &ann, None),
            commit(
                &"b".repeat(40),
                &identity("Rae Reviewer", "rae@example.invalid"),
                None,
            ),
        ]);
        let refused = refuse_persons_own(&rae, &own).unwrap_err();
        assert!(refused.contains("bbbbbbb"), "{refused}");
        // Committed (not authored) by the reviewer counts too.
        let mut committed = commit(&"d".repeat(40), &ann, None);
        committed.committer = rae.clone();
        assert!(refuse_persons_own(&rae, &change(vec![committed])).is_err());
        // An agent session's work under the reviewer's identity: not
        // refused, and not established.
        let by_agent = change(vec![commit(&"e".repeat(40), &rae, Some("oh-5-5"))]);
        assert!(refuse_persons_own(&rae, &by_agent).is_ok());
        let said = judge_independence(
            &person(&rae),
            &by_agent,
            &authors_by(&["oh-5-5"], 0),
            Some(&rae),
        );
        assert!(!said.established, "{said:?}");
        assert!(
            said.not_established[0].contains("carry the reviewer's git identity")
                && said.not_established[0].contains("oh-5-5"),
            "{said:?}"
        );
        // A commit no ended session's record holds: never established.
        let open = change(vec![commit(&"f".repeat(40), &ann, None)]);
        let said = judge_independence(&person(&rae), &open, &authors_by(&[], 1), Some(&rae));
        assert!(!said.established);
        assert!(said.not_established[0].contains("no ended session's record holds"));
        // Where the process runs could not be seen: never established.
        let mut unseen = person(&rae);
        unseen.unseen = Some("ps failed".to_owned());
        assert!(
            !judge_independence(&unseen, &theirs, &authors_by(&["oh-1-1"], 0), Some(&rae))
                .established
        );
    }

    /// The independence rule for an Agent reviewer: established only in its
    /// own session's process group, as OHJAA_SESSION says, and in no other.
    #[test]
    fn an_agent_is_independent_only_in_its_own_session_and_as_it_says() {
        let author = "oh-1-1";
        let theirs = change_of(&[("a1", Some(author))]);
        let authors = authors_by(&[author], 0);
        let reviewer = session("oh-2-1", "/c/widget", 100, None);
        let agent = |inside: Vec<Session>, claimed: Option<&str>, shown: bool| Observed {
            reviewer: Reviewer::Agent {
                session: Box::new(reviewer.clone()),
                shown,
            },
            inside,
            claimed: claimed.map(str::to_owned),
            claimed_other: None,
            unseen: None,
        };
        let honest = judge_independence(
            &agent(vec![reviewer.clone()], Some("oh-2-1"), true),
            &theirs,
            &authors,
            None,
        );
        assert!(honest.established, "{honest:?}");
        // OHJAA_SESSION dropped, inside the session's group: an agent
        // presenting as a person.
        let hidden = judge_independence(
            &agent(vec![reviewer.clone()], None, true),
            &theirs,
            &authors,
            None,
        );
        assert!(!hidden.established);
        assert!(
            hidden.not_established[0].contains("OHJAA_SESSION is not set"),
            "{hidden:?}"
        );
        // Another session's id claimed.
        let borrowed = judge_independence(
            &agent(vec![reviewer.clone()], Some("oh-9-9"), true),
            &theirs,
            &authors,
            None,
        );
        assert!(!borrowed.established);
        assert!(borrowed.not_established[0].contains("OHJAA_SESSION names oh-9-9"));
        // Not shown to run in the session it claims.
        let claimed = judge_independence(
            &agent(Vec::new(), Some("oh-2-1"), false),
            &theirs,
            &authors,
            None,
        );
        assert!(!claimed.established);
        assert!(claimed.not_established[0].contains("not shown to run in session oh-2-1"));
        // A session started inside another.
        let outer = session("oh-3-1", "/c/widget", 90, None);
        let nested = judge_independence(
            &agent(vec![reviewer.clone(), outer], Some("oh-2-1"), true),
            &theirs,
            &authors,
            None,
        );
        assert!(!nested.established);
        assert!(nested.not_established[0].contains("also runs inside live session(s) oh-3-1"));
        // A commit no ended record holds, under the agent's identity.
        let unattributed = change_of(&[("u1", None)]);
        let ann = identity("Ann", "ann@x");
        let said = judge_independence(
            &agent(vec![reviewer.clone()], Some("oh-2-1"), true),
            &unattributed,
            &authors_by(&[], 1),
            Some(&ann),
        );
        assert!(!said.established);
        assert!(said.not_established[1].contains("the git identity the reviewer acts under"));
    }

    /// Who judges is observed: the live session whose process group holds
    /// the process or an ancestor, or whose ohjaa is an ancestor, innermost
    /// first; never an ended one, nor a group id reused since.
    #[test]
    fn the_sessions_a_process_runs_inside_are_observed_not_claimed() {
        let proc = |pid, ppid, pgid, elapsed| process::Proc {
            pid,
            ppid,
            pgid,
            elapsed,
        };
        let now = 10_000;
        // judge (500) < shell (400, group 300) < toimija (300, leads 300)
        // < ohjaa run (200, group 100, the outer session's) < ohjaa (100).
        let table = vec![
            proc(500, 400, 300, 1),
            proc(400, 300, 300, 5),
            proc(300, 200, 300, 9),
            proc(200, 150, 100, 10),
            proc(150, 100, 100, 50),
            proc(100, 1, 100, 60),
        ];
        let chain = table.clone();
        let mut inner = session("oh-9990000-200", "/c/widget", now - 10, None);
        inner.pgid = Some(300);
        inner.group_started_ms = Some((now - 10) * 1000);
        let mut outer = session("oh-9940000-90", "/c/widget", now - 60, None);
        outer.pgid = Some(100);
        let ended = {
            let mut s = session(
                "oh-1-1",
                "/c/widget",
                now - 60,
                Some(left(now - 30, &[], &[])),
            );
            s.pgid = Some(300);
            s
        };
        let all = vec![ended, outer.clone(), inner.clone()];
        let around: Vec<&str> = sessions_around(&all, &table, &chain, now)
            .iter()
            .map(|s| s.id.as_str())
            .collect();
        assert_eq!(around, vec![inner.id.as_str(), outer.id.as_str()]);
        // A group id recorded before its leader started: reused, not it.
        let mut reused = inner.clone();
        reused.group_started_ms = Some((now - 3600) * 1000);
        reused.started_ms = (now - 3600) * 1000;
        reused.id = "oh-1-77".to_owned();
        assert!(sessions_around(&[reused], &table, &chain, now).is_empty());
        // Its ohjaa an ancestor (its group not recorded yet): inside.
        let mut early = session("oh-9990000-200", "/c/widget", now - 10, None);
        early.pgid = None;
        assert_eq!(sessions_around(&[early], &table, &chain, now).len(), 1);
    }

    /// What a public note may hold: exactly what ohjaa writes without
    /// names. A line that only says it is redacted is not.
    #[test]
    fn only_a_truly_redacted_line_is_publishable() {
        let judged = judgement();
        assert!(publishable(&note_line(&judged, false)));
        assert!(!publishable(&note_line(&judged, true)));
        let response = Response {
            schema: RESPONSE_SCHEMA.to_owned(),
            id: "rs-1-2".to_owned(),
            judgement: "jd-1-2".to_owned(),
            head: "c".repeat(40),
            by: Actor::Person {
                name: "Ann".to_owned(),
                email: "ann@x".to_owned(),
            },
            author: true,
            text: "secret".to_owned(),
            at_ms: 1,
        };
        assert!(publishable(&response_line(&response, false)));
        assert!(!publishable(&response_line(&response, true)));
        let mut forged: Value = serde_json::from_str(&note_line(&judged, false)).unwrap();
        forged["reviewer"] = json!({"kind": "person", "name": "Yair"});
        assert!(!publishable(&forged.to_string()));
        let mut forged: Value = serde_json::from_str(&note_line(&judged, false)).unwrap();
        forged["findings"] = json!(["acme-bank"]);
        assert!(!publishable(&forged.to_string()));
        let mut forged: Value = serde_json::from_str(&note_line(&judged, false)).unwrap();
        forged["redacted"] = json!(false);
        assert!(!publishable(&forged.to_string()));
        assert!(!publishable("not json"));
        // A named line and its redacted form say the same; another verdict
        // does not.
        let named: Value = serde_json::from_str(&note_line(&judged, true)).unwrap();
        let redacted: Value = serde_json::from_str(&note_line(&judged, false)).unwrap();
        assert!(agree(&named, &redacted));
        let mut other = redacted.clone();
        other["verdict"] = json!("approve");
        assert!(!agree(&named, &other));
        let mut renamed = named.clone();
        renamed["reviewer"]["name"] = json!("Yair");
        assert!(!agree(&named, &renamed));
    }

    /// Notes are a union: no line of any side is lost, two different lines
    /// of one id are both kept, and the order is git's cat_sort_uniq's.
    #[test]
    fn the_union_of_notes_keeps_every_line() {
        let notes = |pairs: &[(&str, &[&str])]| -> Notes {
            pairs
                .iter()
                .map(|(object, lines)| {
                    (
                        (*object).to_owned(),
                        lines.iter().map(|l| (*l).to_owned()).collect(),
                    )
                })
                .collect()
        };
        let mine = notes(&[("a", &["2", "1"]), ("b", &["x"])]);
        let theirs = notes(&[("a", &["3"]), ("c", &["y"])]);
        assert_eq!(
            union(&[&mine, &theirs]),
            notes(&[("a", &["1", "2", "3"]), ("b", &["x"]), ("c", &["y"])])
        );
        assert_eq!(
            tidy(&notes(&[("a", &["2", "1", "2"]), ("e", &[])])),
            notes(&[("a", &["1", "2"])])
        );
    }

    /// The sharing table of D8 point 3.
    #[test]
    fn who_shares_and_who_names() {
        let entry = |visibility, share| Entry {
            dir: PathBuf::from("/e"),
            branch: "main".to_owned(),
            sharing: home::Sharing {
                visibility,
                share_judgements: share,
                push_judgements: false,
            },
        };
        let private = sharing_of(Some(&entry(Visibility::Private, None)));
        assert!(private.shared && private.names);
        let opted_out = sharing_of(Some(&entry(Visibility::Private, Some(false))));
        assert!(!opted_out.shared);
        let public = sharing_of(Some(&entry(Visibility::Public, None)));
        assert!(!public.shared && !public.names);
        let opted_in = sharing_of(Some(&entry(Visibility::Public, Some(true))));
        assert!(opted_in.shared && !opted_in.names);
        let unknown = sharing_of(Some(&entry(Visibility::Unknown, None)));
        assert!(!unknown.shared && !unknown.names);
        assert!(unknown.why.contains("visibility = \"private\""));
        let none = sharing_of(None);
        assert!(!none.shared && !none.names);
    }

    fn session(id: &str, repo: &str, started_s: i64, left: Option<Value>) -> Session {
        let mut value = json!({
            "id": id, "agent": "sh", "repo": repo, "intent": "x",
            "started_ms": started_s * 1000, "delivered": [],
        });
        if let Some(left) = left {
            value["left"] = left;
        }
        serde_json::from_value(value).unwrap()
    }

    fn left(ended_s: i64, phases: &[&str], observed: &[&str]) -> Value {
        let phase = |c: &&str| json!({"commit": c, "subject": "s"});
        json!({
            "ended_ms": ended_s * 1000,
            "exit": {"how": "code", "code": 0},
            "phases": phases.iter().map(phase).collect::<Vec<_>>(),
            "observed": observed.iter().map(phase).collect::<Vec<_>>(),
            "finished": {"kind": "no_pull_request"},
        })
    }

    fn made_at(reference: &str, commit: &str, at: i64) -> lifecycle::Move {
        lifecycle::Move {
            reference: reference.to_owned(),
            at,
            commit: commit.to_owned(),
            subject: "commit: s".to_owned(),
        }
    }

    fn change_of(commits: &[(&str, Option<&str>)]) -> Change {
        let ann = identity("Ann", "ann@x");
        change(
            commits
                .iter()
                .map(|(sha, session)| commit(sha, &ann, *session))
                .collect(),
        )
    }

    /// Law 2 for an agent session, as the demo with sykli main found it: a
    /// session that commits and judges in the second another session here
    /// ended is the one that made the commit. An ended session's record says
    /// what it made; the clock does not.
    #[test]
    fn a_reviewer_that_committed_in_the_second_another_ended_made_it() {
        let here = "/c/widget";
        let clone = vec![PathBuf::from(here), PathBuf::from("/c/widget-b")];
        let reviewer = session("oh-2-1", here, 100, None);
        let author = session("oh-1-1", here, 90, Some(left(100, &["a1"], &[])));
        let all = vec![author, reviewer.clone()];
        // Its own checkout shows c1 made in its first second: its own, even
        // with the other session ending in that second.
        let own = [made_at("HEAD", "c1", 100), made_at("HEAD", "a1", 99)];
        let change = change_of(&[("c1", None), ("a1", Some("oh-1-1"))]);
        let made = reviewer_made(&reviewer, &change, &all, &clone, &own, &[]);
        assert_eq!(
            made,
            vec![Made {
                commit: "c1".to_owned(),
                shared_with: Vec::new()
            }]
        );
        assert!(made_refusal(&reviewer, "this session", &change, &made).is_some());
        // What the log gives the other session is never the reviewer's.
        let theirs = change_of(&[("a1", Some("oh-1-1"))]);
        let late = [made_at("HEAD", "a1", 100)];
        assert!(reviewer_made(&reviewer, &theirs, &all, &clone, &late, &[]).is_empty());
        // Made before it started: not considered.
        let before = change_of(&[("b0", None)]);
        let early = [made_at("HEAD", "b0", 99)];
        assert!(reviewer_made(&reviewer, &before, &all, &clone, &early, &[]).is_empty());
    }

    /// In another checkout, a commit is the reviewer's unless another
    /// session was in the clone then: an open one by the clock, an ended one
    /// by its record (it saw it made beside it). In its own checkout, one an
    /// open session shared with it is its to answer for, said with whom.
    #[test]
    fn who_else_was_there_decides_only_beside_the_reviewer_checkout() {
        let here = "/c/widget";
        let other = "/c/widget-b";
        let clone = vec![PathBuf::from(here), PathBuf::from(other)];
        let reviewer = session("oh-2-1", here, 100, None);
        let branch = [made_at("refs/heads/feat", "c2", 105)];
        let change = change_of(&[("c2", None)]);
        // Nobody else: its own.
        let alone = vec![reviewer.clone()];
        assert_eq!(
            reviewer_made(&reviewer, &change, &alone, &clone, &[], &branch).len(),
            1
        );
        // An open session in the other checkout: contested.
        let open = session("oh-3-1", other, 101, None);
        let with_open = vec![reviewer.clone(), open.clone()];
        assert!(reviewer_made(&reviewer, &change, &with_open, &clone, &[], &branch).is_empty());
        // An ended one that saw it made beside it: contested; one that did
        // not see it: not, whatever its times.
        let saw = session("oh-4-1", other, 101, Some(left(110, &[], &["c2"])));
        assert!(reviewer_made(
            &reviewer,
            &change,
            &[reviewer.clone(), saw],
            &clone,
            &[],
            &branch
        )
        .is_empty());
        let missed = session("oh-5-1", other, 101, Some(left(110, &[], &[])));
        assert_eq!(
            reviewer_made(
                &reviewer,
                &change,
                &[reviewer.clone(), missed],
                &clone,
                &[],
                &branch
            )
            .len(),
            1
        );
        // Its own checkout, shared with an open session: its to answer for.
        let sharing = session("oh-6-1", here, 101, None);
        let own = [made_at("HEAD", "c2", 105)];
        let made = reviewer_made(
            &reviewer,
            &change,
            &[reviewer.clone(), sharing],
            &clone,
            &own,
            &[],
        );
        assert_eq!(made[0].shared_with, vec!["oh-6-1".to_owned()]);
        let refused = made_refusal(&reviewer, "this session", &change, &made).unwrap();
        assert!(refused.contains("while oh-6-1 shared it"), "{refused}");
    }

    /// An Intent id is what ohjaa mints, never free text a note would carry;
    /// a note's text reaches a terminal with no escape of its own.
    #[test]
    fn ids_are_ids_and_shown_text_has_no_escapes() {
        assert!(intent_id_shaped("in-1791944000000-123"));
        for text in [
            "secret-project",
            "in-",
            "in-12",
            "in-a-1",
            "in-1-",
            "oh-1-2",
        ] {
            assert!(!intent_id_shaped(text), "{text}");
        }
        let mut redacted = judgement();
        redacted.intent_id = Some("secret-project".to_owned());
        assert!(!note_line(&redacted, false).contains("secret"));
        assert_eq!(printable("ok\u{1b}[2Jgone\u{7}"), "ok?[2Jgone?");
    }

    #[test]
    fn a_range_names_its_ends() {
        assert_eq!(split_range("main..feat"), (Some("main"), "feat"));
        assert_eq!(split_range("feat"), (None, "feat"));
        assert_eq!(split_range("main.."), (Some("main"), "HEAD"));
        assert_eq!(split_range("..feat"), (None, "feat"));
    }
}
