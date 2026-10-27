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
    /// Why no transcript named one; beside named ones, what could not be
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Where the launch prompt and the agent's environment named different
    /// sessions ([`Native::with_env`]): said, never resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disagreement: Option<String>,
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
    /// The agent's own environment claimed it, in `ohjaa oriented` or `ohjaa
    /// handover` run from inside the session ([`FromEnv`]). Its `source` is
    /// [`ENV_SOURCE`], and it is `testimony`, when no transcript's launch
    /// prompt named it too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub env: bool,
    /// Only the agent's environment named it: the agent's claim, never
    /// ohjaa's observation (law 3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub testimony: bool,
}

impl Native {
    fn none(why: impl Into<String>) -> Native {
        Native {
            sessions: Vec::new(),
            why: Some(why.into()),
            disagreement: None,
        }
    }

    /// What the transcripts named, with what the agent's own environment
    /// claimed added: each id once. The launch prompt in a transcript is what
    /// ohjaa observed; the environment is the agent's claim (law 3), set by
    /// whatever shell ran `oriented` or `handover`. An id both named is the
    /// observed one, said as claimed too. An id only claimed is added as
    /// testimony, said so. Where the launch prompt named sessions and the
    /// environment claimed only others, the observation is kept, the claim is
    /// not, and the disagreement is said. A `/clear` before the first
    /// read-back looks the same: the launch prompt names the session it
    /// cleared, the environment the one it started.
    pub fn with_env(mut self, env: &[FromEnv]) -> Native {
        if env.is_empty() {
            return self;
        }
        let claimed = |s: &NativeSession| {
            env.iter()
                .any(|e| e.agent == s.agent && e.session_id == s.session_id)
        };
        // A fork is named through its parent, never by a launch prompt of
        // its own: it neither agrees nor disagrees.
        let observed: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.source != ENV_SOURCE && s.forked_from.is_none())
            .map(|s| format!("{} {}", s.agent, s.session_id))
            .collect();
        let agreed = self
            .sessions
            .iter()
            .any(|s| s.source != ENV_SOURCE && s.forked_from.is_none() && claimed(s));
        if !observed.is_empty() && !agreed {
            let claims: Vec<String> = env
                .iter()
                .map(|e| format!("{} {}", e.agent, e.session_id))
                .collect();
            self.disagreement = Some(format!(
                "its launch prompt named {}, and the agent's environment claimed {} instead (a \
                 /clear before its first read-back, or a mistake): the launch prompt's is kept, \
                 the claim is not",
                observed.join(", "),
                claims.join(", ")
            ));
            return self;
        }
        for named in env {
            match self
                .sessions
                .iter_mut()
                .find(|s| s.agent == named.agent && s.session_id == named.session_id)
            {
                Some(found) => found.env = true,
                None => self.sessions.push(NativeSession {
                    agent: named.agent.clone(),
                    session_id: named.session_id.clone(),
                    source: ENV_SOURCE.to_owned(),
                    resumed: false,
                    forked_from: None,
                    env: true,
                    testimony: true,
                }),
            }
        }
        self.disagreement = None;
        self
    }
}

/// The `source` of a native session only the agent's environment named.
pub const ENV_SOURCE: &str = "env";

/// One of the agent's own sessions as its environment named it: the variable
/// the agent sets in each shell it runs for its tools, read by `ohjaa
/// oriented` or `ohjaa handover` run in one. Claude Code sets
/// `CLAUDE_CODE_SESSION_ID` (its parent's id in a subagent's shell, and the
/// new id after a `/clear`), Codex `CODEX_THREAD_ID`, pi `PI_SESSION_ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FromEnv {
    /// `claude`, `codex` or `pi`.
    pub agent: String,
    pub session_id: String,
    /// When it was first named; named again, it keeps that time.
    pub at_ms: i64,
}

impl Agent {
    /// The variable the agent names its own session with in its tools'
    /// shells.
    fn session_var(self) -> &'static str {
        match self {
            Agent::Claude => "CLAUDE_CODE_SESSION_ID",
            Agent::Codex => "CODEX_THREAD_ID",
            Agent::Pi => "PI_SESSION_ID",
        }
    }
}

