//! Independent Judgement (faija-ontology.v2 *Judgement*; law 2: the Agent
//! never judges its own Change), KATANA D5 and D8.
//!
//! `ohjaa judge` records a Judgement of a Change (`base..head`) and its exact
//! State (the head's tree) in the session log's `judgements` table, which
//! stays authoritative for the write:
//!
//! - who made the Change: its commits' authors (name and email), and the
//!   ohjaa sessions the log attributes them to;
//! - the reviewer: a Person (git identity), or the Agent session it runs in;
//! - the Intent, the evidence considered (sykli's run and receipt references
//!   from `sykli status --commit`, never a verdict copied from sykli), the
//!   verdict, findings, severity, blocking, uncertainty, time and provenance;
//! - two independence facts, kept apart: the judge's, procedural (the
//!   reviewer made no commit of the Change), and the evidence's, which is not
//!   established today and is said so.
//!
//! A reviewer that made any commit of the Change is refused, and nothing is
//! recorded. A Judgement is never changed or removed; an answer (`ohjaa
//! judge respond`) is a record of its own beside it.
//!
//! After recording, the Judgement is attached to its head as a git note under
//! `refs/notes/ohjaa/judgements`: one compact, canonical JSON line (git's
//! `cat_sort_uniq` merge then joins two clones' notes line by line). Notes
//! are refs, never files, hooks or config. They leave the machine only with
//! `--push` (or the entry's `push_judgements = true`), only for an entry that
//! shares them: a private one by default, a public one (or one that does not
//! say) only with `share_judgements = true`; and the note of any entry not
//! private names no one: ids, hashes and the verdict only.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
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
const REFUSED: i32 = 3;
const CANNOT: i32 = 2;

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

/// The judge's independence of the authors: procedural.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeIndependence {
    pub established: bool,
    pub kind: String,
    pub basis: Vec<String>,
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
    let mut command = Command::new("git");
    // git's own words, whatever the locale: some are read (`fetch`).
    command.arg("-C").arg(top).args(args).env("LC_ALL", "C");
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
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

/// Attach `line` to `head`'s note under [`NOTES_REF`] (one git process).
fn attach(top: &Path, head: &str, line: &str) -> Result<(), String> {
    git_ok(
        top,
        &["notes", "--ref", NOTES_REF, "append", "-F", "-", head],
        Some(format!("{line}\n").as_bytes()),
    )
    .map(|_| ())
}

/// Push [`NOTES_REF`] to origin, and nothing else: origin's notes are
/// fetched first ([`FETCHED_REF`]) and merged line by line, so none is lost.
fn push(top: &Path, common: &Path, names: bool) -> Result<String, String> {
    fetch(top)?;
    let fetched = gitconfig::read_ref(common, FETCHED_REF).is_some();
    // An entry that is not private never publishes a note that names
    // anyone: one written while it was private, or one not ohjaa's. Origin's
    // own are already there.
    if !names {
        let theirs: BTreeSet<String> = if fetched {
            all_note_lines(top, FETCHED_REF)?.into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let named = all_note_lines(top, NOTES_REF)?
            .into_iter()
            .filter(|line| !theirs.contains(line))
            .filter(|line| {
                serde_json::from_str::<Value>(line).map_or(true, |note| note["redacted"] != true)
            })
            .count();
        if named > 0 {
            return Err(format!(
                "{named} note line(s) under {NOTES_REF} are not redacted (written while its entry \
                 was private, or not ohjaa's), and may name people: they stay on this machine"
            ));
        }
    }
    if fetched {
        git_ok(
            top,
            &[
                "notes",
                "--ref",
                NOTES_REF,
                "merge",
                "-q",
                "-s",
                "cat_sort_uniq",
                FETCHED_REF,
            ],
            None,
        )
        .map_err(|why| format!("origin's notes could not be merged: {why}"))?;
    }
    git_ok(
        top,
        &["push", "-q", "origin", &format!("{NOTES_REF}:{NOTES_REF}")],
        None,
    )
    .map_err(|why| format!("git push origin {NOTES_REF} failed: {why}"))?;
    Ok(format!("pushed {NOTES_REF} to origin"))
}

/// Every non-empty line of every note under `reference` (none when the ref
/// does not exist): `git notes list`, then the notes in one `cat-file
/// --batch`.
fn all_note_lines(top: &Path, reference: &str) -> Result<Vec<String>, String> {
    let listed = git(top, &["notes", "--ref", reference, "list"], None)?;
    if !listed.status.success() {
        return Ok(Vec::new());
    }
    let blobs: Vec<String> = listed
        .stdout_text()
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect();
    if blobs.is_empty() {
        return Ok(Vec::new());
    }
    let asked = format!("{}\n", blobs.join("\n"));
    let output = git(top, &["cat-file", "--batch"], Some(asked.as_bytes()))?;
    if !output.status.success() {
        return Err(format!(
            "the notes under {reference} cannot be read: {}",
            output.failure_detail()
        ));
    }
    // `<oid> blob <size>\n<content>\n`, one after another.
    let bytes = &output.stdout;
    let mut lines = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(end) = bytes[at..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&bytes[at..at + end]).into_owned();
        at += end + 1;
        let size: usize = header
            .split_whitespace()
            .nth(2)
            .and_then(|size| size.parse().ok())
            .ok_or_else(|| format!("the notes under {reference} cannot be read: {header}"))?;
        let content = bytes
            .get(at..at + size)
            .ok_or_else(|| format!("the notes under {reference} were cut short"))?;
        lines.extend(
            String::from_utf8_lossy(content)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_owned),
        );
        at += size + 1;
    }
    Ok(lines)
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

