//! Asking each repo's toimija who is live.
//!
//! Nothing here interprets liveness for itself: `toimija sessions --json` owns
//! that judgment, and a second opinion about it would be a second source of
//! truth. We parse, we time-stamp, we report.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How much of a scan actually happened. Mirrors toimija's own vocabulary.
///
/// The distinction that matters is `Failed` versus an empty `Available`: the
/// first means we could not look, the second means we looked and found nobody.
/// Rendering the first as the second is the failure this whole stack exists to
/// prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Coverage {
    Available,
    Degraded,
    Failed,
}

impl Coverage {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("failed") => Coverage::Failed,
            Some("degraded") => Coverage::Degraded,
            _ => Coverage::Available,
        }
    }
}

/// One session, as toimija reports it.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub worktree: PathBuf,
    pub branch: Option<String>,
    pub intent: String,
    pub scope: Vec<String>,
    pub liveness: String,
    pub pid_check: String,
    pub idle_seconds: i64,
}

impl Session {
    /// Whether this session may still be working the checkout.
    ///
    /// Deliberately *not* `is_active`. Toimija's `stale` conflates two very
    /// different situations — its own rule is
    /// `if pid == Dead || idle > threshold { Stale }`, and it has a test named
    /// `open_marker_past_threshold_is_stale_even_if_pid_alive`. So an agent
    /// thinking hard for ninety minutes is reported exactly like one whose
    /// process is gone.
    ///
    /// Toimija's separate PID check can prove that the recorded process is
    /// gone. Older versions omit it: keep their stale sessions, since silence
    /// alone is not proof of absence.
    pub fn occupies(&self) -> bool {
        self.liveness != "closed" && self.pid_check != "dead"
    }

    /// True when this session's occupancy is inferred rather than observed.
    ///
    /// Anything that is neither `active` nor `closed`: `stale`, a liveness this
    /// build does not recognise, or none at all. Matching only `stale` would
    /// render the others unlabelled and so indistinguishable from an observed
    /// `active` session — presenting inferred occupancy as observed activity,
    /// which is the same overclaim this module exists to avoid, mirrored.
    pub fn is_quiet(&self) -> bool {
        self.liveness != "active" && self.liveness != "closed"
    }
}

/// One repo's scan result.
#[derive(Debug, Clone, Serialize)]
pub struct Scan {
    pub repo: PathBuf,
    pub sessions: Vec<Session>,
    pub warnings: Vec<String>,
    pub coverage: Coverage,
    pub error: Option<String>,
}

impl Scan {
    fn failed(repo: &Path, error: String) -> Self {
        Scan {
            repo: repo.to_path_buf(),
            sessions: Vec::new(),
            warnings: Vec::new(),
            coverage: Coverage::Failed,
            error: Some(error),
        }
    }

    /// Every session that may still be working here. See [`Session::occupies`].
    pub fn occupying(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().filter(|s| s.occupies())
    }
}

#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    sessions: Vec<RawSession>,
    #[serde(default)]
    warnings: Vec<RawWarning>,
    #[serde(default)]
    coverage: RawCoverage,
}

/// Toimija emits `{severity, message}` objects; older versions emitted bare
/// strings. The packet contract requires consumers to tolerate both forms and
/// to ignore shapes they do not recognise, so an unknown warning degrades to
/// its raw JSON rather than failing the whole scan.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawWarning {
    Text(String),
    Structured {
        message: String,
        #[serde(default)]
        severity: Option<String>,
    },
    Unknown(serde_json::Value),
}

