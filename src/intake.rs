//! Intake: turning what people actually said into proposed Knowledge (law 5).
//!
//! A correction a person has to repeat is Knowledge that failed to reach the
//! agent. Intake reads the places corrections happen (agent transcripts, pull
//! request comments, reverts), asks a drafter (an agent CLI such as
//! `claude -p`) which of them carry durable Knowledge, and writes each draft as
//! a *proposal*. Nothing becomes current until a person accepts it.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use faija_knowledge::{Kind, Scope};
use serde::Deserialize;
use serde_json::Value;

use crate::knowledge::{self, day_number, parse_date, Dirs, Loaded, Proposal};
use crate::process;

/// Longer messages are pasted briefs, not corrections.
const MAX_MESSAGE: usize = 1_500;
/// How much of the agent's preceding words travel with a message.
const CONTEXT_TAIL: usize = 600;
const SKIP_PREFIXES: [&str; 12] = [
    "<environment_context>",
    "<user_instructions>",
    "# AGENTS.md",
    "<command-",
    "<local-command",
    "Caveat:",
    "<system-reminder>",
    "[Request interrupted",
    "<task-notification>",
    "<turn_aborted>",
    "This session is being continued",
    "<user_shell_command>",
];

/// Something a person said or did that may carry Knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub source: &'static str,
    /// When, in milliseconds since the epoch. Sources report local offsets,
    /// UTC and fractions of a second; only this number is ever compared.
    pub at_ms: i64,
    /// The same instant as RFC 3339 UTC, for people and the drafter.
    pub at: String,
    /// Where: a repository path, a project, or a pull request URL.
    pub place: String,
    pub who: String,
    /// What the agent said just before, when known.
    pub context: String,
    pub text: String,
}

// ---- time -------------------------------------------------------------------

pub fn rfc3339(seconds: i64) -> String {
    let date = knowledge::date_from_day_number(seconds.div_euclid(86_400));
    let s = seconds.rem_euclid(86_400);
    format!("{date}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Milliseconds since the epoch for `YYYY-MM-DD` (midnight UTC) or an RFC 3339
/// time with `Z` or a `±HH:MM` offset and optional fraction of a second.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let date = parse_date(text)?;
    let days = day_number(date);
    if text.len() == 10 {
        return Some(days * 86_400_000);
    }
    let rest = text.get(10..)?;
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix('t'))?;
    let number = |s: &str| s.parse::<i64>().ok();
    let (hours, minutes, seconds) = (
        number(rest.get(0..2)?)?,
        number(rest.get(3..5)?)?,
        number(rest.get(6..8)?)?,
    );
    if rest.get(2..3)? != ":"
        || rest.get(5..6)? != ":"
        || hours > 23
        || minutes > 59
        || seconds > 60
    {
        return None;
    }
    let mut rest = &rest[8..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let padded = format!("{:0<3}", &fraction[..digits.min(3)]);
        millis = number(&padded)?;
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(0..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.get(3..4)? != ":" {
                return None;
            }
            sign * (number(rest.get(1..3)?)? * 60 + number(rest.get(4..6)?)?)
        }
    };
    let local = ((days * 24 + hours) * 60 + minutes) * 60 + seconds;
    Some((local - offset_minutes * 60) * 1000 + millis)
}

/// The start of an intake window, from `YYYY-MM-DD` or an RFC 3339 time.
pub fn normalize_since(text: &str) -> Result<i64> {
    parse_time(text).with_context(|| format!("`{}` is not a date or RFC 3339 time", text.trim()))
}

// ---- transcripts ------------------------------------------------------------

fn clean(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<system-reminder>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</system-reminder>") {
            Some(end) => rest = &rest[start + end + "</system-reminder>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    let out = out.trim();
    let keep = !out.is_empty()
        && out.chars().count() <= MAX_MESSAGE
        && !SKIP_PREFIXES.iter().any(|prefix| out.starts_with(prefix));
    keep.then(|| out.to_owned())
}

fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(chars)).collect()
}

fn texts(content: &Value, key: &str) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b["type"] == "tool_result") {
                return None;
            }
            let joined: Vec<&str> = blocks
                .iter()
                .filter_map(|block| block[key].as_str())
                .collect();
            (!joined.is_empty()).then(|| joined.join("\n"))
        }
        _ => None,
    }
}

/// One Claude Code transcript (`~/.claude/projects/*/*.jsonl`).
pub fn claude_transcript(text: &str, since_ms: i64, out: &mut Vec<Candidate>) {
    let mut agent_said = String::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry["isSidechain"] == true {
            continue; // a subagent's prompt, not the person
        }
        let content = &entry["message"]["content"];
        match entry["type"].as_str() {
            Some("assistant") => {
                if let Some(said) = texts(content, "text") {
                    agent_said = said;
                }
            }
            Some("user") if entry["isMeta"] != true => {
                // A message that cannot be placed in time cannot be windowed.
                let Some(at_ms) = entry["timestamp"].as_str().and_then(parse_time) else {
                    continue;
                };
                let Some(message) = texts(content, "text").and_then(|t| clean(&t)) else {
                    continue;
                };
                if at_ms > since_ms {
                    out.push(Candidate {
                        source: "claude",
                        at_ms,
                        at: rfc3339(at_ms.div_euclid(1000)),
                        place: entry["cwd"].as_str().unwrap_or("").to_owned(),
                        who: "person".to_owned(),
                        context: tail(&agent_said, CONTEXT_TAIL),
                        text: message,
                    });
                }
            }
            _ => {}
        }
    }
}