/// The agent's own session as the environment names it, for a session that
/// launched `command`: only that agent's variable is read, since an agent
/// started from another agent's shell inherits that one's. `Ok(None)`: the
/// variable is not set (the agent sets none, or this is no tool shell of
/// it); `Err`: it is set to something that is no session id.
pub fn from_env(
    command: &str,
    at_ms: i64,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Option<FromEnv>, String> {
    let Some(agent) = Agent::of(command) else {
        return Ok(None);
    };
    let key = agent.session_var();
    let Some(value) = var(key).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !crate::sessions::is_valid_id(&value) {
        return Err(format!(
            "{key} is set, but not to a session id: not recorded"
        ));
    }
    Ok(Some(FromEnv {
        agent: agent.name().to_owned(),
        session_id: value,
        at_ms,
    }))
}

/// Add `found` to what the environment named, once: `false` when it was
/// named already.
pub fn record_env(named: &mut Vec<FromEnv>, found: FromEnv) -> bool {
    if named
        .iter()
        .any(|n| n.agent == found.agent && n.session_id == found.session_id)
    {
        return false;
    }
    named.push(found);
    true
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

/// How long after the launch a resumed transcript's launch prompt may be
/// said. The agent submits its launch prompt as it starts, so this is
/// seconds; a message into an older session later is no resume.
const RESUME_SLACK_MS: i64 = 60_000;

/// What one transcript says of itself and of the launch it was given.
#[derive(Debug, Default)]
struct Transcript {
    id: Option<String>,
    cwd: Option<String>,
    /// Its own first record: a fork's copies of its parent's are not its.
    began_ms: Option<i64>,
    forked_from: Option<String>,
    /// The session its launch prompt names.
    launched: Option<String>,
    /// For a Codex or pi fork, every session a prompt in it names. Codex
    /// copies a parent's records into a fork restamped and unmarked, so no
    /// prompt in such a fork can be told to be its own launch prompt.
    fork_names: Vec<String>,
}

/// What a search read, to keep it bounded.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Reads {
    /// Transcripts opened: every one written since the launch.
    opened: usize,
    /// Lines read, across them all.
    lines: usize,
}

fn text_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn time_of(value: &Value) -> Option<i64> {
    value.as_str().and_then(crate::intake::parse_time)
}

/// Whether a record is the agent answering: the end of what it was given at
/// its launch.
fn is_reply(agent: Agent, record: &Value) -> bool {
    match agent {
        Agent::Claude => record["type"] == "assistant",
        // The model's own output, and what follows a call it made. Codex's
        // own additions before the launch prompt (its tools, instructions)
        // are not replies.
        Agent::Codex => {
            let payload = &record["payload"];
            let kind = payload["type"].as_str().unwrap_or_default();
            record["type"] == "response_item"
                && (payload["role"] == "assistant"
                    || matches!(kind, "reasoning" | "agent_message" | "compaction")
                    || kind.ends_with("_call")
                    || kind.ends_with("_call_output"))
        }
        Agent::Pi => {
            record["type"] == "message"
                && matches!(
                    record["message"]["role"].as_str(),
                    Some("assistant" | "toolResult")
                )
        }
    }
}

/// Read a transcript only as far as it must be read:
///
/// - Its header first: Codex's and pi's first record, Claude's first records
///   with a folder and a time. A header naming another folder ends the read.
/// - Begun at or after the launch: from its start to the agent's first reply.
///   Its launch prompt is the first record before that reply that names a
///   session.
/// - Begun before the launch, it can only have been resumed by it. Only the
///   lines naming this session are parsed, and the launch prompt must be said
///   within [`RESUME_SLACK_MS`] of the launch.
/// - A Codex or pi fork is read through, for every session it names.
///
/// `Ok(None)`: the transcript is another folder's.
fn read_transcript(
    agent: Agent,
    path: &Path,
    launch: &Launch,
    here: &dyn Fn(&str) -> bool,
    reads: &mut Reads,
) -> std::io::Result<Option<Transcript>> {
    use std::io::BufRead;
    #[derive(PartialEq)]
    enum Mode {
        Header,
        Launch,
        Resumed,
        Fork,
    }
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    reads.opened += 1;
    let ours = format!("{};", handshake(launch.session));
    let mut transcript = Transcript::default();
    let mut mode = Mode::Header;
    let mut bytes = Vec::new();
    let mut first = true;
    loop {
        bytes.clear();
        if reader.read_until(b'\n', &mut bytes)? == 0 {
            break;
        }
        reads.lines += 1;
        let header = std::mem::take(&mut first);
        let Ok(line) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let wanted = match mode {
            // Claude marks a fork's copies: they say nothing of the fork's
            // own launch, only where it came from.
            Mode::Header | Mode::Launch if agent == Agent::Claude => {
                !line.contains("\"forkedFrom\":{")
                    || transcript.cwd.is_none()
                    || transcript.forked_from.is_none()
            }
            Mode::Header | Mode::Launch => true,
            Mode::Resumed => line.contains(&ours),
            Mode::Fork => line.contains(HANDSHAKE),
        };
        if !wanted {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let copied = agent == Agent::Claude && !record["forkedFrom"].is_null();
        if mode == Mode::Header {
            match agent {
                Agent::Claude => {
                    transcript.id = transcript.id.take().or(text_of(&record["sessionId"]));
                    transcript.cwd = transcript.cwd.take().or(text_of(&record["cwd"]));
                    if copied {
                        let from = text_of(&record["forkedFrom"]["sessionId"]);
                        transcript.forked_from = transcript.forked_from.take().or(from);
                    } else {
                        let at = time_of(&record["timestamp"]);
                        transcript.began_ms = transcript.began_ms.or(at);
                    }
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
                }
                Agent::Pi if header && record["type"] == "session" => {
                    transcript.id = text_of(&record["id"]);
                    transcript.cwd = text_of(&record["cwd"]);
                    transcript.began_ms = time_of(&record["timestamp"]);
                    transcript.forked_from = text_of(&record["parentSession"]);
                }
                // A Codex or pi transcript that does not open with its header
                // is not one ohjaa can read.
                Agent::Codex | Agent::Pi => return Ok(None),
            }
            if let Some(cwd) = &transcript.cwd {
                if !here(cwd) {
                    return Ok(None);
                }
            }
            let known = transcript.id.is_some()
                && transcript.cwd.is_some()
                && (agent != Agent::Claude || transcript.began_ms.is_some());
            if !known {
                continue;
            }
            mode = if agent != Agent::Claude && transcript.forked_from.is_some() {
                Mode::Fork
            } else if transcript
                .began_ms
                .is_some_and(|began| began < launch.launched_ms)
            {
                Mode::Resumed
            } else {
                Mode::Launch
            };
            if agent != Agent::Claude {
                // Codex's and pi's header is no message.
                continue;
            }
        }
        let names = || {
            launch_prompt(agent, &record)
                .map(|text| named(&text))
                .unwrap_or_default()
        };
        match mode {
            Mode::Header => {}
            Mode::Launch if copied => {}
            Mode::Launch => {
                if is_reply(agent, &record) {
                    break;
                }
                if let Some(first) = names().into_iter().next() {
                    transcript.launched = Some(first);
                    break;
                }
            }
            Mode::Resumed => {
                let at = time_of(&record["timestamp"]);
                let resumed = at.is_some_and(|at| {
                    at >= launch.launched_ms && at <= launch.launched_ms + RESUME_SLACK_MS
                });
                if !copied && resumed && names().iter().any(|s| s == launch.session) {
                    transcript.launched = Some(launch.session.to_owned());
                    break;
                }
            }
            Mode::Fork => {
                for session in names() {
                    if !transcript.fork_names.contains(&session) {
                        transcript.fork_names.push(session);
                    }
                }
            }
        }
    }
    Ok(Some(transcript))
}

/// What the agent's harness writes as a person's message, and is not one: a
/// local command's output, a shell's, a slash command's echo.
const HARNESS: &[&str] = &["<local-command-", "<bash-", "<command-"];

/// The words of a record that can be a launch prompt: what the person (or
/// the launcher, for them) gave the agent. Never a tool's output, which can
/// quote anything, another session's briefing included, nor what the harness
/// writes in the person's place: a compaction summary, a command's output.
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
    let text = match agent {
        Agent::Claude => {
            let person = record["type"] == "user"
                && record["isMeta"] != true
                && record["isSidechain"] != true
                && record["isCompactSummary"] != true;
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
    }?;
    let start = text.trim_start();
    (!HARNESS.iter().any(|tag| start.starts_with(tag))).then_some(text)
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

/// One launch, as leaving (or the launch closing it as orphaned) knows it.
pub struct Launch<'a> {
    /// The ohjaa session.
    pub session: &'a str,
    /// The command ohjaa launched.
    pub agent: &'a str,
    /// The folder it was launched in.
    pub folder: &'a Path,
    /// The second it was launched in: the transcripts written since are
    /// read (a file's time may be kept in whole seconds).
    pub started_ms: i64,
    /// The launch's own instant, in milliseconds: what a transcript's own
    /// times are compared with. One begun before it was resumed by it, and
    /// that one's launch prompt, or a fork's start, is at or after it.
    pub launched_ms: i64,
    /// The last time it was seen active: at leaving, now; at an orphan close,
    /// the latest time its record shows. A fork begun after it is not its.
    pub last_seen_ms: i64,
    /// Launched with `--no-briefing`: no launch prompt names it.
    pub unbriefed: bool,
}

/// The agent's own sessions this launch ran. A native session is this
/// launch's when its transcript, in the launch folder and written since the
/// launch, was given a launch prompt that names this session: the first
/// person's (for Codex, also developer) message naming a session before the
/// agent's first reply. Never a tool's output, a compaction summary, a
/// command's output or a later message. The launch mints the session's id and
/// hands it to that one agent only, so its launch prompt shows the
/// transcript was written by the launched process.
///
/// - Resumed: the transcript began before the launch, and the launch prompt
///   naming this session was said at the launch. Named, and said so.
/// - Forked: a fork of a native session named here, begun before the session
///   was last seen, whose own launch names no other session. A fork another
///   launch made is that launch's.
/// - Subagents are their parent's: one id.
/// - Two native sessions whose launch prompts name it: neither is named.
pub fn find_native(launch: &Launch, stores: &Stores) -> Native {
    search(launch, stores).0
}

fn search(launch: &Launch, stores: &Stores) -> (Native, Reads) {
    let mut reads = Reads::default();
    let Some(agent) = Agent::of(launch.agent) else {
        let name = Path::new(launch.agent)
            .file_name()
            .map_or_else(|| launch.agent.into(), |name| name.to_string_lossy());
        let why = if name == "opencode" {
            "not looked for: opencode keeps its sessions in SQLite, which ohjaa does not read"
                .to_owned()
        } else {
            format!(
                "not looked for: ohjaa reads the transcripts of claude, codex and pi, not {name}"
            )
        };
        return (Native::none(why), reads);
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.into());
    let folder = canonical(launch.folder);
    let here = |cwd: &str| canonical(Path::new(cwd)) == folder;
    let (paths, mut problems) = stores.transcripts(agent, launch.started_ms);
    let mut read = Vec::new();
    for path in paths {
        match read_transcript(agent, &path, launch, &here, &mut reads) {
            Ok(Some(transcript)) => {
                if transcript.id.as_ref().is_some_and(|id| !id.is_empty()) {
                    read.push((path, transcript));
                }
            }
            Ok(None) => {}
            Err(error) => problems.push(format!("{}: {error}", path.display())),
        }
    }
    let native_of = |path: &Path, t: &Transcript| NativeSession {
        agent: agent.name().to_owned(),
        session_id: t.id.clone().unwrap_or_default(),
        source: path.display().to_string(),
        resumed: t.began_ms.is_some_and(|b| b < launch.launched_ms),
        forked_from: t.forked_from.clone(),
        env: false,
        testimony: false,
    };
    let mut sessions: Vec<NativeSession> = Vec::new();
    let mut own: Vec<(Option<i64>, NativeSession)> = read
        .iter()
        .filter(|(_, t)| t.launched.as_deref() == Some(launch.session))
        .map(|(path, t)| (t.began_ms, native_of(path, t)))
        .collect();
    own.sort_by_key(|(began, _)| *began);
    for (_, native) in own {
        if !sessions.iter().any(|s| s.session_id == native.session_id) {
            sessions.push(native);
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
    if sessions.len() > 1 {
        let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        let why = format!(
            "ambiguous: {} {} sessions were each given its launch prompt ({}); none is named",
            ids.len(),
            agent.name(),
            ids.join(", ")
        );
        return (Native::none(why), reads);
    }
    // Forks of what is named here, oldest first, so a fork of a fork follows.
    let mut forks: Vec<&(PathBuf, Transcript)> = read
        .iter()
        .filter(|(_, t)| {
            let began = t
                .began_ms
                .is_some_and(|b| b >= launch.launched_ms && b <= launch.last_seen_ms);
            let theirs = t.launched.is_some() || t.fork_names.iter().any(|s| s != launch.session);
            t.forked_from.is_some() && began && !theirs
        })
        .collect();
    forks.sort_by_key(|(_, t)| t.began_ms);
    for (path, t) in forks {
        let parent = t.forked_from.as_deref().unwrap_or_default();
        let id = t.id.as_deref().unwrap_or_default();
        if sessions.iter().any(|s| s.session_id == parent)
            && !sessions.iter().any(|s| s.session_id == id)
        {
            sessions.push(native_of(path, t));
        }
    }
    if !sessions.is_empty() {
        return (
            Native {
                sessions,
                why: unread,
                disagreement: None,
            },
            reads,
        );
    }
    let mut why = format!(
        "none found: none of the {} {} transcript(s) written in its folder since its \
         launch was given its launch prompt",
        read.len(),
        agent.name()
    );
    if launch.unbriefed {
        why.push_str("; it was launched with --no-briefing, so no launch prompt names it");
    } else if agent == Agent::Pi {
        why.push_str(
            "; toimija gives pi a launch prompt only where its `agents:` config names one",
        );
    }
    if let Some(unread) = unread {
        why.push_str("; ");
        why.push_str(&unread);
    }
    (Native::none(why), reads)
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

    // ---- the agent's own sessions ------------------------------------------

    use serde_json::json;

    const OURS: &str = "oh-1790000000000-42";
    const STARTED: i64 = 1_790_000_000_000;

    /// A record's time, `ms` after the launch.
    fn at(ms: i64) -> String {
        let ms = STARTED + ms;
        format!(
            "{}.{:03}Z",
            crate::intake::rfc3339(ms.div_euclid(1000)).trim_end_matches('Z'),
            ms.rem_euclid(1000)
        )
    }

    /// The launch prompt the briefing gives, naming `session`.
    fn prompt(session: &str) -> String {
        format!(
            "## Before anything else\nHow this session works (ohjaa):\n{}; its intent is \
             in-1, new.\n",
            handshake(session)
        )
    }

    /// Agents' stores and a launch folder in a sandbox of their own.
    struct Sandbox {
        dir: PathBuf,
        folder: PathBuf,
        stores: Stores,
    }

    impl Sandbox {
        fn new(tag: &str) -> Sandbox {
            let dir = std::env::temp_dir().join(format!(
                "ohjaa-native-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let folder = dir.join("work");
            std::fs::create_dir_all(&folder).unwrap();
            let folder = std::fs::canonicalize(folder).unwrap();
            let stores = Stores {
                claude: Some(dir.join("claude")),
                codex: Some(dir.join("codex")),
                pi: Some(dir.join("pi")),
            };
            Sandbox {
                dir,
                folder,
                stores,
            }
        }

        fn cwd(&self) -> String {
            self.folder.display().to_string()
        }

        fn write(&self, relative: &str, records: &[Value]) -> PathBuf {
            let path = self.dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let lines: String = records.iter().map(|r| format!("{r}\n")).collect();
            std::fs::write(&path, lines).unwrap();
            path
        }

        fn find(&self, agent: &str) -> Native {
            self.find_as(agent, OURS)
        }

        fn find_as(&self, agent: &str, session: &str) -> Native {
            find_native(
                &Launch {
                    session,
                    agent,
                    folder: &self.folder,
                    started_ms: STARTED,
                    launched_ms: STARTED,
                    last_seen_ms: STARTED + 3_600_000,
                    unbriefed: false,
                },
                &self.stores,
            )
        }

        /// A Claude transcript as Claude Code writes one: records with no
        /// time first, then its start, then the launch prompt.
        fn claude(&self, id: &str, began: i64, prompt_at: i64, text: &str) -> PathBuf {
            self.write(
                &format!("claude/projects/-work/{id}.jsonl"),
                &[
                    json!({"type": "mode", "sessionId": id}),
                    json!({"type": "attachment", "sessionId": id, "cwd": self.cwd(),
                           "timestamp": at(began), "isSidechain": false}),
                    json!({"type": "user", "sessionId": id, "cwd": self.cwd(),
                           "timestamp": at(prompt_at), "isSidechain": false,
                           "message": {"role": "user", "content": text}}),
                ],
            )
        }

        fn ids(native: &Native) -> Vec<&str> {
            native
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_briefing_names_its_session_once_and_never_a_longer_id() {
        let said = prompt(OURS);
        assert_eq!(named(&said), [OURS]);
        assert!(named(&said.replace(OURS, &format!("{OURS}7"))) != [OURS]);
        assert!(
            named(&handshake(OURS)).is_empty(),
            "without its semicolon it is not the briefing's"
        );
    }

    #[test]
    fn claude_names_the_transcript_holding_the_launch_prompt_and_never_a_subagent() {
        let sandbox = Sandbox::new("claude");
        let parent = "5e1976e4-71c3-407c-9533-0be05b7e1b70";
        sandbox.claude(parent, 1_000, 1_200, &prompt(OURS));
        // A subagent's transcript, below its parent's, is its parent's work:
        // even holding the prompt, it names no session of its own.
        sandbox.write(
            &format!("claude/projects/-work/{parent}/subagents/agent-a1.jsonl"),
            &[
                json!({"type": "user", "sessionId": parent, "agentId": "a1", "cwd": sandbox.cwd(),
                   "timestamp": at(5_000), "isSidechain": true,
                   "message": {"role": "user", "content": prompt(OURS)}}),
            ],
        );
        // Another session here that read this one's briefing in a tool's
        // output: a quote, not its launch prompt.
        let reader = "0110dda8-f629-4d09-afb6-0b86272ad560";
        sandbox.write(
            &format!("claude/projects/-work/{reader}.jsonl"),
            &[
                json!({"type": "user", "sessionId": reader, "cwd": sandbox.cwd(),
                "timestamp": at(9_000), "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t", "content": prompt(OURS)}]}}),
            ],
        );
        let native = sandbox.find("/usr/local/bin/claude");
        assert_eq!(Sandbox::ids(&native), [parent], "{native:#?}");
        let found = &native.sessions[0];
        assert_eq!(found.agent, "claude");
        assert!(!found.resumed && found.forked_from.is_none(), "{found:#?}");
        assert!(found.source.ends_with(&format!("{parent}.jsonl")));
        assert_eq!(native.why, None);
    }

    #[test]
    fn codex_names_the_rollout_and_a_subagent_folds_into_its_root_thread() {
        let sandbox = Sandbox::new("codex");
        let root = "01a13f11-2653-7a82-8fc1-46bbc7669b3e";
        let meta = |id: &str, source: Value| {
            json!({"type": "session_meta", "timestamp": at(900), "payload": {
                "id": id, "session_id": root, "timestamp": at(800), "cwd": sandbox.cwd(),
                "source": source, "originator": "codex-tui"}})
        };
        let developer = |ms: i64| {
            json!({"type": "response_item", "timestamp": at(ms), "payload": {
                "type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": prompt(OURS)}]}})
        };
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-2026-09-21T12-00-00-{root}.jsonl"),
            &[meta(root, json!("cli")), developer(1_000)],
        );
        sandbox.write(
            "codex/sessions/2026/09/21/rollout-2026-09-21T12-05-00-019a.jsonl",
            &[
                meta(
                    "019a",
                    json!({"subagent": {"thread_spawn": {"parent_thread_id": root, "depth": 1}}}),
                ),
                developer(300_000),
            ],
        );
        let native = sandbox.find("codex");
        assert_eq!(Sandbox::ids(&native), [root], "{native:#?}");
    }

    #[test]
    fn pi_names_the_session_whose_first_message_is_the_launch_prompt() {
        let sandbox = Sandbox::new("pi");
        let id = "01a13c9b-12d8-76df-8f14-1dafced94cfe";
        sandbox.write(
            &format!("pi/sessions/--work--/2026-09-21T12-00-01-000Z_{id}.jsonl"),
            &[
                json!({"type": "session", "version": 3, "id": id, "timestamp": at(1_000),
                       "cwd": sandbox.cwd()}),
                json!({"type": "message", "id": "m1", "parentId": null, "timestamp": at(1_100),
                       "message": {"role": "user", "content": [
                           {"type": "text", "text": prompt(OURS)}]}}),
            ],
        );
        assert_eq!(Sandbox::ids(&sandbox.find("pi")), [id]);
        // Another launch names nothing here, and says why, with pi's limit.
        let other = sandbox.find_as("pi", "oh-1790000000000-43");
        assert!(other.sessions.is_empty());
        let why = other.why.unwrap();
        assert!(
            why.starts_with("none found: none of the 1 pi transcript(s)")
                && why.contains("toimija gives pi a launch prompt only where"),
            "{why}"
        );
    }

    #[test]
    fn two_sessions_that_each_hold_the_launch_prompt_name_neither() {
        let sandbox = Sandbox::new("ambiguous");
        sandbox.claude(
            "aaaaaaaa-0000-0000-0000-000000000001",
            1_000,
            1_200,
            &prompt(OURS),
        );
        sandbox.claude(
            "aaaaaaaa-0000-0000-0000-000000000002",
            2_000,
            2_200,
            &prompt(OURS),
        );
        let native = sandbox.find("claude");
        assert!(native.sessions.is_empty(), "never picked: {native:#?}");
        assert_eq!(
            native.why.as_deref(),
            Some(
                "ambiguous: 2 claude sessions were each given its launch prompt \
                 (aaaaaaaa-0000-0000-0000-000000000001, aaaaaaaa-0000-0000-0000-000000000002); \
                 none is named"
            )
        );
    }

    #[test]
    fn a_resumed_session_is_named_and_said_resumed() {
        let sandbox = Sandbox::new("resumed");
        let id = "b4e4122a-4154-41f2-b642-c1a5632a7f0d";
        // Begun a day before the launch, under another ohjaa session, and
        // resumed by this one: its prompt appended to the same transcript.
        let path = sandbox.claude(id, -86_400_000, -86_399_000, &prompt("oh-1789913600000-7"));
        let mut lines = std::fs::read_to_string(&path).unwrap();
        lines.push_str(&format!(
            "{}\n",
            json!({"type": "user", "sessionId": id, "cwd": sandbox.cwd(),
                   "timestamp": at(3_000), "message": {"role": "user", "content": prompt(OURS)}})
        ));
        std::fs::write(&path, lines).unwrap();
        let native = sandbox.find("claude");
        assert_eq!(Sandbox::ids(&native), [id], "{native:#?}");
        assert!(native.sessions[0].resumed);
    }

    #[test]
    fn a_fork_begun_in_the_session_is_named_beside_its_parent() {
        let sandbox = Sandbox::new("fork");
        let parent = "11e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        sandbox.claude(parent, 1_000, 1_200, &prompt(OURS));
        // A fork copies its parent's records, the launch prompt with them,
        // each marked with where it came from; then records of its own.
        let fork = |id: &str, own_at: i64| {
            let copied = json!({"sessionId": parent, "messageUuid": "u1"});
            sandbox.write(
                &format!("claude/projects/-work/{id}.jsonl"),
                &[
                    json!({"type": "user", "sessionId": id, "cwd": sandbox.cwd(),
                           "timestamp": at(1_200), "forkedFrom": copied,
                           "message": {"role": "user", "content": prompt(OURS)}}),
                    json!({"type": "assistant", "sessionId": id, "cwd": sandbox.cwd(),
                           "timestamp": at(own_at), "message": {"role": "assistant",
                           "content": [{"type": "text", "text": "ok"}]}}),
                ],
            );
        };
        let child = "22e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        fork(child, 600_000);
        // A fork begun after the session ended is another session's.
        fork("33e3a29a-fc6b-40a4-ab3e-3ecf8bb88566", 7_200_000);
        let native = sandbox.find("claude");
        assert_eq!(Sandbox::ids(&native), [parent, child], "{native:#?}");
        assert_eq!(native.sessions[1].forked_from.as_deref(), Some(parent));
        assert!(!native.sessions[1].resumed);
    }

    #[test]
    fn a_codex_fork_is_named_only_as_a_fork_its_copies_restamped() {
        let sandbox = Sandbox::new("codex-fork");
        let parent = "019a0000-0000-7000-8000-000000000001";
        let meta = |id: &str, began: i64, from: Option<&str>| {
            json!({"type": "session_meta", "timestamp": at(began), "payload": {
                "id": id, "session_id": id, "timestamp": at(began), "cwd": sandbox.cwd(),
                "source": "cli", "forked_from_id": from}})
        };
        let developer = |ms: i64| {
            json!({"type": "response_item", "timestamp": at(ms), "payload": {
                "type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": prompt(OURS)}]}})
        };
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-a-{parent}.jsonl"),
            &[meta(parent, 1_000, None), developer(1_100)],
        );
        // As Codex writes a fork: its own header, then its parent's header
        // and records copied after it, every one stamped anew, unmarked.
        let fork = |id: &str, began: i64| {
            sandbox.write(
                &format!("codex/sessions/2026/09/21/rollout-b-{id}.jsonl"),
                &[
                    meta(id, began, Some(parent)),
                    meta(parent, began + 1, None),
                    developer(began + 2),
                ],
            );
        };
        let child = "019a0000-0000-7000-8000-000000000002";
        fork(child, 600_000);
        fork("019a0000-0000-7000-8000-000000000003", 7_200_000);
        let native = sandbox.find("codex");
        assert_eq!(Sandbox::ids(&native), [parent, child], "{native:#?}");
        assert_eq!(native.sessions[1].forked_from.as_deref(), Some(parent));
    }

    /// Review of #55, finding 2: only a transcript's launch prompt names its
    /// session. Another session here holding this one's line anywhere else
    /// (its compaction summary, a local command's or a shell's output, a
    /// paste after its first reply) neither claims this session nor makes
    /// it ambiguous.
    #[test]
    fn only_the_launch_prompt_names_a_claude_session_never_a_later_record() {
        let sandbox = Sandbox::new("claude-later");
        let ours = "aaaaaaaa-0000-0000-0000-0000000000aa";
        sandbox.claude(ours, 1_000, 1_200, &prompt(OURS));
        let line = prompt(OURS);
        let later = [
            json!({"type": "user", "isCompactSummary": true,
                   "message": {"role": "user", "content": format!("Summary: {line}")}}),
            json!({"type": "user", "message": {"role": "user",
                   "content": format!("<local-command-stdout>{line}</local-command-stdout>")}}),
            json!({"type": "user", "message": {"role": "user",
                   "content": format!("<bash-stdout>{line}</bash-stdout><bash-stderr></bash-stderr>")}}),
            json!({"type": "user", "message": {"role": "user",
                   "content": format!("look at this: {line}")}}),
        ];
        for (n, record) in later.into_iter().enumerate() {
            let id = format!("bbbbbbbb-0000-0000-0000-00000000000{n}");
            let other = format!("oh-1790000001000-{n}");
            let mut record = record;
            for (key, value) in [
                ("sessionId", json!(id)),
                ("cwd", json!(sandbox.cwd())),
                ("timestamp", json!(at(900_000))),
            ] {
                record[key] = value;
            }
            sandbox.write(
                &format!("claude/projects/-work/{id}.jsonl"),
                &[
                    json!({"type": "user", "sessionId": id, "cwd": sandbox.cwd(),
                           "timestamp": at(60_000), "message": {"role": "user",
                           "content": prompt(&other)}}),
                    json!({"type": "assistant", "sessionId": id, "cwd": sandbox.cwd(),
                           "timestamp": at(61_000), "message": {"role": "assistant",
                           "content": [{"type": "text", "text": "ok"}]}}),
                    record,
                ],
            );
            assert_eq!(
                Sandbox::ids(&sandbox.find_as("claude", &other)),
                [id.as_str()],
                "each is its own launch's"
            );
        }
        let native = sandbox.find("claude");
        assert_eq!(Sandbox::ids(&native), [ours], "{native:#?}");
    }

    /// Review of #55, finding 2: this session wrote no rollout (quit before
    /// a first turn), and another session's thread holds its line in a
    /// later message. That thread is the other session's alone.
    #[test]
    fn a_paste_into_another_codex_thread_names_nothing_here() {
        let sandbox = Sandbox::new("codex-paste");
        let theirs = "019a0000-0000-7000-8000-0000000000bb";
        let other = "oh-1790000001000-9";
        let message = |role: &str, ms: i64, text: &str| {
            json!({"type": "response_item", "timestamp": at(ms), "payload": {
                "type": "message", "role": role,
                "content": [{"type": if role == "assistant" { "output_text" } else { "input_text" },
                             "text": text}]}})
        };
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-c-{theirs}.jsonl"),
            &[
                json!({"type": "session_meta", "timestamp": at(60_000), "payload": {
                    "id": theirs, "session_id": theirs, "timestamp": at(60_000),
                    "cwd": sandbox.cwd(), "source": "cli"}}),
                message("developer", 60_100, &prompt(other)),
                message("user", 61_000, "go"),
                message("assistant", 62_000, "ok"),
                message("user", 63_000, &format!("this one too: {}", prompt(OURS))),
            ],
        );
        let native = sandbox.find("codex");
        assert!(native.sessions.is_empty(), "{native:#?}");
        assert_eq!(Sandbox::ids(&sandbox.find_as("codex", other)), [theirs]);
    }

    /// Codex gives its own developer and user messages (instructions, the
    /// sandbox's permissions, AGENTS.md, the environment) before the launch
    /// prompt: the launch prompt may sit behind them.
    #[test]
    fn a_codex_launch_prompt_behind_codexs_own_messages_is_found() {
        let sandbox = Sandbox::new("codex-behind");
        let id = "019a0000-0000-7000-8000-0000000000cc";
        let message = |role: &str, ms: i64, text: &str| {
            json!({"type": "response_item", "timestamp": at(ms), "payload": {
                "type": "message", "role": role,
                "content": [{"type": "input_text", "text": text}]}})
        };
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-d-{id}.jsonl"),
            &[
                json!({"type": "session_meta", "timestamp": at(1_000), "payload": {
                    "id": id, "session_id": id, "timestamp": at(1_000),
                    "cwd": sandbox.cwd(), "source": "cli"}}),
                json!({"type": "event_msg", "timestamp": at(1_050),
                       "payload": {"type": "task_started"}}),
                // Real rollouts give the tools before the instructions.
                json!({"type": "response_item", "timestamp": at(1_060), "payload": {
                    "type": "additional_tools", "role": "developer", "tools": []}}),
                message("developer", 1_100, "You are Codex, a coding agent."),
                message(
                    "developer",
                    1_101,
                    "<permissions instructions>…</permissions instructions>",
                ),
                message(
                    "user",
                    1_102,
                    "# AGENTS.md instructions for /work\n<environment_context>…",
                ),
                message("user", 1_103, &prompt(OURS)),
            ],
        );
        assert_eq!(Sandbox::ids(&sandbox.find("codex")), [id]);
    }

    /// ohjaa#64's review: a session begun at 12:00:00.300 and resumed by a
    /// launch at 12:00:00.900. The session's second (`started_ms`, in its id)
    /// is 12:00:00.000, before the transcript began; only the launch's own
    /// instant says the transcript began before it, so it was resumed.
    #[test]
    fn a_session_resumed_in_the_second_it_began_is_named_resumed() {
        let sandbox = Sandbox::new("resumed-same-second");
        let id = "b4e4122a-0000-41f2-b642-c1a5632a7f0d";
        let record = |kind: &str, ms: i64, content: Value| {
            json!({"type": kind, "sessionId": id, "cwd": sandbox.cwd(), "timestamp": at(ms),
                   "isSidechain": false, "message": {"role": kind, "content": content}})
        };
        sandbox.write(
            &format!("claude/projects/-work/{id}.jsonl"),
            &[
                record("user", 300, json!("a question asked without ohjaa")),
                record(
                    "assistant",
                    400,
                    json!([{"type": "text", "text": "an answer"}]),
                ),
                record("user", 950, json!(prompt(OURS))),
            ],
        );
        let native = find_native(
            &Launch {
                session: OURS,
                agent: "claude",
                folder: &sandbox.folder,
                started_ms: STARTED,
                launched_ms: STARTED + 900,
                last_seen_ms: STARTED + 3_600_000,
                unbriefed: false,
            },
            &sandbox.stores,
        );
        assert_eq!(Sandbox::ids(&native), [id], "{native:#?}");
        assert!(native.sessions[0].resumed, "{native:#?}");
    }

    /// A transcript begun before the launch is this session's only when the
    /// launch resumed it: its prompt said at the launch, not a message into
    /// an older session long after.
    #[test]
    fn an_older_session_naming_this_one_long_after_its_launch_did_not_resume_it() {
        let sandbox = Sandbox::new("not-resumed");
        let id = "eeeeeeee-0000-0000-0000-000000000001";
        let path = sandbox.claude(id, -86_400_000, -86_399_000, &prompt("oh-1789913600000-7"));
        let mut lines = std::fs::read_to_string(&path).unwrap();
        lines.push_str(&format!(
            "{}\n",
            json!({"type": "user", "sessionId": id, "cwd": sandbox.cwd(),
                   "timestamp": at(600_000), "message": {"role": "user", "content": prompt(OURS)}})
        ));
        std::fs::write(&path, lines).unwrap();
        let native = sandbox.find("claude");
        assert!(native.sessions.is_empty(), "{native:#?}");
    }

    /// Review of #55, finding 3: a Codex fork made by another ohjaa launch
    /// while this session ran holds this session's prompt, copied and
    /// restamped, and the other launch's own. It is never named here.
    #[test]
    fn a_codex_fork_another_launch_made_is_never_named_here() {
        let sandbox = Sandbox::new("codex-other-fork");
        let parent = "019a0000-0000-7000-8000-0000000000d1";
        let meta = |id: &str, began: i64, from: Option<&str>| {
            json!({"type": "session_meta", "timestamp": at(began), "payload": {
                "id": id, "session_id": id, "timestamp": at(began), "cwd": sandbox.cwd(),
                "source": "cli", "forked_from_id": from}})
        };
        let developer = |ms: i64, session: &str| {
            json!({"type": "response_item", "timestamp": at(ms), "payload": {
                "type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": prompt(session)}]}})
        };
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-e-{parent}.jsonl"),
            &[meta(parent, 1_000, None), developer(1_100, OURS)],
        );
        let fork = "019a0000-0000-7000-8000-0000000000d2";
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-f-{fork}.jsonl"),
            &[
                meta(fork, 600_000, Some(parent)),
                meta(parent, 600_001, None),
                developer(600_002, OURS),
                developer(600_003, "oh-1790000600000-5"),
            ],
        );
        let native = sandbox.find("codex");
        assert_eq!(Sandbox::ids(&native), [parent], "{native:#?}");
    }

    /// Review of #55, finding 3: a Claude fork made by another launch is
    /// that launch's, named there with where it came from, never here.
    #[test]
    fn a_claude_fork_another_launch_made_is_its_own_never_this_ones() {
        let sandbox = Sandbox::new("claude-other-fork");
        let parent = "44e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        sandbox.claude(parent, 1_000, 1_200, &prompt(OURS));
        let fork = "55e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        let other = "oh-1790000600000-5";
        sandbox.write(
            &format!("claude/projects/-work/{fork}.jsonl"),
            &[
                json!({"type": "user", "sessionId": fork, "cwd": sandbox.cwd(),
                       "timestamp": at(1_200), "forkedFrom": {"sessionId": parent,
                       "messageUuid": "u1"},
                       "message": {"role": "user", "content": prompt(OURS)}}),
                json!({"type": "user", "sessionId": fork, "cwd": sandbox.cwd(),
                       "timestamp": at(600_000),
                       "message": {"role": "user", "content": prompt(other)}}),
            ],
        );
        assert_eq!(Sandbox::ids(&sandbox.find("claude")), [parent]);
        let theirs = sandbox.find_as("claude", other);
        assert_eq!(Sandbox::ids(&theirs), [fork], "{theirs:#?}");
        assert_eq!(theirs.sessions[0].forked_from.as_deref(), Some(parent));
    }

    /// Review of #55, finding 3: an orphan's end was never observed, so its
    /// window ends when its record last shows it active, not at the launch
    /// that closes it: a fork begun after that is not its.
    #[test]
    fn an_orphan_names_no_fork_begun_after_it_was_last_seen() {
        let sandbox = Sandbox::new("orphan-fork");
        let parent = "66e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        sandbox.claude(parent, 1_000, 1_200, &prompt(OURS));
        let fork = "77e3a29a-fc6b-40a4-ab3e-3ecf8bb88566";
        sandbox.write(
            &format!("claude/projects/-work/{fork}.jsonl"),
            &[
                json!({"type": "user", "sessionId": fork, "cwd": sandbox.cwd(),
                       "timestamp": at(1_200), "forkedFrom": {"sessionId": parent,
                       "messageUuid": "u1"},
                       "message": {"role": "user", "content": prompt(OURS)}}),
                json!({"type": "assistant", "sessionId": fork, "cwd": sandbox.cwd(),
                       "timestamp": at(5 * 3_600_000), "message": {"role": "assistant",
                       "content": [{"type": "text", "text": "ok"}]}}),
            ],
        );
        let seen = |last_seen_ms: i64| {
            find_native(
                &Launch {
                    session: OURS,
                    agent: "claude",
                    folder: &sandbox.folder,
                    started_ms: STARTED,
                    launched_ms: STARTED,
                    last_seen_ms,
                    unbriefed: false,
                },
                &sandbox.stores,
            )
        };
        // Last seen at its handover, an hour in: the fork is someone's later.
        assert_eq!(Sandbox::ids(&seen(STARTED + 3_600_000)), [parent]);
        // Seen still active after the fork began, it is its own fork.
        assert_eq!(Sandbox::ids(&seen(STARTED + 6 * 3_600_000)), [parent, fork]);
    }

    /// Review of #55, finding 4: a transcript whose header names another
    /// folder is read no further, so leaving reads what is in its folder.
    #[test]
    fn a_transcript_of_another_folder_is_read_no_further_than_its_header() {
        let sandbox = Sandbox::new("bounded");
        let elsewhere = sandbox.dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let elsewhere = elsewhere.display().to_string();
        let filler = |n: usize, ms: i64| {
            (0..n)
                .map(|i| {
                    json!({"type": "response_item", "timestamp": at(ms + i as i64), "payload": {
                        "type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": prompt(OURS)}]}})
                })
                .collect::<Vec<_>>()
        };
        for n in 0..3 {
            let id = format!("019a0000-0000-7000-8000-00000000ff0{n}");
            let mut records = vec![json!({"type": "session_meta", "timestamp": at(1_000),
                "payload": {"id": id, "session_id": id, "timestamp": at(1_000),
                "cwd": elsewhere, "source": "cli"}})];
            records.extend(filler(400, 2_000));
            sandbox.write(
                &format!("codex/sessions/2026/09/21/rollout-x-{id}.jsonl"),
                &records,
            );
        }
        let id = "019a0000-0000-7000-8000-00000000ff10";
        sandbox.write(
            &format!("codex/sessions/2026/09/21/rollout-y-{id}.jsonl"),
            &[
                json!({"type": "session_meta", "timestamp": at(1_000), "payload": {
                    "id": id, "session_id": id, "timestamp": at(1_000), "cwd": sandbox.cwd(),
                    "source": "cli"}}),
                json!({"type": "response_item", "timestamp": at(1_100), "payload": {
                    "type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": prompt(OURS)}]}}),
            ],
        );
        // Claude's header is its first records with a folder and a time.
        for n in 0..2 {
            let other = format!("ffffffff-0000-0000-0000-00000000000{n}");
            let mut records = vec![
                json!({"type": "mode", "sessionId": other}),
                json!({"type": "attachment", "sessionId": other, "cwd": elsewhere,
                       "timestamp": at(1_000)}),
            ];
            records.extend((0..400).map(|i| {
                json!({"type": "user", "sessionId": other, "cwd": elsewhere,
                       "timestamp": at(2_000 + i), "message": {"role": "user",
                       "content": prompt(OURS)}})
            }));
            sandbox.write(
                &format!("claude/projects/-elsewhere/{other}.jsonl"),
                &records,
            );
        }
        let launch = |agent| Launch {
            session: OURS,
            agent,
            folder: &sandbox.folder,
            started_ms: STARTED,
            launched_ms: STARTED,
            last_seen_ms: STARTED + 3_600_000,
            unbriefed: false,
        };
        let (native, reads) = search(&launch("codex"), &sandbox.stores);
        assert_eq!(Sandbox::ids(&native), [id]);
        assert_eq!(
            reads,
            Reads {
                opened: 4,
                lines: 3 + 2
            },
            "one line of each other folder's rollout, and the two of this one"
        );
        let (native, reads) = search(&launch("claude"), &sandbox.stores);
        assert!(native.sessions.is_empty());
        assert_eq!(
            reads,
            Reads {
                opened: 2,
                lines: 2 * 2
            }
        );
    }

    #[test]
    fn a_prompt_outside_the_launch_folder_or_its_time_names_nothing() {
        let sandbox = Sandbox::new("elsewhere");
        let id = "cccccccc-0000-0000-0000-000000000001";
        // Its prompt, said before the launch: another launch's word.
        sandbox.claude(id, -5_000, -4_000, &prompt(OURS));
        // The same prompt in another folder.
        let elsewhere = sandbox.dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        sandbox.write(
            "claude/projects/-elsewhere/dddddddd-0000-0000-0000-000000000001.jsonl",
            &[
                json!({"type": "user", "sessionId": "dddddddd-0000-0000-0000-000000000001",
                "cwd": elsewhere.display().to_string(), "timestamp": at(2_000),
                "message": {"role": "user", "content": prompt(OURS)}}),
            ],
        );
        let native = sandbox.find("claude");
        assert!(native.sessions.is_empty(), "{native:#?}");
        assert!(
            native
                .why
                .unwrap()
                .starts_with("none found: none of the 1 claude transcript(s) written"),
            "only the one in its folder is counted"
        );
    }

    #[test]
    fn an_agent_whose_transcripts_ohjaa_does_not_read_is_said_not_looked_for() {
        let sandbox = Sandbox::new("unread");
        assert_eq!(
            sandbox.find("opencode").why.as_deref(),
            Some(
                "not looked for: opencode keeps its sessions in SQLite, which ohjaa does not read"
            )
        );
        assert_eq!(
            sandbox.find("sh").why.as_deref(),
            Some("not looked for: ohjaa reads the transcripts of claude, codex and pi, not sh")
        );
        // A store never written is no transcript, not a failure.
        assert!(sandbox
            .find("codex")
            .why
            .unwrap()
            .starts_with("none found: none of the 0 codex transcript(s)"));
    }
}