impl RawWarning {
    fn render(self) -> String {
        match self {
            RawWarning::Text(text) => text,
            RawWarning::Structured { message, severity } => match severity {
                Some(level) if level != "warning" => format!("[{level}] {message}"),
                _ => message,
            },
            RawWarning::Unknown(value) => value.to_string(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawCoverage {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSession {
    id: String,
    #[serde(default)]
    agent: Vec<String>,
    worktree: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default)]
    liveness: Option<String>,
    #[serde(default)]
    pid_check: Option<String>,
    #[serde(default)]
    last_seen_at: Option<i64>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse(repo: &Path, stdout: &str, at: i64) -> Result<Scan, serde_json::Error> {
    let payload: Payload = serde_json::from_str(stdout)?;
    let sessions = payload
        .sessions
        .into_iter()
        .map(|s| Session {
            id: s.id,
            agent: s
                .agent
                .into_iter()
                .next()
                .unwrap_or_else(|| "unknown".into()),
            worktree: PathBuf::from(s.worktree),
            branch: s.branch,
            intent: s.intent.unwrap_or_default(),
            scope: s.scope,
            liveness: s.liveness.unwrap_or_else(|| "unknown".into()),
            pid_check: s.pid_check.unwrap_or_else(|| "unknown".into()),
            idle_seconds: match s.last_seen_at {
                Some(seen) if seen > 0 => (at - seen).max(0),
                _ => -1,
            },
        })
        .collect();
    Ok(Scan {
        repo: repo.to_path_buf(),
        sessions,
        warnings: payload
            .warnings
            .into_iter()
            .map(RawWarning::render)
            .collect(),
        coverage: Coverage::parse(payload.coverage.status.as_deref()),
        error: None,
    })
}

/// Ask one repo's toimija who is live. Never fails: a failure is itself a fact.
pub fn scan_repo(repo: &Path) -> Scan {
    let output = Command::new("toimija")
        .args(["sessions", "--json"])
        .current_dir(repo)
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => return Scan::failed(repo, format!("cannot run toimija: {error}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr
            .trim()
            .lines()
            .last()
            .unwrap_or("")
            .trim()
            .to_string();
        let detail = if last.is_empty() {
            format!("toimija exited {}", output.status)
        } else {
            last
        };
        return Scan::failed(repo, detail);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse(repo, &stdout, now()) {
        Ok(scan) => scan,
        Err(error) => Scan::failed(repo, format!("unreadable sessions output: {error}")),
    }
}

/// Scan every repo concurrently. Order is preserved so output stays stable.
pub fn scan_fleet(repos: &[PathBuf]) -> Vec<Scan> {
    std::thread::scope(|scope| {
        repos
            .iter()
            .map(|repo| scope.spawn(move || scan_repo(repo)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("scan thread panicked"))
            .collect()
    })
}

/// Render an age without ever claiming freshness it does not have.
pub fn ago(seconds: i64) -> String {
    match seconds {
        s if s < 0 => "never".to_string(),
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{}m ago", s / 60),
        s => format!("{}h ago", s / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "/tmp/r";

    fn payload(sessions: &str) -> String {
        format!(
            r#"{{"sessions":[{sessions}],"warnings":["overlap"],"coverage":{{"status":"available"}}}}"#
        )
    }

    #[test]
    fn active_session_is_parsed() {
        let raw = payload(
            r#"{"id":"1-2","agent":["codex"],"worktree":"/tmp/r","branch":"main",
                "intent":"work","scope":["src/"],"liveness":"active","last_seen_at":90}"#,
        );
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        assert_eq!(scan.coverage, Coverage::Available);
        assert_eq!(scan.warnings, vec!["overlap".to_string()]);
        let session = &scan.sessions[0];
        assert_eq!(session.agent, "codex");
        assert_eq!(session.branch.as_deref(), Some("main"));
        assert_eq!(session.idle_seconds, 10);
        assert!(session.occupies());
    }

    #[test]
    fn missing_optional_fields_do_not_break_parsing() {
        let raw = payload(r#"{"id":"1-2","worktree":"/tmp/r"}"#);
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        let session = &scan.sessions[0];
        assert_eq!(session.agent, "unknown");
        assert_eq!(session.intent, "");
        assert_eq!(session.liveness, "unknown");
        assert_eq!(
            session.idle_seconds, -1,
            "no heartbeat means unknown, not fresh"
        );
        assert!(
            session.occupies(),
            "unknown liveness is not proof of absence"
        );
    }

    #[test]
    fn structured_warnings_are_accepted() {
        // The shape toimija actually emits. A Vec<String> here silently broke
        // four repos until `fleet` reported the failed coverage.
        let raw = r#"{"sessions":[],"warnings":[
            {"severity":"warning","message":"looks stale"},
            {"severity":"error","message":"marker unreadable"},
            "a bare legacy string",
            {"unrecognised":"shape"}
        ],"coverage":{"status":"available"}}"#;
        let scan = parse(Path::new(REPO), raw, 0).unwrap();
        assert_eq!(scan.coverage, Coverage::Available);
        assert_eq!(scan.warnings[0], "looks stale");
        assert_eq!(scan.warnings[1], "[error] marker unreadable");
        assert_eq!(scan.warnings[2], "a bare legacy string");
        assert!(
            scan.warnings[3].contains("unrecognised"),
            "an unknown shape must degrade, never fail the scan"
        );
    }

    #[test]
    fn a_quiet_session_still_occupies_the_checkout() {
        // Toimija reports `stale` for BOTH a dead process and a live one that
        // has simply been quiet past the threshold — its own test is named
        // `open_marker_past_threshold_is_stale_even_if_pid_alive`. An agent
        // thinking for ninety minutes must not read as an empty checkout.
        let raw = payload(
            r#"{"id":"1-2","agent":["codex"],"worktree":"/tmp/r",
                "liveness":"stale","last_seen_at":1}"#,
        );
        let scan = parse(Path::new(REPO), &raw, 100_000).unwrap();
        let session = &scan.sessions[0];
        assert!(session.occupies(), "stale is not proof of absence");
        assert!(session.is_quiet(), "and its uncertainty must be visible");
        assert_eq!(scan.occupying().count(), 1);
    }

    #[test]
    fn a_closed_session_does_not_occupy_the_checkout() {
        // An explicit close needs no process evidence.
        let raw = payload(r#"{"id":"1-2","worktree":"/tmp/r","liveness":"closed"}"#);
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        assert!(!scan.sessions[0].occupies());
        assert_eq!(scan.occupying().count(), 0);
    }

    #[test]
    fn only_confirmed_dead_processes_are_excluded_from_open_sessions() {
        for (pid_check, occupies) in [
            ("dead", false),
            ("alive", true),
            ("unknown", true),
            ("unrecognised", true),
        ] {
            let raw = payload(&format!(
                r#"{{"id":"1-2","worktree":"/tmp/r","liveness":"stale","pid_check":"{pid_check}"}}"#
            ));
            let scan = parse(Path::new(REPO), &raw, 100).unwrap();
            assert_eq!(scan.sessions[0].pid_check, pid_check);
            assert_eq!(scan.occupying().count(), usize::from(occupies));
        }
    }

    #[test]
    fn an_unrecognised_liveness_is_treated_as_occupying() {
        // A verdict this build does not know is not permission to assume the
        // checkout is free.
        let raw = payload(r#"{"id":"1-2","worktree":"/tmp/r","liveness":"suspended"}"#);
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        assert!(scan.sessions[0].occupies());
        assert!(
            scan.sessions[0].is_quiet(),
            "an unrecognised verdict is uncertain, and must not render like an observed active one"
        );
    }

    #[test]
    fn a_missing_liveness_is_uncertain_not_active() {
        let raw = payload(r#"{"id":"1-2","worktree":"/tmp/r"}"#);
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        assert!(scan.sessions[0].occupies());
        assert!(scan.sessions[0].is_quiet());
    }

    #[test]
    fn an_observed_active_session_is_not_marked_uncertain() {
        let raw = payload(r#"{"id":"1-2","worktree":"/tmp/r","liveness":"active"}"#);
        let scan = parse(Path::new(REPO), &raw, 100).unwrap();
        assert!(!scan.sessions[0].is_quiet());
    }

    #[test]
    fn unparseable_output_is_an_error_not_an_empty_scan() {
        assert!(parse(Path::new(REPO), "{not json", 0).is_err());
    }

    #[test]
    fn toimija_reported_failed_coverage_is_preserved() {
        let raw = r#"{"sessions":[],"coverage":{"status":"failed"}}"#;
        let scan = parse(Path::new(REPO), raw, 0).unwrap();
        assert_eq!(scan.coverage, Coverage::Failed);
    }

    #[test]
    fn toimija_reported_degraded_coverage_is_preserved() {
        let raw = r#"{"sessions":[],"coverage":{"status":"degraded"}}"#;
        let scan = parse(Path::new(REPO), raw, 0).unwrap();
        assert_eq!(scan.coverage, Coverage::Degraded);
    }

    #[test]
    fn failed_scan_is_not_an_empty_scan() {
        let scan = Scan::failed(Path::new(REPO), "not a Git repository".into());
        assert_eq!(scan.coverage, Coverage::Failed);
        assert!(scan.sessions.is_empty());
        assert!(scan.error.is_some());
    }

    #[test]
    fn ago_never_claims_freshness_it_lacks() {
        assert_eq!(ago(-1), "never");
        assert_eq!(ago(5), "5s ago");
        assert_eq!(ago(600), "10m ago");
        assert_eq!(ago(7200), "2h ago");
    }
}
