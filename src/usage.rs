//! Imported usage is a source assertion, never an invoice or an observation by ohjaa.
//!
//! What ohjaa does find itself is which of the agent's own sessions a
//! session ran (D7, [`find_native`]), so a consumer can join the agent's own
//! accounting to it without an imported receipt.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A receipt as stored in the session log. Reading one never refuses a field
/// a later build added; importing one does (see [`Receipt::read`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub schema: String,
    pub agent: String,
    pub session_id: String,
    pub source: String,
    pub observed_at_ms: i64,
    pub complete: bool,
    pub limitations: Vec<String>,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub provider: String,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub estimated_usd_micros: Option<u64>,
    pub estimate_basis: Option<String>,
    pub credits: Option<Amount>,
    pub billed: Option<Amount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    /// Provider-specific credit unit or ISO currency. Never sum unlike units.
    pub unit: String,
    pub micros: u64,
    pub source: String,
}

impl Receipt {
    pub fn read(path: &Path) -> Result<Self> {
        use std::io::Read;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .with_context(|| path.display().to_string())?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "usage receipt must be a regular file");
        // Keep the optional exit hook bounded even if its producer is broken.
        const MAX_RECEIPT: u64 = 8 * 1024 * 1024;
        ensure!(meta.len() <= MAX_RECEIPT, "usage receipt exceeds 8 MiB");
        let mut bytes = Vec::new();
        file.take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECEIPT,
            "usage receipt exceeds 8 MiB"
        );
        let receipt: Self = serde_json::from_slice(&bytes).context("invalid usage receipt")?;
        // Imports stay strict: a field this build does not know is refused,
        // never silently dropped.
        let given: serde_json::Value = serde_json::from_slice(&bytes)?;
        if let Some(field) = unknown_field(&given, &serde_json::to_value(&receipt)?) {
            bail!("invalid usage receipt: unknown field `{field}`");
        }
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == "agent-usage.v1", "unsupported usage schema");
        for (key, value) in [
            ("agent", &self.agent),
            ("session_id", &self.session_id),
            ("source", &self.source),
        ] {
            ensure!(
                !value.trim().is_empty() && value.len() <= 4096,
                "invalid {key}"
            );
        }
        ensure!(self.observed_at_ms > 0, "invalid observation time");
        ensure!(
            self.complete || !self.limitations.is_empty(),
            "incomplete receipts must explain their limitations"
        );
        let mut keys = std::collections::HashSet::new();
        for entry in &self.entries {
            ensure!(
                !entry.provider.is_empty() && !entry.model.is_empty(),
                "missing provider/model (use unknown explicitly)"
            );
            ensure!(
                keys.insert((&entry.provider, &entry.model)),
                "duplicate provider/model"
            );
            if entry.estimated_usd_micros.is_some() {
                ensure!(
                    entry
                        .estimate_basis
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "an estimate needs its rate basis"
                );
            }
            for amount in [&entry.credits, &entry.billed].into_iter().flatten() {
                ensure!(
                    !amount.unit.trim().is_empty() && !amount.source.trim().is_empty(),
                    "amount needs unit and source"
                );
            }
        }
        Ok(())
    }
}

/// The first field in `given` that `known` (the same receipt as this build
/// reads it back) does not have, at any depth.
fn unknown_field(given: &serde_json::Value, known: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match (given, known) {
        (Value::Object(given), Value::Object(known)) => {
            given.iter().find_map(|(key, value)| match known.get(key) {
                None => Some(key.clone()),
                Some(known) => unknown_field(value, known),
            })
        }
        (Value::Array(given), Value::Array(known)) => given
            .iter()
            .zip(known)
            .find_map(|(given, known)| unknown_field(given, known)),
        _ => None,
    }
}

pub fn merge(receipts: &mut Vec<Receipt>, incoming: Receipt) -> Result<()> {
    if let Some(old) = receipts
        .iter_mut()
        .find(|r| r.agent == incoming.agent && r.session_id == incoming.session_id)
    {
        if *old == incoming {
            return Ok(());
        }
        if incoming.observed_at_ms <= old.observed_at_ms {
            bail!("stale or conflicting receipt; existing snapshot was kept");
        }
        *old = incoming;
    } else {
        receipts.push(incoming);
    }
    Ok(())
}