/// One Codex session (`~/.codex/sessions/**/*.jsonl`). Subagent sessions,
/// whose "user" is another agent, are skipped whole.
pub fn codex_session(text: &str, since_ms: i64, out: &mut Vec<Candidate>) {
    let mut place = String::new();
    let mut agent_said = String::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &entry["payload"];
        if entry["type"] == "session_meta" {
            if !payload["source"].is_string() {
                return;
            }
            place = payload["cwd"].as_str().unwrap_or("").to_owned();
            continue;
        }
        if payload["type"] != "message" {
            continue;
        }
        let Some(said) = texts(&payload["content"], "text") else {
            continue;
        };
        match payload["role"].as_str() {
            Some("assistant") => agent_said = said,
            Some("user") => {
                let Some(at_ms) = entry["timestamp"].as_str().and_then(parse_time) else {
                    continue;
                };
                if let Some(message) = clean(&said) {
                    if at_ms > since_ms {
                        out.push(Candidate {
                            source: "codex",
                            at_ms,
                            at: rfc3339(at_ms.div_euclid(1000)),
                            place: place.clone(),
                            who: "person".to_owned(),
                            context: tail(&agent_said, CONTEXT_TAIL),
                            text: message,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn jsonl_files(dir: &Path, modified_after: i64, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            jsonl_files(&path, modified_after, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            let recent = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .is_none_or(|age| age.as_secs() as i64 >= modified_after);
            if recent {
                out.push(path);
            }
        }
    }
}

/// A transcript that cannot be read is said out loud: skipping it silently
/// would move the cursor past corrections nobody ever looked at.
pub fn transcripts(home: &Path, since_ms: i64, warnings: &mut Vec<String>) -> Vec<Candidate> {
    let floor = since_ms.div_euclid(1000);
    let mut out = Vec::new();
    let mut files = Vec::new();
    jsonl_files(&home.join(".claude").join("projects"), floor, &mut files);
    for path in files.drain(..) {
        match std::fs::read_to_string(&path) {
            Ok(text) => claude_transcript(&text, since_ms, &mut out),
            Err(error) => warnings.push(format!("cannot read {}: {error}", path.display())),
        }
    }
    jsonl_files(&home.join(".codex").join("sessions"), floor, &mut files);
    for path in files {
        match std::fs::read_to_string(&path) {
            Ok(text) => codex_session(&text, since_ms, &mut out),
            Err(error) => warnings.push(format!("cannot read {}: {error}", path.display())),
        }
    }
    out
}

// ---- pull requests and reverts ---------------------------------------------

/// `owner/name` of a GitHub remote URL.
pub fn github_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !name.is_empty())
        .then(|| format!("{owner}/{name}"))
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = process::run(
        Command::new("git").arg("-C").arg(repo).args(args),
        None,
        Duration::from_secs(30),
    )?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), output.failure_detail());
    }
    Ok(output.stdout_text())
}

/// Human comments on pull requests and issues since `since`, through `gh`.
pub fn reviews(repos: &[PathBuf], since_ms: i64, warnings: &mut Vec<String>) -> Vec<Candidate> {
    let since = rfc3339(since_ms.div_euclid(1000));
    let mut out = Vec::new();
    for repo in repos {
        let Some(slug) = git(repo, &["remote", "get-url", "origin"])
            .ok()
            .and_then(|url| github_slug(&url))
        else {
            continue;
        };
        for endpoint in ["pulls/comments", "issues/comments"] {
            let api = format!("repos/{slug}/{endpoint}?since={since}&per_page=100");
            let result = process::run(
                Command::new("gh").args([
                    "api",
                    "--paginate",
                    &api,
                    "--jq",
                    ".[] | [.created_at, .user.login, .user.type, .html_url, .body] | @json",
                ]),
                None,
                Duration::from_secs(60),
            );
            let output = match result {
                Ok(output) if output.status.success() => output,
                Ok(output) => {
                    warnings.push(format!("{slug} {endpoint}: {}", output.failure_detail()));
                    continue;
                }
                Err(error) => {
                    warnings.push(format!("{slug} {endpoint}: {error}"));
                    continue;
                }
            };
            for line in output.stdout_text().lines() {
                let Ok(Value::Array(row)) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let field = |i: usize| row.get(i).and_then(Value::as_str).unwrap_or("");
                let (login, kind) = (field(1), field(2));
                if kind == "Bot" || login.ends_with("[bot]") {
                    continue;
                }
                // GitHub's `since` filters on when a comment was last
                // *updated*; an old comment edited today is not new.
                let Some(at_ms) = parse_time(field(0)).filter(|at| *at > since_ms) else {
                    continue;
                };
                if let Some(text) = clean(field(4)) {
                    out.push(Candidate {
                        source: "review",
                        at_ms,
                        at: rfc3339(at_ms.div_euclid(1000)),
                        place: field(3).to_owned(),
                        who: login.to_owned(),
                        context: repo.display().to_string(),
                        text,
                    });
                }
            }
        }
    }
    out
}

/// Reverted commits since `since`: each is an Outcome that went wrong.
pub fn reverts(repos: &[PathBuf], since_ms: i64, warnings: &mut Vec<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for repo in repos {
        // `%at` is seconds since the epoch: no author offset to misorder.
        let log = git(
            repo,
            &[
                "log",
                &format!("--since=@{}", since_ms.div_euclid(1000)),
                "--regexp-ignore-case",
                "--grep=^revert",
                "--max-count=50",
                "--format=%at%x1f%an%x1f%s%x1f%b%x1e",
            ],
        );
        let log = match log {
            Ok(log) => log,
            Err(error) => {
                warnings.push(format!("{}: {error}", repo.display()));
                continue;
            }
        };
        for record in log.split('\u{1e}') {
            let fields: Vec<&str> = record.trim().split('\u{1f}').collect();
            if fields.len() < 4 {
                continue;
            }
            let Some(seconds) = fields[0].trim().parse::<i64>().ok() else {
                continue;
            };
            let at_ms = seconds * 1000;
            if at_ms <= since_ms {
                continue;
            }
            if let Some(text) = clean(&format!("{}\n\n{}", fields[2], fields[3])) {
                out.push(Candidate {
                    source: "revert",
                    at_ms,
                    at: rfc3339(seconds),
                    place: repo.display().to_string(),
                    who: fields[1].to_owned(),
                    context: String::new(),
                    text,
                });
            }
        }
    }
    out
}

// ---- sessions ---------------------------------------------------------------

/// What each ended session's agent said at leaving, as candidates. Its
/// testimony, not a fact: the drafter weighs it, and a person decides.
pub fn handover_candidates(sessions: &[crate::sessions::Session]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for session in sessions {
        let Some(handover) = &session.handover else {
            continue;
        };
        for (label, value) in [
            ("unfinished", &handover.unfinished),
            ("for the next agent", &handover.next),
            ("surprised by", &handover.surprised),
        ] {
            if let Some(value) = value.as_deref().and_then(clean) {
                out.push(Candidate {
                    source: "handover",
                    at_ms: handover.at_ms,
                    at: rfc3339(handover.at_ms.div_euclid(1000)),
                    place: session.repo.clone(),
                    who: session.agent.clone(),
                    context: format!("session {} ({}), its handover", session.id, session.intent),
                    text: format!("{label}: {value}"),
                });
            }
        }
    }
    out
}

/// What ohjaa observed going wrong in each ended session, as candidates: its
/// own commit that sykli refused at the default branch or a pull request head
/// (its flags: never what could not be checked, nor what is not shown to be
/// its); an agent that exited non-zero; a missing handover; a session whose
/// ohjaa was gone before leaving. Observed facts, not anyone's words: the
/// drafter weighs them like any candidate, and a person decides.
pub fn observed_candidates(sessions: &[crate::sessions::Session]) -> Vec<Candidate> {
    use crate::sessions::Exit;
    let mut out = Vec::new();
    for session in sessions {
        let Some(left) = &session.left else {
            continue;
        };
        // Only the session's own refused work is a lesson: a broken tip it is
        // not shown to have pushed, or one never checked, is said, not taught.
        let mut seen: Vec<String> = crate::sessions::flags(session)
            .into_iter()
            .filter(|flag| flag.own)
            .map(|flag| format!("{} ({})", flag.headline, flag.why))
            .collect();
        if left.orphaned {
            seen.push(
                "its ohjaa was gone before leaving; the session was closed as orphaned at \
                 a later launch, and nothing it did was observed"
                    .to_owned(),
            );
        } else {
            match left.exit {
                Exit::Code { code: 0 } => {}
                Exit::Code { code } => seen.push(format!("the agent exited with code {code}")),
                Exit::Signal { signal } => {
                    seen.push(format!("the agent was killed by signal {signal}"))
                }
                Exit::Unknown => seen.push("the agent's exit could not be observed".to_owned()),
            }
            if session.handover.is_none() {
                seen.push("the agent left no handover".to_owned());
            }
        }
        for what in seen {
            out.push(Candidate {
                source: "observed",
                at_ms: left.ended_ms,
                at: rfc3339(left.ended_ms.div_euclid(1000)),
                place: session.repo.clone(),
                who: "ohjaa".to_owned(),
                context: format!(
                    "session {} ({}, {}), observed by ohjaa, not said by anyone",
                    session.id, session.agent, session.intent
                ),
                text: format!("in session {}: {what}", session.id),
            });
        }
    }
    out
}

/// Whether a transcript candidate falls inside this session: its time window,
/// in its repository (or below it).
fn in_window(candidate: &Candidate, session: &crate::sessions::Session) -> bool {
    let ended = session.left.as_ref().map_or(i64::MAX, |left| left.ended_ms);
    candidate.at_ms >= session.started_ms
        && candidate.at_ms <= ended
        && Path::new(&candidate.place).starts_with(&session.repo)
}

/// The transcript candidates that fall inside one of these sessions.
pub fn within_sessions(
    window: Vec<Candidate>,
    sessions: &[crate::sessions::Session],
) -> Vec<Candidate> {
    window
        .into_iter()
        .filter(|candidate| sessions.iter().any(|session| in_window(candidate, session)))
        .collect()
}

/// Every candidate found that is this session's: what was said in its window
/// and repository, its handover, and what ohjaa observed in it. The session
/// is drafted from once each of them has been handled, in however many runs.
pub fn members(session: &crate::sessions::Session, found: &[Candidate]) -> Vec<Key> {
    let own = std::slice::from_ref(session);
    let said = found
        .iter()
        .filter(|c| matches!(c.source, "claude" | "codex") && in_window(c, session));
    handover_candidates(own)
        .iter()
        .chain(&observed_candidates(own))
        .chain(said)
        .map(Candidate::key)
        .collect()
}

// ---- drafting ---------------------------------------------------------------

const INSTRUCTIONS: &str = "\
You maintain a knowledge graph that steers coding agents (Claude, Codex) for the
people they work for. Below are candidates: messages people wrote while working
with agents, comments on pull requests, reverted commits, and failures ohjaa
observed in agent sessions (source \"observed\": work that reached the default
branch without passing evidence, a non-zero exit, a missing handover, a session
cut off). An observed failure is a fact, not anyone's words: cite it as observed,
and draft from it only a lesson that would have prevented it. Find the ones that
carry durable knowledge an agent should have known: a correction of how an agent
behaved, a decision, a fact about the environment, a preference about how work
is done, or a lesson from something that went wrong. Draft each as a node.

Ignore ordinary task instructions (\"fix the test\", \"next\", \"merge it\"),
questions, and anything that only mattered in that moment. Most candidates carry
nothing, and an empty array is a good answer. Never invent: every node must be
supported by the quoted words.

If a candidate corrects something the existing knowledge already covers, that
knowledge did not reach the agent. Draft a lesson that refines the existing id
and says what was missed, instead of a duplicate.

Scopes:
- ours: team-wide decisions and taste that hold in every project
- repo: specific to one project; give its absolute path in \"repo\"
- mine: how this one person likes their agents to work with them
- machine: facts about this computer (installed tools, VMs, paths)

Kinds: decision (chosen, binding), fact (true about the world), taste
(preference about how), lesson (learned from a correction or failure).

Output ONLY a JSON array, no prose, no code fence. Each element:
{\"scope\": \"ours|repo|mine|machine\", \"repo\": \"/absolute/path or null\",
 \"id\": \"kebab-case, at most 64 characters, not an existing id\",
 \"kind\": \"decision|fact|taste|lesson\",
 \"body\": \"one to three plain sentences; imperative where it is a rule\",
 \"origin\": [\"<date> <place>: '<the person's words, under 20 words>'\"],
 \"refines\": \"existing-id or null\", \"supersedes\": []}
";

/// The prompt for one batch of candidates.
pub fn prompt(loaded: &Loaded, batch: &[Candidate]) -> String {
    let mut out = String::from(INSTRUCTIONS);
    out.push_str("\nExisting knowledge:\n");
    if loaded.nodes.is_empty() {
        out.push_str("(none yet)\n");
    }
    for located in &loaded.nodes {
        let node = &located.node;
        let _ = writeln!(
            out,
            "- {} [{}/{}/{}]: {}",
            node.id,
            located.scope.name(),
            node.kind.name(),
            node.status.name(),
            node.body.lines().next().unwrap_or("")
        );
    }
    out.push_str("\nCandidates:\n");
    for (index, candidate) in batch.iter().enumerate() {
        let _ = writeln!(
            out,
            "\n[{}] {} · {} · {} · {}",
            index + 1,
            candidate.at,
            candidate.source,
            candidate.place,
            candidate.who
        );
        if !candidate.context.is_empty() {
            let _ = writeln!(out, "    before: {}", candidate.context.replace('\n', " "));
        }
        let _ = writeln!(out, "    said: {}", candidate.text.replace('\n', " "));
    }
    out
}

/// One node as the drafter proposes it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub scope: String,
    #[serde(default)]
    pub repo: Option<String>,
    pub id: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub origin: Vec<String>,
    #[serde(default)]
    pub refines: Option<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
}

/// The JSON array in a drafter's answer, tolerating prose or a code fence
/// around it.
pub fn parse_drafts(answer: &str) -> Result<Vec<Draft>> {
    let (Some(start), Some(end)) = (answer.find('['), answer.rfind(']')) else {
        bail!("the drafter did not answer with a JSON array");
    };
    if end < start {
        bail!("the drafter did not answer with a JSON array");
    }
    serde_json::from_str(&answer[start..=end]).context("the drafter's array is not valid drafts")
}

impl Draft {
    pub fn into_proposal(self) -> Result<Proposal> {
        let scope = Scope::from_name(&self.scope)
            .with_context(|| format!("`{}` is not a scope", self.scope))?;
        let kind = Kind::from_name(&self.kind)
            .with_context(|| format!("`{}` is not a kind", self.kind))?;
        let repo = match scope {
            Scope::Repo => {
                let repo = self
                    .repo
                    .filter(|path| path.starts_with('/'))
                    .context("a repo draft must name its repository by absolute path")?;
                Some(PathBuf::from(repo))
            }
            _ => None,
        };
        Ok(Proposal {
            scope,
            repo,
            id: self.id,
            kind,
            body: self.body,
            origin: self.origin,
            supersedes: self.supersedes,
            refines: self.refines.filter(|id| !id.is_empty()),
            holds_while: Vec::new(),
        })
    }
}

/// Ask the drafter about one batch: the prompt goes in on stdin.
pub fn draft(drafter: &str, prompt: &str) -> Result<Vec<Draft>> {
    let output = process::run(
        Command::new("sh").args(["-c", drafter]),
        Some(prompt.as_bytes()),
        Duration::from_secs(600),
    )?;
    if !output.status.success() {
        bail!("the drafter failed: {}", output.failure_detail());
    }
    parse_drafts(&output.stdout_text())
}

// ---- the cursor -------------------------------------------------------------

fn cursor_path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("intake-since")
}

/// Where the last intake stopped. An unreadable cursor is an error, never a
/// silent restart from some default.
pub fn read_cursor(dirs: &Dirs) -> Result<Option<i64>> {
    match std::fs::read_to_string(cursor_path(dirs)) {
        Ok(text) => parse_time(&text)
            .map(Some)
            .with_context(|| format!("{} does not hold a time", cursor_path(dirs).display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("cannot read the intake cursor"),
    }
}

/// Record where intake stopped. The cursor only ever moves forward.
pub fn write_cursor(dirs: &Dirs, at_ms: i64) -> Result<()> {
    if read_cursor(dirs)?.is_some_and(|previous| previous >= at_ms) {
        return Ok(());
    }
    std::fs::create_dir_all(&dirs.state)?;
    let at = rfc3339(at_ms.div_euclid(1000));
    // Keep the millisecond, so a message in the same second is not re-read.
    let text = format!("{}.{:03}Z\n", &at[..at.len() - 1], at_ms.rem_euclid(1000));
    std::fs::write(cursor_path(dirs), text)?;
    Ok(())
}

// ---- progress ---------------------------------------------------------------
//
// A run drafts from at most its cap, so a queue can take several runs. Each
// candidate the drafter has answered for is *handled*, and kept so the moment
// its batch is answered (`intake-handled`, beside the cursor): the next run
// continues where this one stopped, a failure keeps what came before it, and
// nothing handled is sent again. The cursor moves only past handled
// candidates, so it never needs to move back.

/// A candidate's identity across runs: when it was said, and a digest of what
/// (the pair duplicates are dropped by). Only the digest is kept on disk,
/// never the words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub at_ms: i64,
    pub digest: u64,
}

impl Candidate {
    pub fn key(&self) -> Key {
        Key {
            at_ms: self.at_ms,
            digest: fnv1a(self.text.as_bytes()),
        }
    }
}

/// 64-bit FNV-1a: the same on every Rust release, unlike `DefaultHasher`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The candidates the drafter has answered for, kept until no later run can
/// find them again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handled(BTreeSet<Key>);

impl Handled {
    pub fn contains(&self, candidate: &Candidate) -> bool {
        self.0.contains(&candidate.key())
    }
}

/// What was found, once: duplicates dropped, oldest first. The order is the
/// order of drafting.
pub fn found(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert((c.at_ms, c.text.clone())));
    candidates.sort_by_key(|candidate| candidate.at_ms);
    candidates
}

/// What one run's drafting came to.
#[derive(Debug, Default)]
pub struct Drafted {
    /// Candidates the drafter answered for in this run.
    pub sent: usize,
    /// Why the run stopped short of its cap, when it did.
    pub stopped: Option<anyhow::Error>,
}

/// Draft from up to `cap` candidates of `found` not handled before, in order,
/// `batch` at a time. Each batch the drafter answered for (`send`) becomes
/// handled and is kept (`keep`) before the next is sent, so a failure loses
/// nothing that came before it.
pub fn draft_pending(
    found: &[Candidate],
    handled: &mut Handled,
    batch: usize,
    cap: usize,
    mut send: impl FnMut(&[Candidate]) -> Result<()>,
    mut keep: impl FnMut(&Handled) -> Result<()>,
) -> Drafted {
    let pending: Vec<Candidate> = found
        .iter()
        .filter(|c| !handled.contains(c))
        .take(cap)
        .cloned()
        .collect();
    let mut drafted = Drafted::default();
    for chunk in pending.chunks(batch.max(1)) {
        if let Err(error) = send(chunk) {
            drafted.stopped = Some(error);
            break;
        }
        handled.0.extend(chunk.iter().map(Candidate::key));
        drafted.sent += chunk.len();
        if let Err(error) = keep(handled) {
            drafted.stopped = Some(error.context("cannot keep intake's progress"));
            break;
        }
    }
    drafted
}

/// A session not yet drafted from, as one run sees it.
#[derive(Debug, Clone)]
pub struct Undrafted {
    pub id: String,
    pub started_ms: i64,
    /// Its candidates among those found, when this run took it on (it has
    /// ended, and the run reads transcripts). It brings them back to every
    /// run until each is handled. `None` while it waits.
    pub members: Option<Vec<Key>>,
    /// The run read its whole window: only then is it drafted from.
    pub whole: bool,
}

/// What a run leaves behind, once it has drafted what it could.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// Where the cursor moves to, when it moves.
    pub cursor: Option<i64>,
    /// The sessions taken on every candidate of which is now handled.
    pub drafted: Vec<String>,
    /// Candidates found and not handled: the next run's work.
    pub left: usize,
}