#[cfg(test)]
mod from_env_tests {
    use super::*;

    /// A shell's environment holding only `vars`.
    fn shell<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn found(agent: &str, id: &str, at_ms: i64) -> FromEnv {
        FromEnv {
            agent: agent.to_owned(),
            session_id: id.to_owned(),
            at_ms,
        }
    }

    fn by_prompt(id: &str) -> NativeSession {
        NativeSession {
            agent: "claude".to_owned(),
            session_id: id.to_owned(),
            source: format!("/h/.claude/projects/-r/{id}.jsonl"),
            resumed: false,
            forked_from: None,
            env: false,
            testimony: false,
        }
    }

    fn named(sessions: Vec<NativeSession>, why: Option<&str>) -> Native {
        Native {
            sessions,
            why: why.map(str::to_owned),
            disagreement: None,
        }
    }

    #[test]
    fn each_agent_names_its_own_session_in_its_own_variable() {
        let env = shell(&[
            ("CLAUDE_CODE_SESSION_ID", "c-1"),
            ("CODEX_THREAD_ID", "x-1"),
            ("PI_SESSION_ID", "p-1"),
        ]);
        for (command, agent, id) in [
            ("/usr/local/bin/claude", "claude", "c-1"),
            ("codex", "codex", "x-1"),
            ("pi", "pi", "p-1"),
        ] {
            assert_eq!(
                from_env(command, 7, &env),
                Ok(Some(found(agent, id, 7))),
                "{command}"
            );
        }
        // Only the launched agent's: a codex started from a Claude shell
        // inherits Claude's id, which is not its own.
        let inherited = shell(&[("CLAUDE_CODE_SESSION_ID", "c-1")]);
        assert_eq!(from_env("codex", 7, &inherited), Ok(None));
        // An agent whose variable ohjaa does not know names nothing.
        assert_eq!(from_env("sh", 7, &env), Ok(None));
        assert_eq!(from_env("opencode", 7, &env), Ok(None));
    }