// ---- the agent's own sessions (D7) -------------------------------------------

/// The words of a launch prompt that name its ohjaa session. The briefing's
/// lifecycle part says them, toimija hands the briefing to the agent as its
/// launch prompt, and the agent keeps that prompt in its transcript.
const HANDSHAKE: &str = "This session is ";

/// The handshake naming `session`, as the briefing says it. A semicolon
/// follows it there, so one id is never read as the start of a longer one.
pub fn handshake(session: &str) -> String {
    format!("{HANDSHAKE}{session}")
}

/// The agent's own sessions a session ran, found at its leaving (or at the
/// launch that closed it as orphaned) in the agent's transcripts: each native
/// session whose transcript holds this session's launch prompt, or why none
/// is named. Found by ohjaa, never the agent's word, and never a guess: a
/// transcript that does not hold the prompt is not named, however well its
/// time and folder fit, and two that each hold it name neither.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Native {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<NativeSession>,
    /// Why none is named; beside named ones, what could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One native session, named as the agent (and vessa) names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSession {
    /// `claude`, `codex` or `pi`.
    pub agent: String,
    /// The agent's id for it. A subagent's work is its parent's: Claude's
    /// subagent transcripts are never read, and a Codex subagent's names its
    /// root thread.
    pub session_id: String,
    /// The transcript it was found in.
    pub source: String,
    /// It began before this session was launched: this session resumed it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
    /// A fork of the native session named. Either this session's launch
    /// prompt is in it, or a copy of it is and the fork began in this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
}

impl Native {
    fn none(why: impl Into<String>) -> Native {
        Native {
            sessions: Vec::new(),
            why: Some(why.into()),
        }
    }
}

/// The agents whose transcripts ohjaa reads, by the basename of the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    fn of(command: &str) -> Option<Agent> {
        match Path::new(command).file_name()?.to_str()? {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "pi" => Some(Agent::Pi),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Pi => "pi",
        }
    }
}

/// Where each agent keeps its transcripts, as the agent finds them itself:
/// its own variable, else its folder in `HOME`.
#[derive(Debug, Clone, Default)]
pub struct Stores {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub pi: Option<PathBuf>,
}

impl Stores {
    pub fn from_env() -> Stores {
        let var = |key: &str| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let home = var("HOME");
        let below = |folder: &str| home.as_ref().map(|home| home.join(folder));
        Stores {
            claude: var("CLAUDE_CONFIG_DIR").or_else(|| below(".claude")),
            codex: var("CODEX_HOME").or_else(|| below(".codex")),
            pi: var("PI_CODING_AGENT_DIR").or_else(|| below(".pi/agent")),
        }
    }

    /// The agent's transcripts written since `since_ms`, and what could not
    /// be looked in. Claude's are `projects/<folder>/<id>.jsonl` (a
    /// subagent's, below `<id>/`, are never read); Codex's
    /// `sessions/<y>/<m>/<d>/rollout-….jsonl`; pi's `sessions/<folder>/….jsonl`.
    fn transcripts(&self, agent: Agent, since_ms: i64) -> (Vec<PathBuf>, Vec<String>) {
        let (root, depth) = match agent {
            Agent::Claude => (self.claude.as_ref().map(|r| r.join("projects")), 2),
            Agent::Codex => (self.codex.as_ref().map(|r| r.join("sessions")), 4),
            Agent::Pi => (self.pi.as_ref().map(|r| r.join("sessions")), 2),
        };
        let (mut found, mut problems) = (Vec::new(), Vec::new());
        match root {
            Some(root) => walk(&root, depth, since_ms, &mut found, &mut problems),
            None => problems.push("HOME is not set".to_owned()),
        }
        found.sort();
        (found, problems)
    }
}

