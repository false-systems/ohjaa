//! Imported usage is a source assertion, never an invoice or an observation by ohjaa.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