/// Where a run reads from: the person's `--since`, else the cursor, else
/// `default_ms` back from `started`. A default window is also returned as the
/// cursor to keep before anything is drafted, so that the next run's default
/// does not slide past what this one did not reach, even if this one is
/// killed.
pub fn window_start(
    explicit: Option<i64>,
    cursor: Option<i64>,
    started: i64,
    default_ms: i64,
) -> (i64, Option<i64>) {
    match (explicit, cursor) {
        (Some(since), _) => (since, None),
        (None, Some(cursor)) => (cursor, None),
        (None, None) => {
            let since = started - default_ms;
            (since, Some(since))
        }
    }
}

/// Where a run read from, and how.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    /// The start: the cursor or the person's `--since` (see `window_start`).
    pub since: i64,
    /// When the run started: the furthest the cursor can move.
    pub started: i64,
    /// Every source was read without a warning.
    pub clean: bool,
}

/// Settle a run, and forget what no later run can find again.
///
/// A queued session brings its own candidates back to every run until each
/// is handled. Any other candidate comes back only while it lies after the
/// cursor, so the first of them not handled holds the cursor: it moves past
/// the handled candidates found after `since`, up to that one, or to
/// `started` when none is left and every source was read. It never moves to
/// `since` or before. `sessions` is every session not yet drafted from, or
/// `None` when the log could not be read; then nothing is forgotten.
pub fn settle(
    found: &[Candidate],
    handled: &mut Handled,
    sessions: Option<&[Undrafted]>,
    window: Window,
) -> Settled {
    let Window {
        since,
        started,
        clean,
    } = window;
    let returning: std::collections::HashSet<Key> = sessions
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.members.as_ref())
        .flatten()
        .copied()
        .collect();
    let first_held = found
        .iter()
        .filter(|c| !handled.contains(c) && !returning.contains(&c.key()))
        .map(|c| c.at_ms)
        .min();
    let through = found
        .iter()
        .filter(|c| c.at_ms > since && handled.contains(c))
        .filter(|c| first_held.is_none_or(|first| c.at_ms < first))
        .map(|c| c.at_ms)
        .max();
    let cursor = match first_held {
        None if clean => Some(started),
        _ => through,
    }
    .map(|at| at.min(started))
    .filter(|at| *at > since);
    let drafted: Vec<String> = sessions
        .unwrap_or_default()
        .iter()
        .filter(|s| {
            s.whole
                && s.members
                    .as_ref()
                    .is_some_and(|members| members.iter().all(|key| handled.0.contains(key)))
        })
        .map(|s| s.id.clone())
        .collect();
    let left = found.iter().filter(|c| !handled.contains(c)).count();
    // A key can come back only from a cursor source after the cursor, from a
    // session not yet drafted from (never before it started), or from what
    // this run found. Everything else is forgotten.
    if let (Some(cursor), Some(sessions)) = (cursor, sessions) {
        let floor = sessions
            .iter()
            .filter(|s| !drafted.contains(&s.id))
            .map(|s| s.started_ms)
            .min()
            .unwrap_or(i64::MAX);
        let seen: std::collections::HashSet<Key> = found.iter().map(Candidate::key).collect();
        handled
            .0
            .retain(|key| key.at_ms > cursor || key.at_ms >= floor || seen.contains(key));
    }
    Settled {
        cursor,
        drafted,
        left,
    }
}