    #[test]
    fn no_variable_names_nothing_and_a_wrong_one_is_said() {
        assert_eq!(from_env("claude", 7, shell(&[])), Ok(None));
        assert_eq!(
            from_env("claude", 7, shell(&[("CLAUDE_CODE_SESSION_ID", "")])),
            Ok(None)
        );
        let refused = from_env(
            "claude",
            7,
            shell(&[("CLAUDE_CODE_SESSION_ID", "../not an id")]),
        )
        .unwrap_err();
        assert!(
            refused.contains("CLAUDE_CODE_SESSION_ID") && refused.contains("not recorded"),
            "{refused}"
        );
    }

    /// `/clear` starts a new session, and Claude Code names the new one in
    /// every shell after it: each id is kept once, first named first.
    #[test]
    fn a_clear_names_a_second_session_and_each_is_kept_once() {
        let mut named_env = Vec::new();
        assert!(record_env(&mut named_env, found("claude", "before", 1_000)));
        assert!(!record_env(
            &mut named_env,
            found("claude", "before", 2_000)
        ));
        assert!(record_env(&mut named_env, found("claude", "after", 3_000)));
        assert_eq!(
            named_env,
            [
                found("claude", "before", 1_000),
                found("claude", "after", 3_000)
            ],
            "named again, an id keeps its first time"
        );
    }

