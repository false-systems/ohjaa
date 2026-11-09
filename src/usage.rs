//! Imported usage is a source assertion, never an invoice or an observation by ohjaa.
//!
//! What ohjaa does find itself is which of the agent's own sessions a
//! session ran (D7, [`find_native`]), so a consumer can join the agent's own
//! accounting to it without an imported receipt.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
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
    /// handover` (gone since) run from inside the session ([`FromEnv`]). Its `source` is
    /// [`ENV_SOURCE`], and it is `testimony`, when no transcript's launch
    /// prompt named it too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub env: bool,
    /// Only the agent's environment named it: the agent's claim, never
    /// ohjaa's observation (law 3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub testimony: bool,
}

impl Native {}

/// The `source` of a native session only the agent's environment named.
pub const ENV_SOURCE: &str = "env";

/// One of the agent's own sessions as its environment named it: the variable
/// the agent sets in each shell it runs for its tools, read by `ohjaa
/// oriented` or `ohjaa handover` (gone since) run in one. Claude Code sets
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