/// The lines of `commit`'s note under `reference`, each a JSON object.
fn note_values(top: &Path, reference: &str, commit: &str) -> Vec<Value> {
    let Ok(text) = git_ok(top, &["notes", "--ref", reference, "show", commit], None) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
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

/// [`reviewer_made`], with the reflogs read: two git processes (the
/// reviewer's checkout's HEAD, and every branch of the clone).
fn made_by_reviewer(
    session: &Session,
    change: &Change,
    all: &[Session],
    top: &Path,
    clone: &[PathBuf],
) -> Result<Vec<Made>, String> {
    let own_checkout = canonical(Path::new(&session.repo));
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
    let branches = lifecycle::reflog(
        top,
        &[
            "reflog",
            "show",
            "--all",
            "--date=unix",
            lifecycle::REFLOG_FORMAT,
        ],
    )
    .ok_or_else(|| "the clone's branch reflogs cannot be read".to_owned())?;
    Ok(reviewer_made(session, change, all, clone, &own, &branches))
}

/// Law 2, for a Person: refused when their identity is on a commit of the
/// Change (as author or committer) that no agent session's record holds.
/// Commits an agent session made under their identity are said, not
/// refused: the log says who made them. Ok: the basis of independence.
fn person_independence(person: &Identity, change: &Change) -> Result<Vec<String>, String> {
    let mine: Vec<&ChangeCommit> = change
        .commits
        .iter()
        .filter(|c| c.author.same(person) || c.committer.same(person))
        .collect();
    let own: Vec<&&ChangeCommit> = mine.iter().filter(|c| c.session.is_none()).collect();
    if !own.is_empty() {
        let listed: Vec<&str> = own.iter().map(|c| short(&c.commit)).collect();
        return Err(format!(
            "{} made {} of the Change ({}): no reviewer judges a Change they made",
            person.shown(),
            if own.len() == 1 {
                "a commit"
            } else {
                "commits"
            },
            listed.join(", ")
        ));
    }
    let mut basis = vec![format!(
        "the reviewer, {}, is on none of the Change's {} commit(s) as author or committer",
        person.shown(),
        change.commits.len()
    )];
    if !mine.is_empty() {
        let by: BTreeSet<&str> = mine.iter().filter_map(|c| c.session.as_deref()).collect();
        basis[0] = format!(
            "{} of the Change's {} commit(s) are under the reviewer's git identity, made by agent \
             session(s) {} (the session log): the reviewer made none of them",
            mine.len(),
            change.commits.len(),
            by.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(basis)
}

/// Law 2, for an Agent session: refused when it made any commit of the
/// Change (`made`, see [`reviewer_made`]), or the log gives one to it.
fn agent_independence(
    session: &Session,
    change: &Change,
    made: &[Made],
    authors: &Authors,
) -> Result<Vec<String>, String> {
    let theirs: Vec<&str> = change
        .commits
        .iter()
        .filter(|c| {
            c.session.as_deref() == Some(session.id.as_str())
                || made.iter().any(|m| m.commit == c.commit)
        })
        .map(|c| short(&c.commit))
        .collect();
    if !theirs.is_empty() {
        let shared: BTreeSet<&str> = made
            .iter()
            .flat_map(|m| m.shared_with.iter().map(String::as_str))
            .collect();
        return Err(format!(
            "{} of the Change ({}) {} made by this session, {}{}: the Agent never judges its own \
             Change",
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
        ));
    }
    let mut basis = vec![format!(
        "the reviewing session {} made none of the Change's {} commit(s): its checkout's reflog \
         and the clone's branches show none created by it, and the session log gives none to it",
        session.id,
        change.commits.len()
    )];
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
    Ok(basis)
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

/// The reviewer: the open session this runs inside (`OHJAA_SESSION`), else
/// the person git names here.
enum Reviewer {
    Agent(Box<Session>),
    Person(Identity),
}

fn reviewer(top: &Path, all: &[Session]) -> Result<Reviewer, String> {
    match std::env::var(sessions::SESSION_ENV) {
        Ok(id) => {
            if !sessions::is_valid_id(&id) {
                return Err(format!(
                    "{} is not a session id: `{id}`",
                    sessions::SESSION_ENV
                ));
            }
            let session = all.iter().find(|s| s.id == id).cloned().ok_or_else(|| {
                format!("session {id} is not in the session log, so what it made cannot be shown")
            })?;
            if session.left.is_some() {
                return Err(format!(
                    "session {id} has ended; a Judgement is recorded from a live session (or by a \
                     person, outside one)"
                ));
            }
            Ok(Reviewer::Agent(Box::new(session)))
        }
        Err(_) => identity_here(top).map(Reviewer::Person),
    }
}

/// `ohjaa judge`: record a Judgement, attach its note, and push it when
/// asked. 0 recorded; 3 refused (law 2, or sharing); 2 nothing to judge.
pub fn judge(args: &JudgeArgs<'_>) -> Result<i32> {
    let here = here()?;
    let sharing = sharing_of(here.entry.as_ref());
    if args.push && !sharing.shared {
        eprintln!(
            "ohjaa judge: refused to share: {}. Nothing was recorded; judge without --push to \
             keep it on this machine, or set share_judgements = true in the entry's repo.toml",
            sharing.why
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
    let logged = read_log()?;
    let reviewer = match reviewer(&here.top, &logged.sessions) {
        Ok(reviewer) => reviewer,
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
    let (actor, basis, session) = match &reviewer {
        Reviewer::Person(person) => match person_independence(person, &change) {
            Ok(basis) => (
                Actor::Person {
                    name: person.name.clone(),
                    email: person.email.clone(),
                },
                basis,
                None,
            ),
            Err(why) => return refused(why),
        },
        Reviewer::Agent(session) => {
            let made = match made_by_reviewer(session, &change, &logged.sessions, &here.top, &clone)
            {
                Ok(made) => made,
                Err(why) => {
                    return refused(format!("what this session made cannot be read: {why}"))
                }
            };
            match agent_independence(session, &change, &made, &authors) {
                Ok(basis) => (
                    Actor::Agent {
                        session: session.id.clone(),
                        agent: program(&session.agent),
                    },
                    basis,
                    Some(session.id.clone()),
                ),
                Err(why) => return refused(why),
            }
        }
    };
    let mut basis = basis;
    if authors.unattributed > 0 {
        basis.push(format!(
            "{} of the Change's commit(s) no ended session's record holds (made outside ohjaa, \
             or by a session still open{}): who made those is not shown by the log, only that \
             the reviewer's own checkout and identity do not hold them",
            authors.unattributed,
            if authors.open.is_empty() {
                String::new()
            } else {
                format!(": {}", authors.open.join(", "))
            }
        ));
    }
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
            judge: JudgeIndependence {
                established: true,
                kind: "procedural".to_owned(),
                basis,
            },
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
        },
        change,
    };
    let bytes = serde_json::to_vec(&judgement)?;
    sessions::Store::open(&log_path()?)
        .and_then(|store| store.insert_new(sessions::JUDGEMENTS, &judgement.id, &bytes, None))
        .map_err(|error| anyhow::anyhow!("the Judgement could not be recorded: {error}"))?;
    println!(
        "ohjaa judge: recorded {} ({}) of {}..{} by {}",
        judgement.id,
        judgement.verdict.words(),
        short(&judgement.change.base),
        short(&judgement.change.head),
        judgement.reviewer.shown()
    );
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
        Err(why) => {
            eprintln!(
                "ohjaa judge: the note could not be attached ({why}); the log holds the \
                 Judgement"
            );
            // Pushing now would share the notes without this one, and say
            // it was shared.
            if args.push || sharing.push {
                eprintln!("ohjaa judge: not shared: its note is not attached");
                return Ok(1);
            }
        }
    }
    if args.push || sharing.push {
        match push(&here.top, &here.common, sharing.names) {
            Ok(said) => println!("ohjaa judge: {said}"),
            Err(why) => {
                eprintln!("ohjaa judge: not shared: {why}; it is recorded on this machine");
                return Ok(1);
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
    Ok(0)
}

/// `ohjaa judge respond <judgement> <text>`: an answer beside a Judgement,
/// never a change to it.
pub fn respond(judgement: &str, text: &str) -> Result<i32> {
    let text = text.trim();
    if text.is_empty() {
        bail!("say what you answer");
    }
    let here = here()?;
    let logged = read_log()?;
    let Some(judged) = logged.judgements.iter().find(|j| j.id == judgement) else {
        eprintln!("ohjaa judge respond: no Judgement {judgement} in this machine's session log");
        return Ok(CANNOT);
    };
    let (by, author) = match reviewer(&here.top, &logged.sessions) {
        Ok(Reviewer::Agent(session)) => {
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
        Ok(Reviewer::Person(person)) => {
            let author = judged
                .change
                .commits
                .iter()
                .any(|c| c.author.same(&person) || c.committer.same(&person));
            (
                Actor::Person {
                    name: person.name,
                    email: person.email,
                },
                author,
            )
        }
        Err(why) => {
            eprintln!("ohjaa judge respond: refused: {why}");
            return Ok(REFUSED);
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
    sessions::Store::open(&log_path()?)
        .and_then(|store| {
            store.insert_new(
                sessions::RESPONSES,
                &response.id,
                &bytes,
                Some((sessions::JUDGEMENTS, &response.judgement)),
            )
        })
        .map_err(|error| anyhow::anyhow!("the answer could not be recorded: {error}"))?;
    println!(
        "ohjaa judge respond: recorded {} beside {}; the Judgement is unchanged",
        response.id, response.judgement
    );
    let sharing = sharing_of(here.entry.as_ref());
    if let Err(why) = attach(
        &here.top,
        &response.head,
        &response_line(&response, sharing.names),
    ) {
        eprintln!("ohjaa judge respond: the note could not be attached ({why}); the log holds it");
    }
    Ok(0)
}

/// `ohjaa judgements <commit>`: every Judgement of it, from the log and the
/// notes (this clone's, and those fetched from origin), each marked with
/// where it was read.
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
                "ohjaa judgements: the session log cannot be read ({error:#}); the notes only"
            );
            Logged {
                sessions: Vec::new(),
                judgements: Vec::new(),
                responses: Vec::new(),
                unreadable: Vec::new(),
            }
        }
    };
    let mut found: Vec<(Value, BTreeSet<&'static str>)> = Vec::new();
    let mut answers: Vec<(Value, BTreeSet<&'static str>)> = Vec::new();
    let add =
        |list: &mut Vec<(Value, BTreeSet<&'static str>)>, value: Value, source: &'static str| {
            let id = value["id"].clone();
            match list.iter_mut().find(|(known, _)| known["id"] == id) {
                Some((_, sources)) => {
                    sources.insert(source);
                }
                None => list.push((value, BTreeSet::from([source]))),
            }
        };
    for judgement in &logged.judgements {
        if judgement.change.head == sha || judgement.change.commits.iter().any(|c| c.commit == sha)
        {
            add(&mut found, serde_json::to_value(judgement)?, "log");
        }
    }
    let listed: Vec<String> = found
        .iter()
        .filter_map(|(v, _)| v["id"].as_str().map(str::to_owned))
        .collect();
    for response in &logged.responses {
        if listed.contains(&response.judgement) {
            add(&mut answers, serde_json::to_value(response)?, "log");
        }
    }
    let fetched_ref = gitconfig::read_ref(&common, FETCHED_REF).is_some();
    let sources: &[(&str, &'static str)] = if fetched_ref {
        &[(NOTES_REF, "note"), (FETCHED_REF, "fetched")]
    } else {
        &[(NOTES_REF, "note")]
    };
    for (reference, source) in sources {
        for value in note_values(&top, reference, &sha) {
            match value["schema"].as_str() {
                Some(NOTE_SCHEMA) => add(&mut found, value, source),
                Some(RESPONSE_NOTE_SCHEMA) => add(&mut answers, value, source),
                _ => {}
            }
        }
    }
    let with_sources = |list: Vec<(Value, BTreeSet<&'static str>)>| -> Vec<Value> {
        list.into_iter()
            .map(|(mut value, sources)| {
                // Fixed order: log, note, fetched.
                let ordered: Vec<&str> = ["log", "note", "fetched"]
                    .into_iter()
                    .filter(|s| sources.contains(s))
                    .collect();
                value["sources"] = json!(ordered);
                value
            })
            .collect()
    };
    let judgements = with_sources(found);
    let responses = with_sources(answers);
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
        let verdict = match judgement["verdict"].as_str() {
            Some("request_changes") => "request changes",
            Some(other) => other,
            None => "?",
        };
        let reviewer = match judgement["reviewer"]["kind"].as_str() {
            Some("agent") => format!(
                "agent session {}{}",
                judgement["reviewer"]["session"].as_str().unwrap_or("?"),
                judgement["reviewer"]["agent"]
                    .as_str()
                    .map_or_else(String::new, |agent| format!(" ({agent})"))
            ),
            Some("person") => match (
                judgement["reviewer"]["name"].as_str(),
                judgement["reviewer"]["email"].as_str(),
            ) {
                (Some(name), Some(email)) => format!("person {name} <{email}>"),
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
            "- {} {verdict} of {}..{} by {reviewer}, {at} [{}]",
            judgement["id"].as_str().unwrap_or("?"),
            short(base),
            short(head),
            sources.join(", ")
        );
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
        let judge = judgement["independence"]["judge"]["established"] == true
            || judgement["independence"]["judge"] == "procedural";
        let evidence_why = judgement["independence"]["evidence"]["why"]
            .as_str()
            .unwrap_or("not established");
        let _ = writeln!(
            out,
            "  independence: {}; evidence not independently produced ({evidence_why})",
            if judge {
                "judge independent of the authors (procedural)"
            } else {
                "judge's independence not established"
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
                "  answered by {by}{}: {}",
                if response["author"] == true {
                    " (an author)"
                } else {
                    ""
                },
                response["text"].as_str().unwrap_or("(not shared)")
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
        out.push_str("Judgements here (ohjaa's log; the judge's independence is procedural, the evidence's is not established):\n");
        for judgement in mine {
            let answered = responses
                .iter()
                .filter(|r| r.judgement == judgement.id)
                .count();
            let _ = writeln!(
                out,
                "- {} {} of {}..{} by {}, {}{}{}{}",
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
            state: "t".repeat(40),
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
                },
                evidence: evidence_independence(&Evidence::default()),
            },
            provenance: Provenance {
                recorded_by: "ohjaa 0".to_owned(),
                session: None,
                terminal: false,
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

    /// Law 2 for a person: refused on their own commit; an agent session's
    /// commit under their identity is said, not refused.
    #[test]
    fn law_two_for_a_person() {
        let rae = identity("Rae Reviewer", "RAE@example.invalid");
        let ann = identity("Ann Author", "ann@example.invalid");
        let theirs = change(vec![commit(&"a".repeat(40), &ann, None)]);
        assert!(person_independence(&rae, &theirs).is_ok());
        let own = change(vec![
            commit(&"a".repeat(40), &ann, None),
            commit(
                &"b".repeat(40),
                &identity("Rae Reviewer", "rae@example.invalid"),
                None,
            ),
        ]);
        let refused = person_independence(&rae, &own).unwrap_err();
        assert!(refused.contains("bbbbbbb"), "{refused}");
        // Committed (not authored) by the reviewer counts too.
        let mut committed = commit(&"d".repeat(40), &ann, None);
        committed.committer = rae.clone();
        assert!(person_independence(&rae, &change(vec![committed])).is_err());
        let by_agent = change(vec![commit(&"e".repeat(40), &rae, Some("oh-5-5"))]);
        let basis = person_independence(&rae, &by_agent).unwrap();
        assert!(
            basis[0].contains("made by agent session(s) oh-5-5"),
            "{basis:?}"
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
        assert!(agent_independence(&reviewer, &change, &made, &authors_none()).is_err());
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
        let refused = agent_independence(&reviewer, &change, &made, &authors_none()).unwrap_err();
        assert!(refused.contains("while oh-6-1 shared it"), "{refused}");
    }

    fn authors_none() -> Authors {
        Authors {
            people: Vec::new(),
            sessions: Vec::new(),
            unattributed: 0,
            open: Vec::new(),
        }
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