    #[test]
    fn the_environment_and_the_launch_prompt_agreeing_name_one_session() {
        let merged = named(vec![by_prompt("a")], None).with_env(&[found("claude", "a", 1)]);
        assert_eq!(merged.sessions.len(), 1, "{merged:#?}");
        assert!(merged.sessions[0].env);
        assert!(merged.sessions[0].source.ends_with("/a.jsonl"));
        assert_eq!(merged.disagreement, None);
        // After a `/clear`: the first named by both, the second by the
        // environment only. They do not disagree.
        let cleared = named(vec![by_prompt("a")], None)
            .with_env(&[found("claude", "a", 1), found("claude", "b", 2)]);
        let ids: Vec<(&str, &str, bool)> = cleared
            .sessions
            .iter()
            .map(|s| (s.session_id.as_str(), s.source.as_str(), s.env))
            .collect();
        assert_eq!(ids[1], ("b", ENV_SOURCE, true), "{cleared:#?}");
        assert!(ids[0].2);
        assert!(
            cleared.sessions[1].testimony && !cleared.sessions[0].testimony,
            "only what the environment alone named is the agent's claim"
        );
        assert_eq!(cleared.disagreement, None);
    }

    /// The environment is the agent's claim; the launch prompt is what ohjaa
    /// observed. When they name different sessions, the observation is kept
    /// and the claim is said, never taken.
    #[test]
    fn the_launch_prompt_wins_over_an_environment_naming_another_session() {
        let merged = named(vec![by_prompt("a")], None).with_env(&[found("claude", "b", 1)]);
        let ids: Vec<&str> = merged
            .sessions
            .iter()
            .map(|s| s.session_id.as_str())
            .collect();
        assert_eq!(ids, ["a"], "the claim is not taken");
        assert_eq!(
            merged.disagreement.as_deref(),
            Some(
                "its launch prompt named claude a, and the agent's environment claimed claude b \
                 instead (a /clear before its first read-back, or a mistake): the launch \
                 prompt's is kept, the claim is not"
            )
        );
        // Merging again changes nothing.
        assert_eq!(merged.clone().with_env(&[found("claude", "b", 1)]), merged);
    }