/// The `.jsonl` files at most `depth` levels below `dir` modified since
/// `since_ms`. A folder that does not exist holds none; one that cannot be
/// read is a problem, never an empty folder.
fn walk(
    dir: &Path,
    depth: usize,
    since_ms: i64,
    found: &mut Vec<PathBuf>,
    problems: &mut Vec<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => return problems.push(format!("{}: {error}", dir.display())),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                problems.push(format!("{}: {error}", dir.display()));
                continue;
            }
        };
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            problems.push(format!("{}: cannot tell what it is", path.display()));
            continue;
        };
        if kind.is_dir() {
            if depth > 1 {
                walk(&path, depth - 1, since_ms, found, problems);
            }
            continue;
        }
        if !kind.is_file() || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        // A time that cannot be read cannot rule the file out.
        let modified_ms = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|at| at.as_millis() as i64);
        if modified_ms.is_none_or(|at| at >= since_ms) {
            found.push(path);
        }
    }
}

/// What one transcript says of itself, and the launch prompts in it that
/// name an ohjaa session.
#[derive(Debug, Default)]
struct Transcript {
    id: Option<String>,
    cwd: Option<String>,
    /// Its own first record: a fork's copies of its parent's are not its.
    began_ms: Option<i64>,
    forked_from: Option<String>,
    prompts: Vec<Prompt>,
}

#[derive(Debug)]
struct Prompt {
    session: String,
    at_ms: Option<i64>,
    /// A fork's copy of its parent's record: Claude marks it `forkedFrom`;
    /// a Codex or pi fork's copies are older than the fork itself.
    copied: bool,
}

fn text_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn time_of(value: &Value) -> Option<i64> {
    value.as_str().and_then(crate::intake::parse_time)
}

/// Read a transcript line by line: whole, since a resumed session's launch
/// prompt can be anywhere in it, but parsing only the lines that say what
/// it is and those that name a session.
fn read_transcript(agent: Agent, path: &Path) -> std::io::Result<Transcript> {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut transcript = Transcript::default();
    let mut bytes = Vec::new();
    let mut first = true;
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        let header = std::mem::take(&mut first);
        let Ok(line) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let names = line.contains(HANDSHAKE);
        let wanted = match agent {
            // Claude has no header: each record carries the session, and the
            // first with a time and a folder says where and when it began.
            Agent::Claude => {
                let copy = line.contains("\"forkedFrom\":{");
                transcript.id.is_none()
                    || transcript.cwd.is_none()
                    || (copy && transcript.forked_from.is_none())
                    || (!copy && transcript.began_ms.is_none())
            }
            // Codex and pi open with a header record.
            Agent::Codex | Agent::Pi => header,
        };
        if !names && !wanted {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let copied = match agent {
            Agent::Claude => {
                let copied = !record["forkedFrom"].is_null();
                transcript.id = transcript.id.take().or(text_of(&record["sessionId"]));
                transcript.cwd = transcript.cwd.take().or(text_of(&record["cwd"]));
                if copied {
                    let from = text_of(&record["forkedFrom"]["sessionId"]);
                    transcript.forked_from = transcript.forked_from.take().or(from);
                } else {
                    let at = time_of(&record["timestamp"]);
                    transcript.began_ms = transcript.began_ms.or(at);
                }
                copied
            }
            Agent::Codex if header && record["type"] == "session_meta" => {
                let meta = &record["payload"];
                // A subagent's work is its parent's: its root thread.
                let subagent = !meta["source"]["subagent"].is_null();
                transcript.id = text_of(&meta[if subagent { "session_id" } else { "id" }]);
                transcript.cwd = text_of(&meta["cwd"]);
                transcript.began_ms =
                    time_of(&meta["timestamp"]).or_else(|| time_of(&record["timestamp"]));
                transcript.forked_from = text_of(&meta["forked_from_id"]);
                false
            }
            Agent::Pi if header && record["type"] == "session" => {
                transcript.id = text_of(&record["id"]);
                transcript.cwd = text_of(&record["cwd"]);
                transcript.began_ms = time_of(&record["timestamp"]);
                transcript.forked_from = text_of(&record["parentSession"]);
                false
            }
            Agent::Codex | Agent::Pi => {
                let at = time_of(&record["timestamp"]);
                matches!((at, transcript.began_ms), (Some(at), Some(began)) if at < began)
            }
        };
        if !names {
            continue;
        }
        let Some(text) = launch_prompt(agent, &record) else {
            continue;
        };
        for session in named(&text) {
            transcript.prompts.push(Prompt {
                session,
                at_ms: time_of(&record["timestamp"]),
                copied,
            });
        }
    }
    Ok(transcript)
}