fn handled_path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("intake-handled")
}

/// What earlier runs handled. Unreadable progress is an error, never a
/// silent start from nothing that would send everything again.
pub fn read_handled(dirs: &Dirs) -> Result<Handled> {
    let path = handled_path(dirs);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Handled::default()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut keys = BTreeSet::new();
    for line in text.lines() {
        let key = line.split_once(' ').and_then(|(at, digest)| {
            Some(Key {
                at_ms: at.parse().ok()?,
                digest: u64::from_str_radix(digest, 16).ok()?,
            })
        });
        keys.insert(key.with_context(|| {
            format!("{} holds `{line}`, not a handled candidate", path.display())
        })?);
    }
    Ok(Handled(keys))
}

/// Keep what has been handled: whole or not at all (written aside, then
/// renamed into place). Nothing handled leaves no file.
pub fn write_handled(dirs: &Dirs, handled: &Handled) -> Result<()> {
    let path = handled_path(dirs);
    if handled.0.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("cannot remove {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&dirs.state)?;
    let mut text = String::new();
    for key in &handled.0 {
        let _ = writeln!(text, "{} {:016x}", key.at_ms, key.digest);
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, text)
        .and_then(|()| std::fs::rename(&partial, &path))
        .with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_transcripts_keep_the_person_and_drop_everything_else() {
        let lines = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I pushed to main."}]}}"#,
            r#"{"type":"user","timestamp":"2026-09-20T10:00:00Z","cwd":"/p/x","message":{"content":"please dont merge PRs u open ok"}}"#,
            r#"{"type":"user","timestamp":"2026-09-20T10:01:00Z","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","isSidechain":true,"timestamp":"2026-09-20T10:02:00Z","message":{"content":"a subagent prompt"}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-09-20T10:03:00Z","message":{"content":"meta"}}"#,
            r#"{"type":"user","timestamp":"2026-09-20T10:04:00Z","message":{"content":"<system-reminder>x</system-reminder>"}}"#,
            r#"{"type":"user","timestamp":"2026-09-01T10:00:00Z","message":{"content":"too old"}}"#,
            "not json",
        ];
        let mut out = Vec::new();
        claude_transcript(
            &lines.join("\n"),
            parse_time("2026-09-10").unwrap(),
            &mut out,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].text, "please dont merge PRs u open ok");
        assert_eq!(out[0].context, "I pushed to main.");
        assert_eq!(out[0].place, "/p/x");
    }

    #[test]
    fn codex_subagent_sessions_are_skipped_whole() {
        let own = [
            r#"{"type":"session_meta","payload":{"source":"cli","cwd":"/p/y"}}"#,
            r#"{"timestamp":"2026-09-20T10:00:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            r#"{"timestamp":"2026-09-20T10:01:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Blocked on macOS."}]}}"#,
            r#"{"timestamp":"2026-09-20T10:02:00Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"we have lima baba"}]}}"#,
        ];
        let mut out = Vec::new();
        codex_session(&own.join("\n"), parse_time("2026-09-10").unwrap(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "we have lima baba");
        assert_eq!(out[0].context, "Blocked on macOS.");
        assert_eq!(out[0].place, "/p/y");

        let sub = own
            .join("\n")
            .replace(r#""source":"cli""#, r#""source":{"subagent":"review"}"#);
        let mut out = Vec::new();
        codex_session(&sub, parse_time("2026-09-10").unwrap(), &mut out);
        assert!(out.is_empty(), "a subagent's user is another agent");
    }

    #[test]
    fn long_pasted_briefs_are_not_candidates() {
        assert!(clean(&"x".repeat(MAX_MESSAGE + 1)).is_none());
        assert_eq!(clean("  short  "), Some("short".to_owned()));
        assert_eq!(
            clean("a <system-reminder>b</system-reminder> c"),
            Some("a  c".to_owned())
        );
    }

    #[test]
    fn github_remotes_resolve_to_slugs() {
        assert_eq!(
            github_slug("git@github.com:false-systems/ohjaa.git").as_deref(),
            Some("false-systems/ohjaa")
        );
        assert_eq!(
            github_slug("https://github.com/o/r").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            github_slug("ssh://git@github.com/o/r.git/").as_deref(),
            Some("o/r")
        );
        assert_eq!(github_slug("https://gitlab.com/o/r"), None);
        assert_eq!(github_slug("https://github.com/o"), None);
    }

    #[test]
    fn drafts_are_found_inside_prose_and_fences() {
        let answer = "Here you go:\n```json\n[{\"scope\":\"ours\",\"id\":\"a\",\"kind\":\"taste\",\"body\":\"B\",\"origin\":[\"o\"]}]\n```";
        let drafts = parse_drafts(answer).unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].refines, None);
        assert!(parse_drafts("[]").unwrap().is_empty());
        assert!(parse_drafts("no array").is_err());
        assert!(parse_drafts("] [").is_err());
    }

    #[test]
    fn a_repo_draft_must_name_an_absolute_repository() {
        let draft = Draft {
            scope: "repo".into(),
            repo: Some("relative/path".into()),
            id: "x".into(),
            kind: "fact".into(),
            body: "b".into(),
            origin: vec!["o".into()],
            refines: Some(String::new()),
            supersedes: Vec::new(),
        };
        assert!(draft.clone().into_proposal().is_err());
        let fine = Draft {
            repo: Some("/abs".into()),
            ..draft
        };
        let proposal = fine.into_proposal().unwrap();
        assert_eq!(proposal.repo, Some(PathBuf::from("/abs")));
        assert_eq!(proposal.refines, None, "an empty refines means none");
    }

    #[test]
    fn times_render_as_rfc3339() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(
            normalize_since("2026-09-10").unwrap(),
            parse_time("2026-09-10T00:00:00Z").unwrap()
        );
        assert!(normalize_since("yesterday").is_err());
    }

    #[test]
    fn times_compare_as_instants_not_strings() {
        let utc = parse_time("2026-09-25T10:00:00Z").unwrap();
        // The same instant, written by an author at +02:00.
        assert_eq!(parse_time("2026-09-25T12:00:00+02:00"), Some(utc));
        assert_eq!(parse_time("2026-09-25T05:30:00-04:30"), Some(utc));
        // As strings, ".123Z" sorts *before* "Z"; as instants it is later.
        assert!("2026-09-25T10:00:00.123Z" < "2026-09-25T10:00:00Z");
        assert_eq!(parse_time("2026-09-25T10:00:00.123Z"), Some(utc + 123));
        assert_eq!(parse_time("2026-09-25T10:00:00.5Z"), Some(utc + 500));
        assert_eq!(parse_time("2026-09-25T10:00:00.123456Z"), Some(utc + 123));
        for bad in [
            "2026-09-25T10:00:00",
            "2026-09-25T25:00:00Z",
            "2026-09-25T10:00:00.Z",
            "2026-09-25 10:00:00Z",
            "2026-09-25T10:00:00+0200",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn observed_failures_are_candidates_and_a_clean_session_raises_none() {
        use crate::sessions::{Exit, Finished, Handover, Left, Session};
        let session = |id: &str, left: Left, handover: bool| Session {
            id: id.to_owned(),
            agent: "claude".to_owned(),
            repo: "/r/selko".to_owned(),
            outside_repo: false,
            intent: "retry uploads".to_owned(),
            started_ms: 1_000,
            branch: None,
            head_at_start: None,
            scoped: Vec::new(),
            delivered: Vec::new(),
            readback: None,
            handover: handover.then(Handover::default),
            left: Some(left),
            drafted: false,
            usage: Vec::new(),
            usage_refused: None,
            intent_id: None,
            task: None,
            pgid: None,
            group_started_ms: None,
            validations: Vec::new(),
        };
        let ended = |exit: Exit| Left {
            exit,
            ..Left::orphaned(2_000)
        };
        let clean = Left {
            orphaned: false,
            finished: Finished::NoPullRequest,
            ..ended(Exit::Code { code: 0 })
        };
        let killed = Left {
            orphaned: false,
            ..ended(Exit::Signal { signal: 9 })
        };
        let found = observed_candidates(&[
            session("oh-1", clean, true),
            session("oh-2", killed, true),
            session("oh-3", Left::orphaned(3_000), false),
        ]);
        let texts: Vec<&str> = found.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "in session oh-2: the agent was killed by signal 9",
                "in session oh-3: its ohjaa was gone before leaving; the session was closed as \
                 orphaned at a later launch, and nothing it did was observed",
            ],
            "a clean session with a handover raises nothing; an orphan is not also blamed \
             for the handover it could not write"
        );
        assert!(found
            .iter()
            .all(|c| c.source == "observed" && c.who == "ohjaa"));

        // Only a real failure of the session's own work is a candidate: what
        // could not be checked, and what is not shown to be its, raise none.
        use crate::sessions::{Check, Landed};
        let landed = |check: Check, unattributed: bool| Landed {
            commit: "c0ffee1".to_owned(),
            subject: "s".to_owned(),
            onto: "origin/main".to_owned(),
            check: Some(check),
            evidence: None,
            at_tip: true,
            unattributed,
        };
        let quiet = Left {
            orphaned: false,
            finished: Finished::NoPullRequest,
            landed: vec![
                landed(
                    Check::Unverified {
                        why: "sykli has no --commit".to_owned(),
                    },
                    false,
                ),
                landed(
                    Check::Stale {
                        why: "refused: tree expected a but got b".to_owned(),
                    },
                    false,
                ),
                landed(Check::Unchecked, false),
                landed(
                    Check::Failed {
                        why: "refused: outcome expected passed".to_owned(),
                    },
                    true,
                ),
            ],
            ..ended(Exit::Code { code: 0 })
        };
        assert!(
            observed_candidates(&[session("oh-4", quiet, true)]).is_empty(),
            "unverified, stale, unchecked and unattributed are recorded, never lessons"
        );
    }

    #[test]
    fn the_cursor_only_moves_forward_and_keeps_milliseconds() {
        let root = std::env::temp_dir().join(format!("ohjaa-cursor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: root.join("mine"),
            machine: root.join("machine"),
            proposals: root.join("proposals"),
            state: root.clone(),
        };
        assert_eq!(read_cursor(&dirs).unwrap(), None);
        let later = parse_time("2026-09-25T10:00:00.250Z").unwrap();
        write_cursor(&dirs, later).unwrap();
        assert_eq!(read_cursor(&dirs).unwrap(), Some(later));
        write_cursor(&dirs, later - 86_400_000).unwrap();
        assert_eq!(read_cursor(&dirs).unwrap(), Some(later), "never backwards");
        std::fs::write(root.join("intake-since"), "garbage").unwrap();
        assert!(
            read_cursor(&dirs).is_err(),
            "an unreadable cursor is an error"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn handled_candidates_are_kept_by_digest_and_unreadable_progress_is_an_error() {
        let root = std::env::temp_dir().join(format!("ohjaa-handled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: root.join("mine"),
            machine: root.join("machine"),
            proposals: root.join("proposals"),
            state: root.clone(),
        };
        assert_eq!(read_handled(&dirs).unwrap(), Handled::default());
        let said = |at_ms: i64, text: &str| Candidate {
            source: "claude",
            at_ms,
            at: rfc3339(at_ms.div_euclid(1000)),
            place: "/p".to_owned(),
            who: "person".to_owned(),
            context: String::new(),
            text: text.to_owned(),
        };
        let mut handled = Handled::default();
        handled.0.insert(said(-5, "never python").key());
        handled.0.insert(said(7, "we have lima").key());
        write_handled(&dirs, &handled).unwrap();
        let kept = std::fs::read_to_string(root.join("intake-handled")).unwrap();
        assert!(
            !kept.contains("python") && !kept.contains("lima"),
            "digests, never the words: {kept}"
        );
        assert_eq!(read_handled(&dirs).unwrap(), handled);
        assert!(read_handled(&dirs)
            .unwrap()
            .contains(&said(7, "we have lima")));
        assert!(!read_handled(&dirs)
            .unwrap()
            .contains(&said(8, "we have lima")));
        std::fs::write(root.join("intake-handled"), "7 nothex\n").unwrap();
        assert!(read_handled(&dirs).is_err(), "never a silent start over");
        write_handled(&dirs, &Handled::default()).unwrap();
        assert!(
            !root.join("intake-handled").exists(),
            "nothing handled, no file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cursor_moves_only_past_what_was_handled_and_never_to_where_it_began() {
        let c = |at_ms: i64, text: &str| Candidate {
            source: "review",
            at_ms,
            at: rfc3339(at_ms.div_euclid(1000)),
            place: "https://github.com/o/r/pull/1".to_owned(),
            who: "person".to_owned(),
            context: String::new(),
            text: text.to_owned(),
        };
        let (old, a, b, z) = (c(3, "old"), c(10, "a"), c(20, "b"), c(30, "c"));
        let settle_with = |found: &[Candidate],
                           done: &[&Candidate],
                           sessions: Option<&[Undrafted]>,
                           since,
                           clean| {
            let mut handled = Handled::default();
            handled.0.extend(done.iter().map(|c| c.key()));
            let window = Window {
                since,
                started: 100,
                clean,
            };
            settle(found, &mut handled, sessions, window)
        };
        let found = [a.clone(), b.clone(), z.clone()];
        // The first candidate not handled holds it.
        let held = settle_with(&found, &[&a, &z], None, 5, true);
        assert_eq!((held.cursor, held.left), (Some(10), 1));
        // Nothing left: to where the run started, but only when every
        // source was read; after a warning, past what was handled.
        assert_eq!(
            settle_with(&found, &[&a, &b, &z], None, 5, true).cursor,
            Some(100)
        );
        assert_eq!(
            settle_with(&found, &[&a, &b, &z], None, 5, false).cursor,
            Some(30)
        );
        // Nothing handled after `since`: it stays.
        assert_eq!(settle_with(&found, &[], None, 5, true).cursor, None);
        // An older candidate no session brings back holds it where it is...
        let found = [old.clone(), a.clone()];
        assert_eq!(settle_with(&found, &[&a], None, 5, true).cursor, None);
        // ...and one a queued session brings back does not.
        let queued = [Undrafted {
            id: "oh-1".to_owned(),
            started_ms: 1,
            members: Some(vec![old.key()]),
            whole: true,
        }];
        let settled = settle_with(&found, &[&a], Some(&queued), 5, true);
        assert_eq!((settled.cursor, settled.drafted.len()), (Some(100), 0));
        // A window that begins after the run started moves nothing.
        assert_eq!(
            settle_with(std::slice::from_ref(&a), &[&a], None, 200, true).cursor,
            None
        );
        // Where a run reads from: `--since`, else the cursor, else the
        // default window, which is kept as the cursor before drafting.
        assert_eq!(window_start(Some(7), Some(9), 100, 50), (7, None));
        assert_eq!(window_start(None, Some(9), 100, 50), (9, None));
        assert_eq!(window_start(None, None, 100, 50), (50, Some(50)));
    }

    #[test]
    fn damaged_progress_is_an_error_never_read_as_less() {
        let root = std::env::temp_dir().join(format!("ohjaa-damaged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: root.join("mine"),
            machine: root.join("machine"),
            proposals: root.join("proposals"),
            state: root.clone(),
        };
        let said = |at_ms: i64, text: &str| Candidate {
            source: "claude",
            at_ms,
            at: rfc3339(at_ms.div_euclid(1000)),
            place: "/p".to_owned(),
            who: "person".to_owned(),
            context: String::new(),
            text: text.to_owned(),
        };
        let mut handled = Handled::default();
        for (at, text) in [(5, "a"), (-7, "b"), (1_793_000_000_123, "c")] {
            handled.0.insert(said(at, text).key());
        }
        write_handled(&dirs, &handled).unwrap();
        let path = root.join("intake-handled");
        let whole = std::fs::read(&path).unwrap();
        assert_eq!(read_handled(&dirs).unwrap(), handled);
        let read = |bytes: &[u8]| {
            std::fs::write(&path, bytes).unwrap();
            read_handled(&dirs)
        };
        // Nothing writes an empty file, so an empty one is damage.
        assert!(read(b"").is_err(), "an empty file is not \"none handled\"");
        // Cut anywhere (inside a digest, at a line boundary, inside the
        // last line): damage, said out loud, never fewer keys read quietly.
        for cut in 1..whole.len() {
            assert!(
                read(&whole[..cut]).is_err(),
                "cut at {cut} read as progress: {:?}",
                String::from_utf8_lossy(&whole[..cut])
            );
        }
        // And what follows the end is damage too.
        let mut longer = whole.clone();
        longer.extend_from_slice(b"5 0000000000000000\n");
        assert!(read(&longer).is_err(), "a line after the end");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- the model: random queues, caps and failure points ---------------------

    use std::collections::HashSet;

    /// SplitMix64: a small deterministic generator, so a failing seed replays.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        /// In `0..n`.
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.below(100) < percent
        }
    }

    struct ModelSession {
        id: String,
        repo: String,
        started: i64,
        ended: i64,
        hands_over: bool,
    }

    impl ModelSession {
        /// As the log has it at `now`: ended (with its handover, if any), or
        /// still open.
        fn at(&self, now: i64) -> crate::sessions::Session {
            use crate::sessions::{Exit, Finished, Handover, Left, Session};
            let ended = self.ended <= now;
            Session {
                id: self.id.clone(),
                agent: "claude".to_owned(),
                repo: self.repo.clone(),
                outside_repo: false,
                intent: "model".to_owned(),
                started_ms: self.started,
                branch: None,
                head_at_start: None,
                scoped: Vec::new(),
                delivered: Vec::new(),
                readback: None,
                handover: (ended && self.hands_over).then(|| Handover {
                    at_ms: self.ended,
                    surprised: Some(format!("{} surprised", self.id)),
                    ..Handover::default()
                }),
                left: ended.then(|| Left {
                    exit: Exit::Code { code: 0 },
                    orphaned: false,
                    finished: Finished::NoPullRequest,
                    ..Left::orphaned(self.ended)
                }),
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
    }

    /// What there is to draft from: transcripts and reviews, each said at its
    /// time, and sessions, some of whose windows lie before the first run.
    struct World {
        /// Where the first run's window starts.
        since: i64,
        /// How far back a run with no cursor reads (fourteen days, for
        /// `ohjaa intake`): the first run is at `since + window`.
        window: i64,
        said: Vec<Candidate>,
        sessions: Vec<ModelSession>,
        /// What the report finds: each found by a run whose window holds the
        /// first time (a commit landing, as `git log --since` sees it), and
        /// dated by the second (its commit time), which may lie before the
        /// window.
        reported: Vec<(i64, Candidate)>,
        /// By then everything was said and every session ended.
        end: i64,
    }

    fn said(source: &'static str, at_ms: i64, place: String, text: String) -> Candidate {
        Candidate {
            source,
            at_ms,
            at: rfc3339(at_ms.div_euclid(1000)),
            place,
            who: "person".to_owned(),
            context: String::new(),
            text,
        }
    }

    fn world(rng: &mut Rng) -> World {
        let since = 50_000;
        let sessions: Vec<ModelSession> = (0..rng.below(4))
            .map(|j| {
                let started = rng.below(120_000) as i64;
                ModelSession {
                    id: format!("oh-{j}"),
                    repo: format!("/r/s{j}"),
                    started,
                    ended: started + rng.below(40_000) as i64,
                    hands_over: rng.chance(60),
                }
            })
            .collect();
        let mut out = Vec::new();
        for session in &sessions {
            for i in 0..rng.below(6) {
                let at = session.started
                    + rng.below((session.ended - session.started + 1) as u64) as i64;
                let text = format!("{} said {i}", session.id);
                out.push(said("claude", at, format!("{}/src", session.repo), text));
            }
        }
        for i in 0..rng.below(30) {
            // Coarse times, now and then, so candidates share a millisecond.
            let at = if rng.chance(30) {
                rng.below(20) as i64 * 10_000
            } else {
                rng.below(200_000) as i64
            };
            let (source, place) = match rng.below(3) {
                0 => ("review", "https://github.com/o/r/pull/1".to_owned()),
                1 if !sessions.is_empty() => {
                    let session = &sessions[rng.below(sessions.len() as u64) as usize];
                    ("claude", session.repo.clone())
                }
                _ => ("codex", "/r/elsewhere".to_owned()),
            };
            out.push(said(source, at, place, format!("said {i}")));
        }
        // The same words at the same time, from two sources: one candidate.
        if let Some(first) = out.first().cloned() {
            if rng.chance(30) {
                out.push(Candidate {
                    source: "review",
                    ..first
                });
            }
        }
        let reported: Vec<(i64, Candidate)> = (0..rng.below(6))
            .map(|i| {
                let at = rng.below(200_000) as i64;
                let landed = at + rng.below(60_000) as i64;
                let text = format!("unproven {i}");
                (landed, said("observed", at, "/r/x".to_owned(), text))
            })
            .collect();
        let end = out
            .iter()
            .map(|c| c.at_ms)
            .chain(reported.iter().map(|(landed, _)| *landed))
            .chain(sessions.iter().map(|s| s.ended))
            .max()
            .unwrap_or(0);
        World {
            since,
            window: 1 + rng.below(40_000) as i64,
            said: out,
            sessions,
            reported,
            end,
        }
    }

    #[derive(Default)]
    struct State {
        cursor: Option<i64>,
        handled: Handled,
        drafted: HashSet<String>,
    }

    #[derive(Clone, Copy)]
    enum Fail {
        None,
        /// The drafter fails on this send (1-based).
        Drafter(usize),
        /// Killed after this batch was answered, before it was kept.
        Killed(usize),
    }

    struct Plan {
        cap: usize,
        batch: usize,
        /// Every source read without a warning.
        clean: bool,
        /// Every transcript read: each session's whole window.
        whole: bool,
        fail: Fail,
    }

    struct Ran {
        /// Found and not handled when the run began.
        before: usize,
        sent: usize,
        /// Found and not handled when it ended; `None` when it was killed.
        after: Option<usize>,
    }

    /// One `ohjaa intake` at `now`, as `cmd_intake` runs it: the same
    /// gathering, then the same calls. Asserts that nothing kept is ever sent
    /// again and that the cursor never goes before the run's window.
    fn run(
        world: &World,
        state: &mut State,
        now: i64,
        plan: &Plan,
        kept: &mut HashSet<Key>,
    ) -> Ran {
        // With no cursor yet, the default window, which slides with time
        // unless it is kept.
        let (since, keep) = window_start(None, state.cursor, now, world.window);
        state.cursor = state.cursor.max(keep);
        let undrafted: Vec<crate::sessions::Session> = world
            .sessions
            .iter()
            .filter(|s| !state.drafted.contains(&s.id))
            .map(|s| s.at(now))
            .collect();
        let queued: Vec<_> = undrafted
            .iter()
            .filter(|s| s.left.is_some())
            .cloned()
            .collect();
        let from = queued
            .iter()
            .map(|s| s.started_ms - 1)
            .min()
            .map_or(since, |earliest| earliest.min(since));
        let visible = || world.said.iter().filter(|c| c.at_ms <= now);
        // Not `whole`: the transcript that could not be read is a session's.
        let sessions_now: Vec<_> = world.sessions.iter().map(|s| s.at(now)).collect();
        let readable = |c: &&Candidate| plan.whole || !sessions_now.iter().any(|s| in_window(c, s));
        let (new, older): (Vec<_>, Vec<_>) = visible()
            .filter(|c| c.source != "review" && c.at_ms > from)
            .filter(readable)
            .cloned()
            .partition(|c| c.at_ms > since);
        let mut candidates = new;
        candidates.extend(within_sessions(older, &queued));
        candidates.extend(
            visible()
                .filter(|c| c.source == "review" && c.at_ms > since)
                .cloned(),
        );
        candidates.extend(
            world
                .reported
                .iter()
                .filter(|(landed, _)| *landed > since && *landed <= now)
                .map(|(_, c)| c.clone()),
        );
        candidates.extend(handover_candidates(&queued));
        candidates.extend(observed_candidates(&queued));
        let found = found(candidates);

        let before = found.iter().filter(|c| !state.handled.contains(c)).count();
        let mut handled = state.handled.clone();
        let mut durable = state.handled.clone();
        let (mut sends, mut keeps) = (0, 0);
        let drafted = draft_pending(
            &found,
            &mut handled,
            plan.batch,
            plan.cap,
            |chunk| {
                sends += 1;
                if matches!(plan.fail, Fail::Drafter(n) if n == sends) {
                    bail!("the drafter failed");
                }
                for c in chunk {
                    assert!(
                        !kept.contains(&c.key()),
                        "`{}` sent again after it was kept",
                        c.text
                    );
                }
                Ok(())
            },
            |handled| {
                keeps += 1;
                if matches!(plan.fail, Fail::Killed(n) if n == keeps) {
                    bail!("killed");
                }
                durable = handled.clone();
                Ok(())
            },
        );
        kept.extend(durable.0.iter().copied());
        let batches = before.min(plan.cap).div_ceil(plan.batch);
        let expected = match plan.fail {
            Fail::Drafter(n) | Fail::Killed(n) if n <= batches => {
                let answered = if matches!(plan.fail, Fail::Killed(_)) {
                    n
                } else {
                    n - 1
                };
                (answered * plan.batch).min(before.min(plan.cap))
            }
            _ => before.min(plan.cap),
        };
        assert_eq!(
            drafted.sent, expected,
            "a run drafts up to its cap, until it fails"
        );
        if let Fail::Killed(n) = plan.fail {
            if n <= batches {
                // Only what was kept survives the kill.
                state.handled = durable;
                return Ran {
                    before,
                    sent: drafted.sent,
                    after: None,
                };
            }
        }
        assert_eq!(
            durable, handled,
            "every answered batch was kept before the next"
        );

        let sessions: Vec<Undrafted> = undrafted
            .iter()
            .map(|s| Undrafted {
                id: s.id.clone(),
                started_ms: s.started_ms,
                members: s.left.is_some().then(|| members(s, &found)),
                whole: plan.whole,
            })
            .collect();
        let window = Window {
            since,
            started: now,
            clean: plan.clean,
        };
        let settled = settle(&found, &mut handled, Some(&sessions), window);
        assert!(
            settled
                .cursor
                .is_none_or(|cursor| cursor > since && cursor <= now),
            "the cursor {:?} is outside ({since}, {now}]",
            settled.cursor
        );
        assert_eq!(
            settled.left,
            before - drafted.sent,
            "what was drafted is no longer left"
        );
        // `write_cursor`: forward only.
        state.cursor = state.cursor.max(settled.cursor);
        state.drafted.extend(settled.drafted);
        state.handled = handled;
        Ran {
            before,
            sent: drafted.sent,
            after: Some(settled.left),
        }
    }

    #[test]
    fn model_every_run_makes_progress_and_repeated_runs_drain_the_queue() {
        for seed in 0..3_000 {
            let mut rng = Rng(seed);
            let world = world(&mut rng);
            let mut state = State::default();
            let mut kept = HashSet::new();
            let mut cursors = Vec::new();
            let mut now = world.since + world.window;
            // While the world changes: runs at random times, with random caps,
            // warnings, drafter failures and kills.
            loop {
                // A transcript that cannot be read is a warning too.
                let whole = !rng.chance(10);
                let plan = Plan {
                    cap: 1 + rng.below(6) as usize,
                    batch: 1 + rng.below(3) as usize,
                    clean: whole && !rng.chance(10),
                    whole,
                    fail: match rng.below(10) {
                        0 | 1 => Fail::Drafter(1 + rng.below(3) as usize),
                        2 => Fail::Killed(1 + rng.below(3) as usize),
                        _ => Fail::None,
                    },
                };
                let ran = run(&world, &mut state, now, &plan, &mut kept);
                if matches!(plan.fail, Fail::None) && ran.before > 0 {
                    assert!(ran.sent >= 1, "seed {seed}: a run with work left made none");
                }
                cursors.push(state.cursor);
                if now >= world.end {
                    break;
                }
                now += 1 + rng.below(30_000) as i64;
            }
            // Then the world stands still: every run strictly reduces the work
            // left, until one reports that nothing is left.
            let mut runs = 0;
            loop {
                now += 1_000;
                let plan = Plan {
                    cap: 1 + rng.below(3) as usize,
                    batch: 1 + rng.below(2) as usize,
                    clean: true,
                    whole: true,
                    fail: Fail::None,
                };
                let ran = run(&world, &mut state, now, &plan, &mut kept);
                cursors.push(state.cursor);
                if ran.before == 0 {
                    assert_eq!((ran.sent, ran.after), (0, Some(0)), "seed {seed}");
                    break;
                }
                assert!(ran.after.unwrap() < ran.before, "seed {seed}: no progress");
                runs += 1;
                assert!(runs < 500, "seed {seed}: the queue never drained");
            }
            assert!(
                cursors.windows(2).all(|pair| pair[0] <= pair[1]),
                "seed {seed}: the cursor moved back: {cursors:?}"
            );
            // Every session was drafted from, and every candidate there was
            // to draft from was drafted from: those said after the first
            // window began, and every session's own.
            assert_eq!(state.drafted.len(), world.sessions.len(), "seed {seed}");
            let mut owed: HashSet<Key> = world
                .said
                .iter()
                .filter(|c| c.at_ms > world.since)
                .map(Candidate::key)
                .collect();
            owed.extend(
                world
                    .reported
                    .iter()
                    .filter(|(landed, _)| *landed > world.since)
                    .map(|(_, c)| c.key()),
            );
            for session in &world.sessions {
                owed.extend(members(&session.at(world.end), &world.said));
            }
            assert_eq!(
                kept, owed,
                "seed {seed}: drafted from exactly what was owed"
            );
            // One more run: nothing to draft, and nothing left kept that no
            // run could find again.
            now += 1_000;
            let plan = Plan {
                cap: 1,
                batch: 1,
                clean: true,
                whole: true,
                fail: Fail::None,
            };
            let ran = run(&world, &mut state, now, &plan, &mut kept);
            assert_eq!((ran.before, ran.sent), (0, 0), "seed {seed}");
            assert!(
                state.handled.0.is_empty(),
                "seed {seed}: {:?}",
                state.handled
            );
        }
    }

    #[test]
    fn the_audits_queue_drains_in_four_runs_of_four_hundred() {
        // The 2026-10-26 audit, in the model: 486 old candidates of 65 queued
        // sessions and 842 new ones, with the default cap of 400.
        let mut out = Vec::new();
        let sessions: Vec<ModelSession> = (0..65)
            .map(|j| ModelSession {
                id: format!("oh-{j}"),
                repo: format!("/r/s{j}"),
                started: j * 1_000,
                ended: j * 1_000 + 900,
                hands_over: true,
            })
            .collect();
        for (i, session) in sessions.iter().enumerate() {
            for k in 0..(if i < 31 { 7 } else { 6 }) {
                let text = format!("{} said {k}", session.id);
                out.push(said(
                    "claude",
                    session.started + k,
                    session.repo.clone(),
                    text,
                ));
            }
        }
        for i in 0..842 {
            out.push(said(
                "codex",
                100_000 + i,
                "/r/elsewhere".to_owned(),
                format!("new {i}"),
            ));
        }
        let world = World {
            since: 99_999,
            window: 100_002,
            said: out,
            sessions,
            reported: Vec::new(),
            end: 100_000 + 842,
        };
        let mut state = State::default();
        let mut kept = HashSet::new();
        let plan = Plan {
            cap: 400,
            batch: 40,
            clean: true,
            whole: true,
            fail: Fail::None,
        };
        let mut lefts = Vec::new();
        for run_number in 1..=5 {
            let ran = run(&world, &mut state, 200_000 + run_number, &plan, &mut kept);
            lefts.push(ran.after.unwrap());
        }
        assert_eq!(lefts, [1328 - 400, 528, 128, 0, 0]);
        assert_eq!(state.drafted.len(), 65);
    }
}