    #[test]
    fn the_environment_names_what_no_launch_prompt_did_and_keeps_why() {
        let why = "none found: none of the 1 claude transcript(s) written in its folder since \
                   its launch was given its launch prompt";
        let merged = named(Vec::new(), Some(why)).with_env(&[found("claude", "late", 1)]);
        assert_eq!(merged.sessions.len(), 1);
        assert_eq!(merged.sessions[0].source, ENV_SOURCE);
        assert!(merged.sessions[0].testimony, "the agent's claim, said so");
        assert_eq!(
            merged.why.as_deref(),
            Some(why),
            "what the search found is kept"
        );
        assert_eq!(merged.disagreement, None, "nothing to disagree with");
        // Nothing named from the environment changes nothing.
        let alone = named(vec![by_prompt("a")], None);
        assert_eq!(alone.clone().with_env(&[]), alone);
        // A fork is named through its parent: it neither agrees nor
        // disagrees.
        let mut fork = by_prompt("f");
        fork.forked_from = Some("a".to_owned());
        let merged = named(vec![by_prompt("a"), fork], None).with_env(&[found("claude", "a", 1)]);
        assert_eq!(merged.disagreement, None);
    }

    /// Stored, what only the environment named says `source: "env"`, `env:
    /// true` and `testimony: true`; one found by its launch prompt alone
    /// says none of them.
    #[test]
    fn a_named_session_is_stored_with_where_it_was_named() {
        let mut found_alone = by_prompt("f");
        found_alone.forked_from = Some("a".to_owned());
        let merged = named(vec![by_prompt("a"), found_alone], None)
            .with_env(&[found("claude", "a", 1), found("claude", "b", 2)]);
        let stored = serde_json::to_value(&merged).unwrap();
        assert!(stored["sessions"][1].get("env").is_none(), "{stored:#}");
        assert!(stored["sessions"][1].get("testimony").is_none());
        assert_eq!(stored["sessions"][0]["env"], true);
        assert!(stored["sessions"][0].get("testimony").is_none());
        assert_eq!(stored["sessions"][2]["source"], "env");
        assert_eq!(stored["sessions"][2]["env"], true);
        assert_eq!(stored["sessions"][2]["testimony"], true);
        let disagreeing = named(vec![by_prompt("a")], None).with_env(&[found("claude", "b", 1)]);
        let stored_too = serde_json::to_value(&disagreeing).unwrap();
        assert!(stored_too["disagreement"].is_string());
        let read: Native = serde_json::from_value(stored).unwrap();
        assert_eq!(read, merged);
        // A record #55 wrote reads as before.
        let older: Native = serde_json::from_str(
            r#"{"sessions":[{"agent":"claude","session_id":"a","source":"/t.jsonl"}]}"#,
        )
        .unwrap();
        assert!(!older.sessions[0].env && older.disagreement.is_none());
    }
}