/// The words of a record that can be a launch prompt: what the person (or
/// the launcher, for them) gave the agent. Never a tool's output, which can
/// quote anything, another session's briefing included.
fn launch_prompt(agent: Agent, record: &Value) -> Option<String> {
    let joined = |content: &Value, kinds: &[&str]| match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            if !parts
                .iter()
                .all(|part| kinds.contains(&part["type"].as_str().unwrap_or_default()))
            {
                return None;
            }
            let texts: Vec<&str> = parts.iter().filter_map(|p| p["text"].as_str()).collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    };
    match agent {
        Agent::Claude => {
            let person = record["type"] == "user"
                && record["isMeta"] != true
                && record["isSidechain"] != true;
            person
                .then(|| joined(&record["message"]["content"], &["text"]))
                .flatten()
        }
        Agent::Codex => {
            let payload = &record["payload"];
            let given = record["type"] == "response_item"
                && payload["type"] == "message"
                && matches!(payload["role"].as_str(), Some("user" | "developer"));
            given
                .then(|| joined(&payload["content"], &["input_text", "text"]))
                .flatten()
        }
        Agent::Pi => {
            let person = record["type"] == "message" && record["message"]["role"] == "user";
            person
                .then(|| joined(&record["message"]["content"], &["text"]))
                .flatten()
        }
    }
}

/// The ohjaa sessions a launch prompt names: `This session is <id>;`.
fn named(text: &str) -> Vec<String> {
    text.match_indices(HANDSHAKE)
        .filter_map(|(at, _)| {
            let rest = &text[at + HANDSHAKE.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            let id = &rest[..end];
            (rest[end..].starts_with(';') && crate::sessions::is_valid_id(id))
                .then(|| id.to_owned())
        })
        .collect()
}

/// One launch, as leaving knows it.
pub struct Launch<'a> {
    /// The ohjaa session.
    pub session: &'a str,
    /// The command ohjaa launched.
    pub agent: &'a str,
    /// The folder it was launched in.
    pub folder: &'a Path,
    pub started_ms: i64,
    /// When leaving looked: a launch prompt after it is not this session's.
    pub until_ms: i64,
    /// Launched with `--no-briefing`: no launch prompt names it.
    pub unbriefed: bool,
}

/// The agent's own sessions this launch ran. A native session is this
/// launch's when its transcript, written since the launch, holds the launch
/// prompt naming it: a user (for Codex, also a developer) message, never a
/// tool's output, said in the launch folder between the launch and now. The
/// launch mints the session's id and hands it to that one agent only, so the
/// prompt shows the transcript was written by the launched process.
///
/// - Resumed: the transcript began before the launch; named, and said so.
/// - Forked: a fork holds a copy of the prompt. A fork begun in this session
///   that names no other session in a prompt of its own is named too.
/// - Subagents are their parent's: one id.
/// - Two native sessions that each hold the prompt itself: neither is named.
pub fn find_native(launch: &Launch, stores: &Stores) -> Native {
    let Some(agent) = Agent::of(launch.agent) else {
        let name = Path::new(launch.agent)
            .file_name()
            .map_or_else(|| launch.agent.into(), |name| name.to_string_lossy());
        return Native::none(if name == "opencode" {
            "not looked for: opencode keeps its sessions in SQLite, which ohjaa does not read"
                .to_owned()
        } else {
            format!("not looked for: ohjaa reads the transcripts of claude, codex and pi, not {name}")
        });
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    let folder = canonical(launch.folder);
    let within = |at: Option<i64>| {
        at.is_some_and(|at| at >= launch.started_ms && at <= launch.until_ms)
    };
    let (paths, mut problems) = stores.transcripts(agent, launch.started_ms);
    let (mut here, mut own, mut forks) = (0, Vec::new(), Vec::new());
    for path in paths {
        let transcript = match read_transcript(agent, &path) {
            Ok(transcript) => transcript,
            Err(error) => {
                problems.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let (Some(id), Some(cwd)) = (&transcript.id, &transcript.cwd) else {
            continue;
        };
        if id.is_empty() || canonical(Path::new(cwd)) != folder {
            continue;
        }
        here += 1;
        let ours = |copied: bool| {
            transcript
                .prompts
                .iter()
                .any(|p| p.session == launch.session && p.copied == copied && within(p.at_ms))
        };
        let another = transcript
            .prompts
            .iter()
            .any(|p| p.session != launch.session && !p.copied);
        let native = NativeSession {
            agent: agent.name().to_owned(),
            session_id: id.clone(),
            source: path.display().to_string(),
            resumed: transcript.began_ms.is_some_and(|b| b < launch.started_ms),
            forked_from: transcript.forked_from.clone(),
        };
        if ours(false) {
            own.push((transcript.began_ms, native));
        } else if ours(true)
            && !another
            && native.forked_from.is_some()
            && within(transcript.began_ms)
        {
            forks.push((transcript.began_ms, native));
        }
    }
    let unread = (!problems.is_empty()).then(|| {
        let shown: Vec<&str> = problems.iter().take(3).map(String::as_str).collect();
        format!(
            "{} place(s) could not be read: {}{}",
            problems.len(),
            shown.join("; "),
            if problems.len() > 3 { "; …" } else { "" }
        )
    });
    let mut ids: Vec<&str> = own.iter().map(|(_, n)| n.session_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > 1 {
        return Native::none(format!(
            "ambiguous: {} {} sessions each hold its launch prompt ({}); none is named",
            ids.len(),
            agent.name(),
            ids.join(", ")
        ));
    }
    let mut found: Vec<(Option<i64>, NativeSession)> = own.into_iter().chain(forks).collect();
    found.sort_by_key(|(began, _)| *began);
    let mut sessions: Vec<NativeSession> = Vec::new();
    for (_, native) in found {
        if !sessions.iter().any(|s| s.session_id == native.session_id) {
            sessions.push(native);
        }
    }
    if !sessions.is_empty() {
        return Native {
            sessions,
            why: unread,
        };
    }
    let mut why = format!(
        "none found: none of the {here} {} transcript(s) written in its folder since its \
         launch holds its launch prompt",
        agent.name()
    );
    if launch.unbriefed {
        why.push_str("; it was launched with --no-briefing, so no launch prompt names it");
    } else if agent == Agent::Pi {
        why.push_str("; toimija gives pi a launch prompt only where its `agents:` config names one");
    }
    if let Some(unread) = unread {
        why.push_str("; ");
        why.push_str(&unread);
    }
    Native::none(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIPT: &str = r#"{"schema":"agent-usage.v1","agent":"claude","session_id":"n-1",
        "source":"test","observed_at_ms":5,"complete":true,"limitations":[],
        "entries":[{"provider":"anthropic","model":"m","input_tokens":1,"output_tokens":2,
        "cache_read_tokens":null,"cache_write_tokens":null,"estimated_usd_micros":null,
        "estimate_basis":null,"credits":null,"billed":null}]}"#;

    fn with_field(at: &str) -> String {
        match at {
            "receipt" => RECEIPT.replacen("{\"schema\"", "{\"later\":1,\"schema\"", 1),
            _ => RECEIPT.replacen("{\"provider\"", "{\"later\":1,\"provider\"", 1),
        }
    }

    #[test]
    fn a_stored_receipt_keeps_reading_when_a_later_build_adds_a_field() {
        for at in ["receipt", "entry"] {
            let stored: Receipt = serde_json::from_str(&with_field(at))
                .unwrap_or_else(|error| panic!("{at}: {error}"));
            assert_eq!(stored.entries.len(), 1);
        }
    }

    #[test]
    fn an_imported_receipt_with_a_field_it_does_not_know_is_still_refused() {
        let dir = std::env::temp_dir().join(format!("ohjaa-usage-strict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for at in ["receipt", "entry"] {
            let path = dir.join(format!("{at}.json"));
            std::fs::write(&path, with_field(at)).unwrap();
            let error = Receipt::read(&path).unwrap_err();
            assert!(
                format!("{error:#}").contains("unknown field `later`"),
                "{at}: {error:#}"
            );
        }
        let path = dir.join("known.json");
        std::fs::write(&path, RECEIPT).unwrap();
        assert!(Receipt::read(&path).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
