//! Intake: turning what people actually said into proposed Knowledge (law 5).
//!
//! A correction a person has to repeat is Knowledge that failed to reach the
//! agent. Intake reads the places corrections happen (what people wrote in
//! agent sessions, through vessa; pull request comments; reverts), keeps
//! locally what looks like a correction or a repeated instruction, asks a
//! drafter (an agent CLI such as `claude -p`) which of them carry durable
//! Knowledge, and writes each draft as a *proposal*. Nothing becomes current
//! until a person accepts it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use faija_knowledge::{Kind, Node, Scope, Status};
use serde::Deserialize;
use serde_json::Value;

use crate::knowledge::{self, day_number, parse_date, Dirs, Loaded, Proposal};
use crate::process;

/// Longer messages are pasted briefs, not corrections.
const MAX_MESSAGE: usize = 1_500;
/// How much of the agent's preceding words travel with a message.
const CONTEXT_TAIL: usize = 600;

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
    /// The session it was said or observed in, when one is known: the
    /// agent's own (as vessa names it) for what a person wrote, ohjaa's for a
    /// handover or an observed failure. Kept with what it re-teaches.
    pub session: Option<String>,
}

// ---- time -------------------------------------------------------------------

pub fn rfc3339(seconds: i64) -> String {
    let date = knowledge::date_from_day_number(seconds.div_euclid(86_400));
    let s = seconds.rem_euclid(86_400);
    format!("{date}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// An instant as RFC 3339 UTC with its milliseconds, as the cursor keeps it.
pub fn rfc3339_ms(at_ms: i64) -> String {
    let at = rfc3339(at_ms.div_euclid(1000));
    format!("{}.{:03}Z", &at[..at.len() - 1], at_ms.rem_euclid(1000))
}

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Milliseconds since the epoch for `YYYY-MM-DD` (midnight UTC) or an RFC 3339
/// time with `Z` or a `±HH:MM` offset and optional fraction of a second.
pub fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let date_text = text.get(..10)?;
    if !date_text.bytes().enumerate().all(|(at, byte)| {
        if at == 4 || at == 7 {
            byte == b'-'
        } else {
            byte.is_ascii_digit()
        }
    }) {
        return None;
    }
    let date = parse_date(text)?;
    let days = day_number(date);
    if text.len() == 10 {
        return Some(days * 86_400_000);
    }
    let rest = text.get(10..)?;
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix('t'))?;
    let number = |s: &str| {
        s.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| s.parse::<i64>().ok())
            .flatten()
    };
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
            let hours = number(rest.get(1..3)?)?;
            let minutes = number(rest.get(4..6)?)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 60 + minutes)
        }
    };
    let local = ((days * 24 + hours) * 60 + minutes) * 60 + seconds;
    Some((local - offset_minutes * 60) * 1000 + millis)
}

/// The start of an intake window, from `YYYY-MM-DD` or an RFC 3339 time.
pub fn normalize_since(text: &str) -> Result<i64> {
    parse_time(text).with_context(|| format!("`{}` is not a date or RFC 3339 time", text.trim()))
}

// ---- what people said, through vessa ----------------------------------------

/// The text without the `<system-reminder>` blocks a harness writes into it,
/// trimmed: as vessa lists a message's text, so a candidate's words (and the
/// digest it is kept by) are the same whichever reader found them.
fn stripped(text: &str) -> String {
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
    out.trim().to_owned()
}

/// Words that can be a candidate: something left once stripped, and not a
/// pasted brief.
fn clean(text: &str) -> Option<String> {
    let out = stripped(text);
    (!out.is_empty() && out.chars().count() <= MAX_MESSAGE).then_some(out)
}

fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(chars)).collect()
}

/// The form of `vessa messages --json` intake reads.
pub const VESSA_MESSAGES: &str = "vessa-messages.v1";

/// How long vessa may take to list what people wrote: every transcript it
/// knows of is read when the window is long (two months took 5 s on the
/// machine this was written on).
const VESSA_TIMEOUT: Duration = Duration::from_secs(120);

/// The sources of what people wrote in agent sessions: the agents vessa
/// reads, and `agent` for one it names that intake does not know yet.
pub const SAID: [&str; 5] = ["claude", "codex", "pi", "opencode", "agent"];

/// One message a person wrote in an agent session, as vessa lists it: the
/// candidate it becomes, and what the pre-filter reads of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub candidate: Candidate,
    /// vessa's `kind`: `message`, `queued` (typed while the agent worked),
    /// `answer` (to the agent's question, in the person's own words),
    /// `rejection` (the reason for refusing a tool call) or `task` (typed
    /// into a launch).
    pub kind: String,
    /// The agent's reply it follows. None when there was none, or when that
    /// turn failed: then the message answers a failure, not the agent.
    pub reply: Option<String>,
    /// Longer than `MAX_MESSAGE`: a pasted brief, never a candidate.
    pub long: bool,
}

#[derive(Deserialize)]
struct VessaDocument {
    schema: String,
    messages: Vec<VessaMessage>,
    /// Absent from a document vessa ended with an `error`.
    #[serde(default)]
    coverage: Option<VessaCoverage>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct VessaMessage {
    agent: String,
    session: String,
    #[serde(default)]
    kind: String,
    at: Option<String>,
    #[serde(default)]
    cwd: String,
    text: String,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    reply: Option<VessaReply>,
}

#[derive(Deserialize)]
struct VessaReply {
    text: String,
    #[serde(default)]
    failed: bool,
}

#[derive(Deserialize)]
struct VessaCoverage {
    #[serde(default)]
    gaps: Vec<String>,
    #[serde(default)]
    limitations: Vec<String>,
}

/// What people wrote in agent sessions after `since_ms` (exclusive), as
/// vessa, the one reader of agent conversations, lists it: `vessa messages
/// --since <time> --json`. What vessa says it could not read is a warning
/// here, which holds the cursor, and a store it does not read is a gap, said
/// and holding nothing (see [`listed`]); a vessa that cannot list messages
/// is said, and nothing reads the transcripts in its place.
pub fn transcripts(since_ms: i64, warnings: &mut Vec<String>, gaps: &mut Vec<String>) -> Vec<Said> {
    let since = rfc3339_ms(since_ms);
    let output = process::run(
        Command::new("vessa").args(["messages", "--since", &since, "--json"]),
        None,
        VESSA_TIMEOUT,
    );
    match output {
        Ok(output) => {
            // clap ends a usage error with a hint; its first `error:` line
            // is what went wrong.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("error:"))
                .map_or_else(|| output.failure_detail(), str::to_owned);
            listed(
                output.status.code(),
                &output.stdout,
                &detail,
                since_ms,
                warnings,
                gaps,
            )
        }
        Err(error) => {
            let text = format!("{error:#}");
            warnings.push(
                if text.contains("No such file") || text.contains("not found") {
                    "vessa is not on PATH: install vessa, the one reader of agent conversations"
                        .to_owned()
                } else {
                    format!("vessa could not list messages: {text}")
                },
            );
            Vec::new()
        }
    }
}

/// Read vessa's answer (`vessa-messages.v1`; its exit `code`, and `detail`,
/// the last line it wrote to stderr). Exit 4 says the list is partial, for
/// two kinds of reason, which vessa keeps apart:
/// - a source it could not read (`warnings`), or what a transcript leaves
///   unknown (`coverage.limitations`): each is a warning, which holds the
///   cursor until vessa reads it;
/// - a store it knows of and never reads (`coverage.gaps`; today opencode's
///   SQLite, and only when written in the window): each goes to `gaps`, to
///   be said on every run and hold nothing. Waiting cannot read it, and a
///   hold would keep the window before every later write of the store, so
///   it would never end; once vessa reads it, a `--since` reads it back.
///
/// A document vessa ended with an `error` keeps the messages listed before
/// it, and warns. A vessa with no `messages` (one older than vessa#6) is told
/// to be updated.
pub fn listed(
    code: Option<i32>,
    stdout: &[u8],
    detail: &str,
    since_ms: i64,
    warnings: &mut Vec<String>,
    gaps: &mut Vec<String>,
) -> Vec<Said> {
    let exit = code.map_or("a signal".to_owned(), |code| code.to_string());
    let document: VessaDocument = match serde_json::from_slice(stdout) {
        Ok(document) => document,
        Err(error) => {
            warnings.push(
                if code == Some(2) || detail.contains("unrecognized subcommand") {
                    format!(
                        "vessa does not list messages (exit {exit}: {detail}): update vessa; \
                         intake reads what people wrote only through `vessa messages` \
                         ({VESSA_MESSAGES})"
                    )
                } else if stdout.iter().all(u8::is_ascii_whitespace) {
                    format!("vessa listed no messages (exit {exit}: {detail})")
                } else {
                    format!("vessa's messages are not {VESSA_MESSAGES} ({error}; exit {exit})")
                },
            );
            return Vec::new();
        }
    };
    if document.schema != VESSA_MESSAGES {
        warnings.push(format!(
            "vessa wrote `{}`, and intake reads {VESSA_MESSAGES}: update vessa or ohjaa so \
             they agree",
            document.schema
        ));
        return Vec::new();
    }
    let before = (warnings.len(), gaps.len());
    let coverage = document.coverage.as_ref();
    for warning in document
        .warnings
        .iter()
        .chain(coverage.map(|c| &c.limitations).into_iter().flatten())
    {
        warnings.push(format!("vessa: {warning}"));
    }
    for gap in coverage.map(|c| &c.gaps).into_iter().flatten() {
        gaps.push(format!("vessa: {gap}"));
    }
    if let Some(error) = &document.error {
        warnings.push(format!(
            "vessa failed after listing {} message(s) (exit {exit}): {error}",
            document.messages.len()
        ));
    }
    if code != Some(0) && (warnings.len(), gaps.len()) == before {
        warnings.push(format!(
            "vessa says its list is partial (exit {exit}: {detail})"
        ));
    }
    let mut undated = 0;
    let mut out = Vec::new();
    for message in document.messages {
        // vessa's window is exclusive too; a message no time can place
        // cannot be windowed at all.
        let Some(at_ms) = message.at.as_deref().and_then(parse_time) else {
            undated += 1;
            continue;
        };
        if at_ms <= since_ms {
            continue;
        }
        let text = stripped(&message.text);
        if text.is_empty() {
            continue;
        }
        out.push(heard(message, at_ms, text));
    }
    if undated > 0 {
        warnings.push(format!(
            "vessa listed {undated} message(s) whose time intake cannot read"
        ));
    }
    out
}

/// One of vessa's messages as a candidate: when, where (the folder it was
/// said in), who (the person, how, and in which session), what the agent
/// said before, and the words, exactly as vessa gives them.
fn heard(message: VessaMessage, at_ms: i64, text: String) -> Said {
    let source = SAID
        .iter()
        .find(|agent| **agent == message.agent)
        .copied()
        .unwrap_or("agent");
    let how = match message.kind.as_str() {
        "queued" => ", typed while the agent worked",
        "answer" => ", answering the agent's question",
        "rejection" => ", refusing a tool call",
        "task" => ", the task given at launch",
        _ => "",
    };
    let context = match (&message.question, &message.reply) {
        (Some(question), _) if message.kind == "answer" => {
            tail(&format!("asked: {question}"), CONTEXT_TAIL)
        }
        (_, Some(reply)) if reply.failed => format!(
            "(the agent's turn failed) {}",
            tail(&reply.text, CONTEXT_TAIL)
        ),
        (_, Some(reply)) => tail(&reply.text, CONTEXT_TAIL),
        _ => String::new(),
    };
    let reply = message
        .reply
        .filter(|reply| !reply.failed)
        .map(|reply| reply.text);
    Said {
        long: text.chars().count() > MAX_MESSAGE,
        candidate: Candidate {
            source,
            at_ms,
            at: rfc3339(at_ms.div_euclid(1000)),
            place: message.cwd,
            who: format!(
                "person{how} ({} session {})",
                message.agent, message.session
            ),
            context,
            text,
            session: Some(message.session),
        },
        kind: message.kind,
        reply,
    }
}

// ---- the pre-filter ---------------------------------------------------------
//
// Most of what people write to agents is the work itself, not a correction,
// and each drafter call costs minutes and tokens. Before the drafter, intake
// keeps, locally and with no network, only what looks like a correction or a
// repeated instruction: the archived seula's rules, ported as they were
// (`phrase-rules.v3`; repeats by `normalized-text.v1`), and a reason given for
// refusing a tool call, which is a correction by what it is.
//
// Measured on 2026-10-26 its precision was about 65% strict and 87% lenient,
// and its recall low: it drops corrections. So it is never silent. Every run
// says how many messages it read, kept (by rule) and dropped (by agent);
// `--no-prefilter` sends every message; and nothing it drops is kept as
// handled. Its verdict is not stored either: a message it dropped becomes a
// candidate again when a run reads it again, under that run's rules (a
// `--since` before the cursor reads a stretch again, with `--no-prefilter` or
// with rules changed since). A rule change does not reach back by itself:
// that would need a record of every message read, not only of those sent.

/// The correction rules, by the name seula reported them under.
pub const PREFILTER: &str = "phrase-rules.v3";

/// Why the pre-filter keeps each message: `correction` (a phrase rule),
/// `repeat` (the same words more than once among those read), `rejection`
/// (a reason for refusing a tool call); `None` when it drops it.
pub fn prefilter(said: &[Said]) -> Vec<Option<&'static str>> {
    let words: Vec<String> = said.iter().map(|s| normalized(&s.candidate.text)).collect();
    let mut times: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for text in words.iter().filter(|t| (20..=2000).contains(&t.len())) {
        *times.entry(text).or_default() += 1;
    }
    said.iter()
        .zip(&words)
        .map(|(said, text)| {
            if said.kind == "rejection" {
                Some("rejection")
            } else if correction_signal(&said.candidate.text, said.reply.as_deref()).is_some() {
                Some("correction")
            } else if times.get(text.as_str()).is_some_and(|n| *n > 1) {
                Some("repeat")
            } else {
                None
            }
        })
        .collect()
}

/// What the pre-filter did with one run's messages, said by counts only.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sifted {
    /// Messages read: in the window, or in a queued session's.
    pub read: usize,
    /// Of those, pasted briefs (longer than `MAX_MESSAGE`), never candidates.
    pub long: usize,
    /// The pre-filter ran (not `--no-prefilter`).
    pub filtered: bool,
    /// Kept as candidates, by the rule that kept each.
    pub kept: BTreeMap<&'static str, usize>,
    /// Dropped by the pre-filter, by source.
    pub dropped: BTreeMap<&'static str, usize>,
}

/// The candidates among `said`: every message that is not a pasted brief,
/// and, when `filter`, only those the pre-filter keeps.
pub fn sift(said: Vec<Said>, filter: bool) -> (Vec<Candidate>, Sifted) {
    let mut sifted = Sifted {
        read: said.len(),
        filtered: filter,
        ..Sifted::default()
    };
    let (long, said): (Vec<Said>, Vec<Said>) = said.into_iter().partition(|s| s.long);
    sifted.long = long.len();
    let verdicts = if filter {
        prefilter(&said)
    } else {
        vec![Some("unfiltered"); said.len()]
    };
    let mut kept = Vec::new();
    for (said, verdict) in said.into_iter().zip(verdicts) {
        match verdict {
            Some(rule) => {
                *sifted.kept.entry(rule).or_default() += 1;
                kept.push(said.candidate);
            }
            None => *sifted.dropped.entry(said.candidate.source).or_default() += 1,
        }
    }
    (kept, sifted)
}

fn counts(counts: &BTreeMap<&'static str, usize>) -> String {
    counts
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Sifted {
    /// One line for the person: read, left out as briefs, kept, dropped.
    pub fn said(&self) -> String {
        let kept: usize = self.kept.values().sum();
        let mut line = format!("{} message(s) people wrote, listed by vessa", self.read);
        if self.long > 0 {
            let _ = write!(
                line,
                "; {} longer than {MAX_MESSAGE} characters left out as pasted briefs",
                self.long
            );
        }
        if !self.filtered {
            let _ = write!(
                line,
                "; no pre-filter (--no-prefilter): {kept} candidate(s)"
            );
            return line;
        }
        let dropped: usize = self.dropped.values().sum();
        let _ = write!(line, "; the pre-filter ({PREFILTER}) kept {kept}");
        if kept > 0 {
            let _ = write!(line, " ({})", counts(&self.kept));
        }
        let _ = write!(line, " and dropped {dropped}");
        if dropped > 0 {
            let _ = write!(
                line,
                " ({}), keeping none of them: `--no-prefilter` sends every message, and \
                 `--since` reads a stretch again",
                counts(&self.dropped)
            );
        }
        line
    }
}

/// `reply` is the agent's reply this message follows, if any (seula's
/// `correction_signal`, unchanged).
fn correction_signal(text: &str, reply: Option<&str>) -> Option<&'static str> {
    let after_response = reply.is_some();
    let asked = reply.is_some_and(asks);
    if text.len() > 2000 {
        return None;
    }
    let mut fence = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            let marker = &line[..3];
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() || line.starts_with('>') {
            continue;
        }
        for clause in line.split(['.', '!', '?', ';']) {
            // Quoted examples and inline code are not direct instructions. This
            // deliberately misses mixed quote/prose clauses rather than guessing.
            if clause.contains(['"', '`', '“', '”']) {
                continue;
            }
            let clause = spelled(&normalized(clause).replace('’', "'"));
            let contrast = clause.starts_with("but ");
            let mut text = clause.as_str();
            for prefix in ["but ", "no, ", "actually, ", "please "] {
                text = text.strip_prefix(prefix).unwrap_or(text);
            }
            let starts = |phrase: &str| {
                text.strip_prefix(phrase).is_some_and(|rest| {
                    rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric() && c != '\'')
                })
            };
            if [
                "i already told you",
                "i told you",
                "that's not what",
                "that is not what",
                "you forgot",
                "you missed",
                "as i said",
                "like i said",
                "i already asked",
                "i already said",
            ]
            .iter()
            .any(|p| starts(p))
            {
                return Some("explicit reminder or mismatch");
            }
            if after_response {
                if [
                    "this is not enough",
                    "that is not enough",
                    "that's not enough",
                    "this isn't enough",
                    "something is missing",
                    "this is still wrong",
                    "that is still wrong",
                    "we are still missing",
                    "this doesn't address",
                    "that doesn't address",
                    "not what i asked",
                ]
                .iter()
                .any(|p| starts(p))
                {
                    return Some("unmet expectation after assistant response");
                }
                if (contrast && starts("we need"))
                    || [
                        "stop asking",
                        "do not use",
                        "don't use",
                        "instead",
                        "we need more than",
                        "let's do it better",
                        "lets do it better",
                        "try again",
                    ]
                    .iter()
                    .any(|p| starts(p))
                {
                    return Some("requested redirection after assistant response");
                }
                if asked_again(text) {
                    return Some("explicit reminder or mismatch");
                }
                // A "no" answering the assistant's question is an answer, unless
                // the person says the assistant misunderstood.
                if rejection(&clause) && (!asked || misunderstood(&clause)) {
                    return Some("rejection after assistant response");
                }
                if challenge(text) {
                    return Some("challenge to the assistant's action after response");
                }
            }
        }
    }
    None
}

/// Apostrophe-free spellings as people type them, so "dont use" reads as
/// "don't use" and "thats not what" as "that's not what".
fn spelled(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let (stem, tail) = word.split_at(word.trim_end_matches([',', ':']).len());
            let stem = match stem {
                "dont" => "don't",
                "doesnt" => "doesn't",
                "didnt" => "didn't",
                "isnt" => "isn't",
                "cant" => "can't",
                "thats" => "that's",
                "lets" => "let's",
                "its" => "it's",
                "im" => "i'm",
                "youre" => "you're",
                other => other,
            };
            format!("{stem}{tail}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words that follow "no" when it rejects the reply ("no i mean", "no we
/// had", "no but"), unlike "no" as a determiner ("no target", "no files").
const REJECTION_FOLLOWERS: [&str; 21] = [
    "i", "i'm", "we", "you", "u", "it", "it's", "that", "that's", "this", "but", "let's", "just",
    "wait", "actually", "not", "don't", "please", "stop", "why", "instead",
];

/// "no" as an interjection that rejects the assistant's reply: "no", "no no",
/// "no, keep it", "no i mean ...", or a short "no redis man". Not a status
/// such as "no target", "no files changed" or "no need to ...".
fn rejection(clause: &str) -> bool {
    if clause == "nope" || clause.starts_with("nope ") || clause.starts_with("nope,") {
        return true;
    }
    let Some(rest) = clause.strip_prefix("no") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if let Some(after) = rest.strip_prefix(',') {
        return !matches!(
            after.split_whitespace().next(),
            Some("thanks" | "thank" | "worries" | "problem")
        );
    }
    let Some(rest) = rest.strip_prefix(' ') else {
        return false;
    };
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words.as_slice() {
        ["no", ..] => true,
        [first, ..] if REJECTION_FOLLOWERS.contains(first) => true,
        // "no redis man", but not the idiom "no way" (surprise, not refusal).
        [middle, last] => {
            !matches!(*middle, "way" | "problem" | "worries")
                && matches!(*last, "man" | "dude" | "bro" | "mate" | "sis")
        }
        _ => false,
    }
}

/// The assistant ended its reply with a question ("Want me to ...?").
fn asks(reply: &str) -> bool {
    reply
        .lines()
        .rev()
        .map(|line| line.trim().trim_end_matches(['*', '_', '`', ')']))
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.ends_with('?'))
}

/// "no i mean ...": the person says the assistant got them wrong.
fn misunderstood(clause: &str) -> bool {
    ["i mean", "not what", "misunderstood", "misunderstand"]
        .iter()
        .any(|p| clause.contains(p))
}

/// "why do you merge without review?": questioning what the assistant did,
/// not asking for its reasoning ("why do you think ...").
fn challenge(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().take(4).collect();
    match words.as_slice() {
        ["why", "do" | "did" | "are" | "were" | "would" | "does" | "is", "you" | "u", rest @ ..] => {
            !matches!(
                rest.first(),
                Some(
                    &"think"
                        | &"recommend"
                        | &"suggest"
                        | &"prefer"
                        | &"believe"
                        | &"like"
                        | &"mean"
                )
            )
        }
        _ => false,
    }
}

/// "i asked you to ...", "i said no": restating an earlier request. Not a story
/// such as "i asked the reviewer", "i asked that agent" or "i said hello", nor
/// "i asked it", where "it" is as likely another agent or tool.
fn asked_again(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().take(3).collect();
    matches!(
        words.as_slice(),
        [
            "i",
            "asked" | "said",
            "you"
                | "u"
                | "for"
                | "to"
                | "twice"
                | "again"
                | "already"
                | "before"
                | "earlier"
                | "not"
                | "no"
        ]
    )
}

/// Whitespace collapsed, lowercase.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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

/// `git <args>` in `repo`, in the C locale, so that its refusals read the
/// same on every machine: `LANGUAGE` is removed too, since a git built with
/// GNU gettext (Homebrew's, on macOS) honours it over `LC_ALL=C`.
fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("LANGUAGE");
    command
}

/// What `git <args>` printed in `repo`, or `Ok(None)` when its current
/// branch was never committed to: a repository created and not yet
/// committed to, or a new orphan branch, has no history to read, which is
/// empty, not failed. git refuses both with the words it also uses for a
/// branch whose ref was lost (a loose ref deleted, `packed-refs` cut short);
/// a lost ref leaves its reflog behind ([`had_commits`]), and is an error,
/// as is every other refusal. All of it at no git process more.
fn git(repo: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = process::run(&mut git_command(repo, args), None, Duration::from_secs(30))?;
    if !output.status.success() {
        let detail = output.failure_detail();
        if let Some(branch) = detail
            .strip_prefix("fatal: your current branch '")
            .and_then(|rest| rest.strip_suffix("' does not have any commits yet"))
        {
            if !had_commits(repo, branch) {
                return Ok(None);
            }
            bail!(
                "git {}: {detail}, yet it has a reflog, so it had some: its ref is lost",
                args.join(" ")
            );
        }
        bail!("git {}: {detail}", args.join(" "));
    }
    Ok(Some(output.stdout_text()))
}

/// Whether `branch` of `repo` was ever committed to, read from the files:
/// git keeps a reflog of each branch beside the refs (unless
/// `core.logAllRefUpdates` is off), written with its first commit and kept
/// when its ref is lost. (A reftable keeps no such file; git calls a
/// damaged reftable branch broken, which warns.)
fn had_commits(repo: &Path, branch: &str) -> bool {
    common_dir(repo).is_some_and(|common| common.join("logs/refs/heads").join(branch).is_file())
}

/// The folder that holds a repository's refs and their logs: its `.git`,
/// or for a linked worktree, the one its `.git` file leads to.
fn common_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = repo.join(text.strip_prefix("gitdir:")?.trim());
    Some(match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => gitdir.join(common.trim()),
        Err(_) => gitdir,
    })
}

/// Human comments on pull requests and issues since `since`, through `gh`.
pub fn reviews(repos: &[PathBuf], since_ms: i64, warnings: &mut Vec<String>) -> Vec<Candidate> {
    let since = rfc3339(since_ms.div_euclid(1000));
    let mut out = Vec::new();
    for repo in repos {
        let Some(slug) = git(repo, &["remote", "get-url", "origin"])
            .ok()
            .flatten()
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
                        session: None,
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
            Ok(Some(log)) => log,
            // No commits yet: nothing to read, and nothing unread.
            Ok(None) => continue,
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
                    session: None,
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
                    session: Some(session.id.clone()),
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
                session: Some(session.id.clone()),
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

/// Whether what was said falls inside one of these sessions.
pub fn within_sessions(candidate: &Candidate, sessions: &[crate::sessions::Session]) -> bool {
    sessions.iter().any(|session| in_window(candidate, session))
}

/// Every candidate found that is this session's: what was said in its window
/// and repository, its handover, and what ohjaa observed in it. The session
/// is drafted from once each of them has been handled, in however many runs.
pub fn members(session: &crate::sessions::Session, found: &[Candidate]) -> Vec<Key> {
    let own = std::slice::from_ref(session);
    let said = found
        .iter()
        .filter(|c| SAID.contains(&c.source) && in_window(c, session));
    handover_candidates(own)
        .iter()
        .chain(&observed_candidates(own))
        .chain(said)
        .map(Candidate::key)
        .collect()
}

// ---- secrets ----------------------------------------------------------------
//
// What people paste to agents includes credentials: the review of #56 found
// what looks like a live key in one real message. Nothing that goes to the
// drafter keeps one. Every candidate's words, and the agent's words before
// them, are scrubbed as the prompt is made, from every source, with the
// pre-filter on or off: a token that looks like a secret becomes
// `[redacted]`. The original is never said, logged or kept; only how many
// were redacted is said. The rules are few and local: known key prefixes,
// JWTs, private key blocks, and a long mixed token right after a word that
// names a secret.

const REDACTED: &str = "[redacted]";

/// `text` with every secret-looking token replaced by `[redacted]`, and how
/// many were.
pub fn scrub(text: &str) -> (String, usize) {
    let (text, blocks) = scrub_private_keys(text);
    let (text, tokens) = scrub_tokens(&text);
    (text, blocks + tokens)
}

/// PEM private key blocks, whole: to their end line, else to the end.
fn scrub_private_keys(text: &str) -> (String, usize) {
    const BEGIN: &str = "-----BEGIN ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut count = 0;
    while let Some(begin) = rest.find(BEGIN) {
        let Some(label) = rest[begin + BEGIN.len()..].find("-----") else {
            break;
        };
        let header_end = begin + BEGIN.len() + label + "-----".len();
        if !rest[begin..header_end].contains("PRIVATE KEY") {
            out.push_str(&rest[..header_end]);
            rest = &rest[header_end..];
            continue;
        }
        out.push_str(&rest[..begin]);
        out.push_str(REDACTED);
        count += 1;
        let after = &rest[header_end..];
        rest = after
            .find("-----END ")
            .and_then(|end| {
                let label = end + "-----END ".len();
                after[label..]
                    .find("-----")
                    .map(|close| &after[label + close + "-----".len()..])
            })
            .unwrap_or("");
    }
    out.push_str(rest);
    (out, count)
}

/// The bytes a token is made of (all ASCII, so a run of them always starts
/// and ends on a character boundary).
fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'/' | b'~')
}

/// Every run of token bytes that looks like a secret, on its own or after
/// the word before it.
fn scrub_tokens(text: &str) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut count, mut copied, mut i) = (0, 0, 0);
    let mut previous: Option<(usize, usize)> = None;
    while i < bytes.len() {
        if !token_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && token_byte(bytes[i]) {
            i += 1;
        }
        // Base64 padding is the token's, unless more of a token follows it
        // (`key=value`).
        let mut end = i;
        while end < bytes.len() && bytes[end] == b'=' {
            end += 1;
        }
        if end < bytes.len() && token_byte(bytes[end]) {
            end = i;
        }
        i = end;
        // A sentence's full stop is not the token's.
        let mut stop = end;
        while stop > start && bytes[stop - 1] == b'.' {
            stop -= 1;
        }
        let run = &text[start..stop];
        let after = previous.map(|(s, e)| (&text[s..e], &text[e..start]));
        if let Some(len) = secret(run, after) {
            out.push_str(&text[copied..start]);
            out.push_str(REDACTED);
            copied = start + len;
            count += 1;
        }
        previous = Some((start, stop));
    }
    out.push_str(&text[copied..]);
    (out, count)
}

/// How much of `run` is a secret, if any: a known key prefix, a JWT, or a
/// long mixed token right after a word that names a secret (`after`: that
/// word, and what stands between).
fn secret(run: &str, after: Option<(&str, &str)>) -> Option<usize> {
    let base64url = |part: &str| {
        part.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    let parts: Vec<&str> = run.split('.').collect();
    if run.starts_with("eyJ")
        && parts.len() == 3
        && parts.iter().all(|p| p.len() >= 10 && base64url(p))
    {
        return Some(run.len());
    }
    let head = &run[..run
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(run.len())];
    let mixed = |s: &str| {
        s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().any(|b| b.is_ascii_alphabetic())
    };
    let prefixed = |prefix: &str, least: usize, ok: &dyn Fn(&str) -> bool| {
        head.strip_prefix(prefix)
            .is_some_and(|rest| rest.len() >= least && ok(rest))
    };
    let alnum = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric());
    let aws = |s: &str| {
        s.len() == 16
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    let found = prefixed("sk-", 20, &mixed)
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
            .iter()
            .any(|p| prefixed(p, 30, &alnum))
        || prefixed("github_pat_", 20, &mixed)
        || ["xoxb-", "xoxp-", "xoxa-", "xoxs-"]
            .iter()
            .any(|p| prefixed(p, 10, &mixed))
        || prefixed("AKIA", 16, &aws)
        || prefixed("ASIA", 16, &aws)
        || prefixed("glpat-", 20, &mixed)
        || prefixed("AIza", 30, &|_| true);
    if found {
        return Some(head.len());
    }
    // A key of a vendor not named here, alone: an id and a secret joined by
    // a dot, or a long token mixing capitals, lower case and digits.
    if dotted_key(run) || mixes_cases_and_digits(run) {
        return Some(run.len());
    }
    let (word, between) = after?;
    let separated = (1..=4).contains(&between.len())
        && between
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t' | b':' | b'=' | b'"' | b'\''));
    let distinct = run.bytes().collect::<BTreeSet<u8>>().len();
    let long_and_mixed =
        run.len() >= 16 && mixed(run) && distinct >= 8 && !run.starts_with(['/', '~', '.']);
    (separated && names_a_secret(word) && long_and_mixed).then_some(run.len())
}

/// `<hex, 24 or more>.<letters and digits, 12 or more>`: an id and a secret
/// joined by a dot, as some providers issue keys.
fn dotted_key(run: &str) -> bool {
    let parts: Vec<&str> = run.split('.').collect();
    parts.windows(2).any(|pair| {
        pair[0].len() >= 24
            && pair[0].bytes().all(|b| b.is_ascii_hexdigit())
            && pair[1].len() >= 12
            && pair[1].bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

/// 32 or more characters with no space, two or more each of capitals, lower
/// case letters and digits: what a key with no known prefix looks like. A
/// hash or a UUID, in either case, is hex and is left alone. A path is
/// judged by the pieces of its names (between `/`, `.`, `-` and `_`), so a
/// long path, or a versioned or dated file name, made of short words is no
/// key; a Codex rollout name has one capital besides. A long name with digits and
/// capitals (`ProjectSettingsV2Controller3`) is redacted too: a cheap loss
/// to the drafter.
fn mixes_cases_and_digits(run: &str) -> bool {
    let long_and_mixed = |part: &str| {
        let count = |kind: fn(&u8) -> bool| part.bytes().filter(kind).count();
        part.len() >= 32
            && count(u8::is_ascii_uppercase) >= 2
            && count(u8::is_ascii_lowercase) >= 2
            && count(u8::is_ascii_digit) >= 2
            && !part.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    };
    if run.starts_with(['/', '~', '.']) {
        run.split(['/', '.', '-', '_']).any(long_and_mixed)
    } else {
        long_and_mixed(run)
    }
}

/// `key`, `token`, `secret`, `password`, `passwd` or `bearer`, alone or
/// ending a name (`api_key`, `GITHUB_TOKEN`, `apiKey`), never inside a word
/// (`monkey`).
fn names_a_secret(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    ["key", "token", "secret", "password", "passwd", "bearer"]
        .iter()
        .any(|name| {
            lower.ends_with(name) && {
                let cut = word.len() - name.len();
                cut == 0
                    || matches!(word.as_bytes()[cut - 1], b'_' | b'-' | b'.')
                    || word.as_bytes()[cut].is_ascii_uppercase()
                        && word.as_bytes()[cut - 1].is_ascii_lowercase()
            }
        })
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
nothing, and an empty answer is a good answer. Never invent: every node must be
supported by the quoted words.

After the instructions comes the knowledge already present: the current nodes,
and the proposals waiting for a person to review them. Return nothing for a
lesson already present, current or waiting, however it would be worded and under
whatever id: every draft costs that person a review, and a second draft of one
lesson is only noise. If a candidate adds to a present node, name that node's id
in \"refines\" and say what it adds, instead of drafting a new node beside it.

A person who corrects an agent on what a current node already says is teaching
it again: the node is there, and it did not change what the agent did (\"no
Python\", \"use Lima\", \"I asked you twice\"). For such a candidate, name each
current node it re-teaches in \"reteaches\", by the candidate's number, and draft
nothing for it, unless it also adds to the node: then draft a refinement too.
Only a person's words re-teach (a message in an agent session, a review
comment), never an observed failure, a handover or a revert. Name only nodes
listed with status current, by their exact ids.

Scopes:
- ours: team-wide decisions and taste that hold in every project
- repo: specific to one project; give its absolute path in \"repo\"
- mine: how this one person likes their agents to work with them
- machine: facts about this computer (installed tools, VMs, paths)

Kinds: decision (chosen, binding), fact (true about the world), taste
(preference about how), lesson (learned from a correction or failure).

Output ONLY one JSON object, no prose, no code fence:
{\"drafts\": [<node>, ...],
 \"reteaches\": [{\"candidate\": <its number, as in [1]>, \"ids\": [\"current-id\", ...]}]}
Leave a candidate that re-teaches nothing out of \"reteaches\"; when nothing
carries anything, answer {\"drafts\": [], \"reteaches\": []}. Each node:
{\"scope\": \"ours|repo|mine|machine\", \"repo\": \"/absolute/path or null\",
 \"id\": \"kebab-case, at most 64 characters, not an id already present\",
 \"kind\": \"decision|fact|taste|lesson\",
 \"body\": \"one to three plain sentences; imperative where it is a rule\",
 \"origin\": [\"<date> <place>: '<the person's words, under 20 words>'\"],
 \"refines\": \"existing-id or null\", \"supersedes\": []}
";

/// The prompt for one batch of candidates, and how many secret-looking
/// tokens were redacted from it. Everything in it but the instructions is
/// scrubbed of secrets (see [`scrub`]) and counted: the knowledge already
/// present (each line whole, before it is cut to fit [`KNOWN_BUDGET`]), and
/// each candidate's time, source, place and author, its words and the
/// agent's words before them.
pub fn prompt(known: &Known, batch: &[Candidate]) -> (String, usize) {
    let (text, redacted, _) = prompt_with_evidence(known, batch);
    (text, redacted)
}

/// The prompt and the exact knowledge sent, for validating its answer.
pub fn prompt_with_evidence(known: &Known, batch: &[Candidate]) -> (String, usize, Shown) {
    let mut out = String::from(INSTRUCTIONS);
    let mut redacted = 0;
    let shown = known.render(&mut out, &mut redacted, batch);
    out.push_str("\nCandidates:\n");
    for (index, candidate) in batch.iter().enumerate() {
        let (header, n) = scrub(&format!(
            "[{}] {} · {} · {} · {}",
            index + 1,
            candidate.at,
            candidate.source,
            candidate.place,
            candidate.who
        ));
        redacted += n;
        let _ = writeln!(out, "\n{}", header.replace('\n', " "));
        if !candidate.context.is_empty() {
            let (context, n) = scrub(&candidate.context);
            redacted += n;
            let _ = writeln!(out, "    before: {}", context.replace('\n', " "));
        }
        let (text, n) = scrub(&candidate.text);
        redacted += n;
        let _ = writeln!(out, "    said: {}", text.replace('\n', " "));
    }
    (out, redacted, shown)
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

/// What the drafter answers for one batch: its drafts, and what each
/// candidate re-teaches of the current knowledge.
#[derive(Debug, Default)]
pub struct Answer {
    pub drafts: Vec<Draft>,
    /// Each `{"candidate": <n>, "ids": [...]}` as the drafter wrote it: read
    /// and checked one by one ([`Shown::retaught`]), so one it got wrong
    /// costs only itself.
    pub reteaches: Vec<Value>,
}

#[derive(Deserialize)]
struct AnswerObject {
    drafts: Option<Vec<Draft>>,
    reteaches: Option<Vec<Value>>,
}

/// The drafter's answer: a JSON object holding `drafts` or `reteaches` (or
/// both), or, as it answered before it was asked what each candidate
/// re-teaches, an array of drafts. Prose or a code fence around it is
/// tolerated. The first enclosing JSON value is read: an invalid object
/// must never be recovered as one of its nested legacy arrays.
pub fn parse_answer(answer: &str) -> Result<Answer> {
    let mut remaining = answer;
    loop {
        let start = remaining
            .find(['{', '['])
            .context("the drafter did not answer with a JSON object")?;
        let source = &remaining[start..];
        let mut values = serde_json::Deserializer::from_str(source).into_iter::<Value>();
        let value = values
            .next()
            .context("the drafter gave no JSON value")?
            .context("the drafter's answer is not valid JSON")?;
        if value.is_array() {
            match serde_json::from_value(value) {
                Ok(drafts) => {
                    return Ok(Answer {
                        drafts,
                        reteaches: Vec::new(),
                    })
                }
                Err(_) => {
                    // Prose can contain references such as Notes [1]. Skip the
                    // whole parsed value, never scan inside an invalid object.
                    remaining = &source[values.byte_offset()..];
                    continue;
                }
            }
        }
        let object: AnswerObject = serde_json::from_value(value)
            .context("the drafter's answer is not drafts and what they re-teach")?;
        if object.drafts.is_none() && object.reteaches.is_none() {
            bail!("the drafter's answer contains neither drafts nor re-teachings");
        }
        return Ok(Answer {
            drafts: object.drafts.unwrap_or_default(),
            reteaches: object.reteaches.unwrap_or_default(),
        });
    }
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
pub fn draft(drafter: &str, prompt: &str) -> Result<Answer, Failure> {
    let output = process::run(
        Command::new("sh").args(["-c", drafter]),
        Some(prompt.as_bytes()),
        Duration::from_secs(600),
    )
    .map_err(Failure::Silent)?;
    if !output.status.success() {
        let detail = anyhow!("{}", output.failure_detail());
        return Err(Failure::Refused(
            detail.context(format!("the drafter failed ({})", output.status)),
        ));
    }
    parse_answer(&output.stdout_text()).map_err(Failure::Refused)
}

// ---- what a person had to teach again -----------------------------------------
//
// A candidate the drafter says re-teaches a current node is the evidence that
// the node did not change what an agent did ([`crate::evidence`]). Its
// answer is checked as every draft is: a re-teaching is kept only when it
// names a candidate of the batch, that candidate is a person's words, and the
// id is one current node shown with that candidate (a repo node only with a
// candidate from its repository). Everything else is dropped, and counted.

/// What a person wrote, as against what ohjaa observed or an agent said: only
/// that re-teaches.
fn a_persons_words(candidate: &Candidate) -> bool {
    SAID.contains(&candidate.source) || candidate.source == "review"
}

/// A re-teaching the drafter named, as kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retaught {
    pub node: crate::evidence::NodeKey,
    /// The message: its key, when it was said, where from, and in which
    /// session.
    pub key: Key,
    pub at: String,
    pub source: &'static str,
    pub session: Option<String>,
}

/// The re-teachings the drafter named that were not kept, by why.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dropped {
    /// An id no current node shown with its candidate has (proposed,
    /// superseded, retired, unknown, of another repository, or in two scopes).
    pub not_current: usize,
    /// Of a candidate that is not a person's words.
    pub not_said: usize,
    /// Of a number no candidate of the batch has.
    pub no_candidate: usize,
    /// An entry, or an id in one, that is not what was asked for.
    pub unreadable: usize,
}

impl Dropped {
    pub fn total(&self) -> usize {
        self.not_current + self.not_said + self.no_candidate + self.unreadable
    }

    pub fn add(&mut self, other: Dropped) {
        self.not_current += other.not_current;
        self.not_said += other.not_said;
        self.no_candidate += other.no_candidate;
        self.unreadable += other.unreadable;
    }

    pub fn describe(&self) -> String {
        [
            (
                self.not_current,
                "naming no current node shown with its candidate",
            ),
            (self.not_said, "of a candidate that is not a person's words"),
            (self.no_candidate, "of no candidate in its batch"),
            (self.unreadable, "unreadable"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}"))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

// ---- what is already present ------------------------------------------------
//
// Every draft costs a person a review. The drafter is shown what is already
// present where the batch's candidates come from, current or waiting for
// that person, and told to return nothing for it; and its answer is checked
// here too. A draft whose title mostly repeats a present node's, in the same
// scope (and repository), may be its duplicate. Only a person decides that:
// it is written like any other proposal, and said as a *possible duplicate*
// of that node, in the run and whenever `ohjaa review` shows it (after that
// node), derived each time and stored never. Each draft written in a run is
// present for every draft after it, in its batch and in the batches after.

/// At most this many characters of the prompt list what is already present.
const KNOWN_BUDGET: usize = 24_000;
/// At most this many characters of a node's first line stand for it there.
const SUMMARY_CHARS: usize = 100;

/// A node a draft may repeat: one in a scope directory, or a proposal
/// waiting for a person.
#[derive(Debug, Clone)]
pub struct Present {
    id: String,
    scope: Scope,
    /// For a repo node: its repository.
    repo: Option<PathBuf>,
    kind: Kind,
    status: Status,
    /// The first line of its body.
    summary: String,
    words: BTreeSet<String>,
}

impl Present {
    fn new(scope: Scope, repo: Option<&Path>, node: &Node) -> Present {
        Present {
            id: node.id.clone(),
            scope,
            repo: repo.filter(|_| scope == Scope::Repo).map(Path::to_path_buf),
            kind: node.kind,
            status: node.status,
            summary: node.body.lines().next().unwrap_or("").to_owned(),
            words: title_words(&node.id),
        }
    }

    /// A proposal just written, of `repo` for a repo one: waiting, like
    /// those written before this run.
    fn proposed(proposal: &Proposal, repo: Option<PathBuf>) -> Present {
        Present {
            id: proposal.id.clone(),
            scope: proposal.scope,
            repo: repo.filter(|_| proposal.scope == Scope::Repo),
            kind: proposal.kind,
            status: Status::Proposed,
            summary: proposal.body.trim().lines().next().unwrap_or("").to_owned(),
            words: title_words(&proposal.id),
        }
    }

    /// Present as a lesson: current, or proposed and waiting. A superseded or
    /// retired node is shown to the drafter, and is not repeated by drafting
    /// it again: a person put it aside, and may want to see it come back.
    fn holds(&self) -> bool {
        matches!(self.status, Status::Current | Status::Proposed)
    }

    /// Whether the drafter is shown it with `batch`: every node of ours,
    /// mine and machine, and a repo node only when the batch speaks of its
    /// repository ([`concerns`]).
    fn shown_with(&self, batch: &[Candidate]) -> bool {
        self.scope != Scope::Repo
            || self
                .repo
                .as_deref()
                .is_some_and(|repo| concerns(repo, batch))
    }
}

/// Whether a batch speaks of `repo`: a candidate's place (or, for a review,
/// the repository it was read from) is the repository or lies in it, or one
/// of the place's folders is named after it, as a pull request's URL is, and
/// a worktree named `<repo>-<topic>` (the `_worktrees` convention).
fn concerns(repo: &Path, batch: &[Candidate]) -> bool {
    let Some(name) = repo.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    batch
        .iter()
        .flat_map(|c| std::iter::once(&c.place).chain((c.source == "review").then_some(&c.context)))
        .any(|place| {
            let place = Path::new(place);
            place.starts_with(repo)
                || place.components().any(|part| {
                    part.as_os_str().to_str().is_some_and(|part| {
                        part.strip_prefix(name)
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
                    })
                })
        })
}

/// What is already present, in the order the drafter is shown it: every node
/// in the scope directories, then the proposals waiting beside them.
#[derive(Debug, Clone, Default)]
pub struct Known {
    nodes: Vec<Present>,
    /// The repository ohjaa runs in: a repo proposal naming none is its.
    repo_root: Option<PathBuf>,
}

/// Only nodes actually rendered into a prompt, after its shared size limit.
/// Evidence checks cannot be called on an unbounded on-disk knowledge set.
pub struct Shown {
    nodes: Vec<Present>,
}

impl Known {
    /// Everything loaded, and every proposal waiting for a person.
    pub fn read(dirs: &Dirs, loaded: &Loaded) -> Result<Known> {
        let (waiting, _) = knowledge::pending(dirs, loaded)?;
        Ok(Known::of(loaded, &waiting, dirs.repo_root.as_deref()))
    }

    /// The repo nodes `loaded` holds are `repo_root`'s. A proposed node
    /// committed into a scope directory is in both `loaded` and `waiting`;
    /// it is listed once.
    pub fn of(loaded: &Loaded, waiting: &[knowledge::Pending], repo_root: Option<&Path>) -> Known {
        let nodes = loaded
            .nodes
            .iter()
            .map(|located| Present::new(located.scope, repo_root, &located.node))
            .chain(
                waiting
                    .iter()
                    .filter(|pending| pending.staged)
                    .map(|pending| {
                        Present::new(pending.scope, pending.repo.as_deref(), &pending.node)
                    }),
            )
            .collect();
        Known {
            nodes,
            repo_root: repo_root.map(Path::to_path_buf),
        }
    }

    /// The repository of a repo proposal, as `propose` files it: the one it
    /// names, else the one ohjaa runs in.
    fn repo_of(&self, proposal: &Proposal) -> Option<PathBuf> {
        (proposal.scope == Scope::Repo)
            .then(|| proposal.repo.clone().or_else(|| self.repo_root.clone()))
            .flatten()
    }

    /// `proposal` as present once it is written, waiting.
    pub fn present(&self, proposal: &Proposal) -> Present {
        Present::proposed(proposal, self.repo_of(proposal))
    }

    pub fn add(&mut self, present: Present) {
        self.nodes.push(present);
    }

    /// The id of the present node `proposal` may repeat, if any (see
    /// [`nearest`]). A person decides whether it does.
    pub fn repeated_by(&self, proposal: &Proposal) -> Option<&str> {
        nearest(
            self.nodes.iter(),
            proposal.scope,
            self.repo_of(proposal).as_deref(),
            &proposal.id,
            proposal.refines.as_deref(),
            &proposal.supersedes,
        )
    }
}

impl Shown {
    /// The one current node `id` names that the drafter was shown with
    /// `with`: none when no current node has it there, or two do.
    fn current(&self, id: &str, with: &[Candidate]) -> Option<crate::evidence::NodeKey> {
        let mut found = self.nodes.iter().filter(|present| {
            present.status == Status::Current && present.id == id && present.shown_with(with)
        });
        let one = found.next()?;
        found
            .next()
            .is_none()
            .then(|| crate::evidence::NodeKey::new(one.scope, one.repo.as_deref(), &one.id))
    }

    /// The re-teachings the drafter named for `batch` that are kept (see
    /// [`a_persons_words`] and [`Known::current`]), each pair of a candidate
    /// and a node once, in the order named; and what was dropped, by why.
    pub fn retaught(&self, reteaches: &[Value], batch: &[Candidate]) -> (Vec<Retaught>, Dropped) {
        let mut kept: Vec<Retaught> = Vec::new();
        let mut dropped = Dropped::default();
        let mut named: BTreeSet<(usize, String)> = BTreeSet::new();
        for entry in reteaches {
            let number = entry.get("candidate").and_then(|n| {
                n.as_u64()
                    .or_else(|| n.as_str().and_then(|s| s.trim().parse().ok()))
            });
            let ids = entry.get("ids").and_then(Value::as_array);
            let (Some(number), Some(ids)) = (number, ids) else {
                dropped.unreadable += ids.map_or(1, |ids| ids.len().max(1));
                continue;
            };
            let candidate = usize::try_from(number)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| batch.get(i).map(|c| (i, c)));
            for id in ids {
                let Some(id) = id.as_str() else {
                    dropped.unreadable += 1;
                    continue;
                };
                let Some((index, candidate)) = candidate else {
                    dropped.no_candidate += 1;
                    continue;
                };
                if !named.insert((index, id.to_owned())) {
                    continue;
                }
                if !a_persons_words(candidate) {
                    dropped.not_said += 1;
                    continue;
                }
                match self.current(id, std::slice::from_ref(candidate)) {
                    Some(node) => kept.push(Retaught {
                        node,
                        key: candidate.key(),
                        at: candidate.at.clone(),
                        source: candidate.source,
                        session: candidate.session.clone(),
                    }),
                    None => dropped.not_current += 1,
                }
            }
        }
        (kept, dropped)
    }

    /// The current nodes among `ids` (what drafts from `batch` refine or
    /// supersede) the drafter was shown with it: those they cite.
    pub fn cited(&self, ids: &[String], batch: &[Candidate]) -> Vec<crate::evidence::NodeKey> {
        let mut cited: Vec<crate::evidence::NodeKey> = ids
            .iter()
            .filter_map(|id| self.current(id, batch))
            .collect();
        cited.sort();
        cited.dedup();
        cited
    }
}

impl Known {
    /// The list the drafter is shown with `batch`, within [`KNOWN_BUDGET`]
    /// characters: what is present where it comes from ([`Present::shown_with`]),
    /// current knowledge first (then what was superseded or retired, then
    /// what waits for a person), each in the order stored; what does not fit
    /// is counted. Each line is scrubbed whole, then cut, then measured; only
    /// what is sent adds to `redacted`.
    fn render(&self, out: &mut String, redacted: &mut usize, batch: &[Candidate]) -> Shown {
        let (waiting, mut other): (Vec<&Present>, Vec<&Present>) = self
            .nodes
            .iter()
            .filter(|present| present.shown_with(batch))
            .partition(|present| present.status == Status::Proposed);
        // Stable: the order stored, within each.
        other.sort_by_key(|present| present.status != Status::Current);
        let mut budget = KNOWN_BUDGET;
        let mut sent = Vec::new();
        let mut list = |heading: &str, nodes: &[&Present], out: &mut String| {
            out.push_str(heading);
            if nodes.is_empty() {
                out.push_str("(none)\n");
            }
            let mut shown = 0;
            for present in nodes {
                let (head, in_head) = scrub(&format!(
                    "- {} [{}/{}/{}]{}",
                    present.id,
                    present.scope.name(),
                    present.kind.name(),
                    present.status.name(),
                    present
                        .repo
                        .as_ref()
                        .map(|repo| format!(" in {}", repo.display()))
                        .unwrap_or_default(),
                ));
                let (first, in_first) = scrub(&present.summary);
                let mut summary: String = first.chars().take(SUMMARY_CHARS).collect();
                if summary.len() < first.len() {
                    summary.push('…');
                }
                let line = format!("{}: {summary}\n", head.replace('\n', " "));
                if line.len() > budget {
                    break;
                }
                budget -= line.len();
                *redacted += in_head + in_first;
                out.push_str(&line);
                // Evidence can name only an identifier that survived scrubbing
                // and normalization in the header actually sent to the drafter.
                if line.starts_with(&format!("- {} [", present.id)) {
                    sent.push((**present).clone());
                }
                shown += 1;
            }
            if shown < nodes.len() {
                let _ = writeln!(out, "- … and {} more, not shown", nodes.len() - shown);
            }
        };
        list(
            "\nExisting knowledge (its status says whether it is current):\n",
            &other,
            out,
        );
        list(
            "\nProposals waiting for a person to review (present too: return nothing for a lesson \
             one of them holds):\n",
            &waiting,
            out,
        );
        Shown { nodes: sent }
    }
}

/// The id of the node among `nodes` that the node `id` may repeat, if any:
/// one present ([`Present::holds`]), of the same scope (and, for a repo one,
/// the same repository), whose title its own says mostly again ([`near`]);
/// the nearest, and of those as near the first. The node it refines,
/// current or waiting, and those it supersedes, are not compared: a
/// refinement is meant to say its base again, and to add to it.
fn nearest<'a>(
    nodes: impl Iterator<Item = &'a Present>,
    scope: Scope,
    repo: Option<&Path>,
    id: &str,
    refines: Option<&str>,
    supersedes: &[String],
) -> Option<&'a str> {
    let words = title_words(id);
    nodes
        .filter(|present| present.holds() && present.id != id)
        .filter(|present| {
            present.scope == scope && (scope != Scope::Repo || present.repo.as_deref() == repo)
        })
        .filter(|present| refines != Some(present.id.as_str()) && !supersedes.contains(&present.id))
        .filter_map(|present| near(&words, &present.words).map(|score| (score, present)))
        .min_by_key(|(score, _)| std::cmp::Reverse(*score))
        .map(|(_, present)| present.id.as_str())
}

/// For each of `waiting`, in its order, the id of the node it may repeat,
/// derived now ([`nearest`]) and stored never: among current knowledge and
/// the proposals that came before it, older, or as old and stored first. Of
/// two near proposals the later is the possible duplicate, never both.
/// Superseded and retired nodes are not present. A person decides.
pub fn possible_duplicates(
    loaded: &Loaded,
    waiting: &[knowledge::Pending],
    repo_root: Option<&Path>,
) -> Vec<Option<String>> {
    let current: Vec<Present> = loaded
        .nodes
        .iter()
        .filter(|located| located.node.status != Status::Proposed)
        .map(|located| Present::new(located.scope, repo_root, &located.node))
        .collect();
    let proposed: Vec<Present> = waiting
        .iter()
        .map(|pending| Present::new(pending.scope, pending.repo.as_deref(), &pending.node))
        .collect();
    let came = |i: usize| (waiting[i].node.as_of, i);
    waiting
        .iter()
        .enumerate()
        .map(|(i, pending)| {
            let before = proposed
                .iter()
                .enumerate()
                .filter(|(j, _)| came(*j) < came(i))
                .map(|(_, present)| present);
            nearest(
                current.iter().chain(before),
                pending.scope,
                pending.repo.as_deref(),
                &pending.node.id,
                pending.node.refines.as_deref(),
                &pending.node.supersedes,
            )
            .map(str::to_owned)
        })
        .collect()
}

/// The waiting proposals, each with what it may repeat, in the order a
/// person reviews them: as found, with each possible duplicate right after
/// the one it may repeat, when that one waits too.
pub fn review_order(
    waiting: Vec<(knowledge::Pending, Option<String>)>,
) -> Vec<(knowledge::Pending, Option<String>)> {
    let ids: BTreeSet<String> = waiting.iter().map(|(p, _)| p.node.id.clone()).collect();
    let follows = |of: &Option<String>| of.as_ref().filter(|of| ids.contains(*of)).cloned();
    let mut rest: Vec<Option<(knowledge::Pending, Option<String>)>> =
        waiting.into_iter().map(Some).collect();
    let mut order = Vec::with_capacity(rest.len());
    for start in 0..rest.len() {
        if rest[start]
            .as_ref()
            .is_none_or(|(_, of)| follows(of).is_some())
        {
            continue;
        }
        let mut stack = vec![start];
        while let Some(at) = stack.pop() {
            let Some(item) = rest[at].take() else {
                continue;
            };
            // Its followers in their order, the first on top.
            let after: Vec<usize> = (0..rest.len())
                .filter(|i| {
                    rest[*i].as_ref().is_some_and(|(_, of)| {
                        follows(of).as_deref() == Some(item.0.node.id.as_str())
                    })
                })
                .collect();
            stack.extend(after.into_iter().rev());
            order.push(item);
        }
    }
    order.extend(rest.into_iter().flatten());
    order
}

/// Words that carry no lesson in a title.
const FILLER: [&str; 45] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "has",
    "have", "how", "in", "into", "is", "it", "its", "must", "of", "on", "onto", "or", "should",
    "so", "than", "that", "the", "then", "this", "to", "up", "via", "was", "were", "what", "when",
    "where", "which", "who", "will", "with",
];

/// The words of a node's title. A node has no title but its id, kebab-case
/// words: they are split at every character that is not a letter or digit,
/// lowercased, filler dropped, and one common English ending cut from each
/// (`-ing`, `-ed`, `-es`, `-s` but not `-ss`, `-e`) when three letters or
/// more remain. So `start-sessions-through-ohjaa-run` and
/// `start-a-session-through-ohjaa-run` say the same words, and
/// `prove-with-ohjaa-gate-before-merging` says `merg` as `merge` does.
pub fn title_words(id: &str) -> BTreeSet<String> {
    id.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| !word.is_empty() && !FILLER.contains(&word.as_str()))
        .map(|word| {
            let cut = ["ing", "ed", "es", "s", "e"].iter().find_map(|ending| {
                let stem = word.strip_suffix(ending)?;
                (stem.chars().count() >= 3 && !(*ending == "s" && stem.ends_with('s')))
                    .then_some(stem)
            });
            cut.unwrap_or(&word).to_owned()
        })
        .collect()
}

/// Whether two titles say mostly the same words, and how nearly (in
/// thousandths): they share at least two words, and the shared words are at
/// least 60% of all their words, counted as Dice's coefficient,
/// 2·|shared| / (|a| + |b|) ≥ 0.6. `read-back-briefing-and-leave-handover`
/// and `read-briefing-and-leave-handover` share 4 of 5 and 4 words (0.89);
/// `start-agents-through-ohjaa-run` and `start-sessions-through-ohjaa-run`
/// 4 of 5 and 5 (0.8); `gate-before-pushing-main` and
/// `prove-with-ohjaa-gate-before-merging` only 2 of 4 and 5 (0.44), which
/// the drafter, shown both, is told to see itself.
pub fn near(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Option<usize> {
    let shared = a.intersection(b).count();
    let total = a.len() + b.len();
    (shared >= 2 && 10 * shared >= 3 * total).then(|| 2_000 * shared / total)
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
    // Keep the millisecond, so a message in the same second is not re-read.
    std::fs::write(cursor_path(dirs), format!("{}\n", rfc3339_ms(at_ms)))?;
    Ok(())
}

// ---- progress ---------------------------------------------------------------
//
// A run drafts from at most its cap, so a queue can take several runs. Each
// candidate the drafter has answered for is *handled*, and kept the moment
// its batch is answered (`intake-handled`, beside the cursor): the next run
// continues where this one stopped, a failure keeps what came before it, and
// nothing answered for is sent again. A batch the drafter cannot answer is
// split until the candidate it fails on stands alone; one that fails alone
// `MAX_ATTEMPTS` times, over as many runs, is set aside and said, never
// dropped in silence. The cursor moves only past handled candidates, and only
// when every source was read, so it never needs to move back.

/// How often a candidate may fail alone before it is set aside.
pub const MAX_ATTEMPTS: u8 = 3;

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

/// A candidate the drafter failed on alone: how often, and why the last time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failing {
    pub attempts: u8,
    /// Where it came from (`claude`, `review`, …), to say it by.
    pub source: String,
    /// Why, in ohjaa's words: never the drafter's answer, never the words.
    pub why: String,
}

/// What earlier runs did with each candidate: answered for (kept until no
/// later run can find it again), or failed alone (kept until the drafter
/// answers for it; set aside once it has failed `MAX_ATTEMPTS` times).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Handled {
    answered: BTreeSet<Key>,
    failing: BTreeMap<Key, Failing>,
    /// This run sends the set-aside candidates again (`--retry-failed`):
    /// until it is done drafting, they are not done with. Never kept.
    retrying: bool,
}

impl Handled {
    /// Done with: answered for, or set aside.
    pub fn contains(&self, candidate: &Candidate) -> bool {
        self.has(&candidate.key())
    }

    fn has(&self, key: &Key) -> bool {
        self.answered.contains(key)
            || (!self.retrying
                && self
                    .failing
                    .get(key)
                    .is_some_and(|f| f.attempts >= MAX_ATTEMPTS))
    }

    fn answer(&mut self, chunk: &[Candidate]) {
        for candidate in chunk {
            let key = candidate.key();
            self.failing.remove(&key);
            self.answered.insert(key);
        }
    }

    /// One more failure of `candidate` alone.
    fn fail(&mut self, candidate: &Candidate, why: String) -> Failing {
        let failing = self.failing.entry(candidate.key()).or_insert(Failing {
            attempts: 0,
            source: candidate.source.to_owned(),
            why: String::new(),
        });
        failing.attempts = failing.attempts.saturating_add(1);
        failing.why = why;
        failing.clone()
    }

    /// The candidates set aside: when, from where, and why.
    pub fn set_aside(&self) -> impl Iterator<Item = (&Key, &Failing)> {
        self.failing
            .iter()
            .filter(|(_, f)| f.attempts >= MAX_ATTEMPTS)
    }

    /// Send the set-aside candidates again in this run, from their records,
    /// which stay until the drafter answers for them.
    pub fn retry_failed(&mut self) {
        self.retrying = true;
    }

    /// Drafting is done: a set-aside candidate the drafter failed on again
    /// is set aside again.
    pub fn done_retrying(&mut self) {
        self.retrying = false;
    }

    fn is_empty(&self) -> bool {
        self.answered.is_empty() && self.failing.is_empty()
    }

    /// Forget answered keys that `keep` does not; a failure stays until the
    /// drafter answers for it, so a retry can find it.
    fn retain(&mut self, keep: impl Fn(&Key) -> bool) {
        self.answered.retain(|key| keep(key));
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

/// Why the drafter gave no drafts for a batch.
#[derive(Debug)]
pub enum Failure {
    /// No answer at all: it could not be asked, or did not finish in time.
    /// Splitting the batch would not help, so the run stops.
    Silent(anyhow::Error),
    /// It answered, but not with drafts (it failed, or said something else):
    /// the batch may hold a candidate it cannot take, so it is split.
    Refused(anyhow::Error),
}

/// Why, in ohjaa's words: the outermost message only (the drafter's answer
/// and output stay out), on one short line.
fn why(error: &anyhow::Error) -> String {
    error
        .to_string()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(120)
        .collect()
}

/// What one run's drafting came to.
#[derive(Debug, Default)]
pub struct Drafted {
    /// Candidates the drafter answered for in this run.
    pub sent: usize,
    /// The candidates it failed on alone in this run, as they stand now
    /// (`MAX_ATTEMPTS` attempts: set aside).
    pub failed: Vec<(Key, Failing)>,
    /// Why the run stopped short of its cap, when it did.
    pub stopped: Option<anyhow::Error>,
}

/// Draft from up to `cap` candidates of `found` not handled before, in order,
/// `batch` at a time. Each batch the drafter answered for (`send`) becomes
/// handled and is kept (`keep`) before the next is sent, so a failure loses
/// nothing that came before it. A batch it refuses is split in two, and each
/// half sent, down to the one candidate it fails on, whose failure is kept
/// too: every run makes progress, even past a candidate the drafter never
/// takes. Two candidates failing alone in a row look like a drafter that
/// answers nothing, and stop the run.
pub fn draft_pending(
    found: &[Candidate],
    handled: &mut Handled,
    batch: usize,
    cap: usize,
    mut send: impl FnMut(&[Candidate]) -> Result<(), Failure>,
    mut keep: impl FnMut(&Handled) -> Result<()>,
) -> Drafted {
    let pending: Vec<Candidate> = found
        .iter()
        .filter(|c| !handled.contains(c))
        .take(cap)
        .cloned()
        .collect();
    let mut work: std::collections::VecDeque<&[Candidate]> = pending.chunks(batch.max(1)).collect();
    let mut drafted = Drafted::default();
    let mut alone_in_a_row = 0;
    while let Some(chunk) = work.pop_front() {
        match send(chunk) {
            Ok(()) => {
                handled.answer(chunk);
                drafted.sent += chunk.len();
                alone_in_a_row = 0;
            }
            Err(Failure::Silent(error)) => {
                drafted.stopped = Some(error);
                break;
            }
            Err(Failure::Refused(_)) if chunk.len() > 1 => {
                let (left, right) = chunk.split_at(chunk.len() / 2);
                work.push_front(right);
                work.push_front(left);
                continue;
            }
            Err(Failure::Refused(error)) => {
                let candidate = &chunk[0];
                let failing = handled.fail(candidate, why(&error));
                drafted.failed.push((candidate.key(), failing));
                alone_in_a_row += 1;
                if alone_in_a_row >= 2 {
                    drafted.stopped =
                        Some(error.context("the drafter failed on two candidates in a row, alone"));
                }
            }
        }
        if let Err(error) = keep(handled) {
            drafted.stopped = Some(error.context("cannot keep intake's progress"));
            break;
        }
        if drafted.stopped.is_some() {
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
    /// Every source was read, and none with a warning. Only then may the
    /// cursor move: it is one for all of them, and must not pass what a
    /// source it skipped, or could not read, still holds.
    pub complete: bool,
}

/// Settle a run, and forget what no later run can find again.
///
/// A queued session brings its own candidates back to every run until each
/// is handled. Any other candidate comes back only while it lies after the
/// cursor, so the first of them not handled holds the cursor: it moves past
/// the handled candidates found after `since`, up to that one, or to
/// `started` when none is left. It moves only after a complete run, and
/// never to `since` or before. `sessions` is every session not yet drafted
/// from, or `None` when the log could not be read; then nothing is
/// forgotten.
pub fn settle(
    found: &[Candidate],
    handled: &mut Handled,
    sessions: Option<&[Undrafted]>,
    window: Window,
) -> Settled {
    let Window {
        since,
        started,
        complete,
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
        _ if !complete => None,
        None => Some(started),
        Some(_) => through,
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
                    .is_some_and(|members| members.iter().all(|key| handled.answered.contains(key)))
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
        handled.retain(|key| key.at_ms > cursor || key.at_ms >= floor || seen.contains(key));
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

/// The first line of `intake-handled`: what it is, and in which form. A
/// change to how candidates are rendered changes their digests, so it comes
/// with a new version, by choice.
const HANDLED_HEADER: &str = "ohjaa intake-handled v1";

/// What earlier runs handled. Damaged progress (empty, cut short, or not in
/// this form) is an error, never read as less, which would send it again.
pub fn read_handled(dirs: &Dirs) -> Result<Handled> {
    let path = handled_path(dirs);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Handled::default()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    parse_handled(&text).with_context(|| {
        format!(
            "{} is damaged; remove it to start over (what is still in the window is then sent \
             again)",
            path.display()
        )
    })
}

/// `HANDLED_HEADER`, one line per candidate (`<at_ms> <digest>`, or
/// `<at_ms> <digest> failed <attempts> <source> <why>`), and `end <lines>`.
fn parse_handled(text: &str) -> Result<Handled> {
    let body = text
        .strip_suffix('\n')
        .context("it does not end with a whole line")?;
    let mut lines = body.split('\n');
    if lines.next() != Some(HANDLED_HEADER) {
        bail!("it does not begin with `{HANDLED_HEADER}`");
    }
    let mut handled = Handled::default();
    let (mut entries, mut end) = (0usize, None);
    for line in lines {
        if end.is_some() {
            bail!("a line follows its end");
        }
        if let Some(count) = line.strip_prefix("end ") {
            end = Some(
                count
                    .parse::<usize>()
                    .ok()
                    .context("its end line holds no count")?,
            );
            continue;
        }
        let (key, failing) =
            handled_line(line).with_context(|| format!("`{line}` is not an entry"))?;
        match failing {
            None => handled.answered.insert(key),
            Some(failing) => handled.failing.insert(key, failing).is_none(),
        };
        entries += 1;
    }
    match end {
        Some(count) if count == entries => Ok(handled),
        Some(count) => bail!("it ends saying {count} entries, and holds {entries}"),
        None => bail!("it has no end line: it was cut short"),
    }
}

fn handled_line(line: &str) -> Option<(Key, Option<Failing>)> {
    let mut fields = line.splitn(3, ' ');
    let at_ms = fields.next()?.parse().ok()?;
    let digest = fields.next().filter(|d| {
        d.len() == 16
            && d.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })?;
    let key = Key {
        at_ms,
        digest: u64::from_str_radix(digest, 16).ok()?,
    };
    let Some(rest) = fields.next() else {
        return Some((key, None));
    };
    let mut fields = rest.splitn(4, ' ');
    if fields.next()? != "failed" {
        return None;
    }
    let attempts = fields.next()?.parse().ok().filter(|n| *n >= 1)?;
    let source = fields.next().filter(|s| !s.is_empty())?.to_owned();
    let why = fields.next().unwrap_or("").to_owned();
    Some((
        key,
        Some(Failing {
            attempts,
            source,
            why,
        }),
    ))
}

/// Keep what has been handled: whole or not at all. Nothing handled leaves
/// no file.
pub fn write_handled(dirs: &Dirs, handled: &Handled) -> Result<()> {
    let path = handled_path(dirs);
    if handled.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).with_context(|| format!("cannot remove {}", path.display()))
            }
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&dirs.state)?;
    let mut text = format!("{HANDLED_HEADER}\n");
    for key in &handled.answered {
        let _ = writeln!(text, "{} {:016x}", key.at_ms, key.digest);
    }
    for (key, failing) in &handled.failing {
        let _ = writeln!(
            text,
            "{} {:016x} failed {} {} {}",
            key.at_ms, key.digest, failing.attempts, failing.source, failing.why
        );
    }
    let _ = writeln!(
        text,
        "end {}",
        handled.answered.len() + handled.failing.len()
    );
    replace_durably(&path, text.as_bytes())
}

/// Replace `path` whole: written aside and flushed to disk, renamed into
/// place, and its folder flushed after, so neither a kill nor a power loss
/// leaves part of it.
pub(crate) fn replace_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let partial = path.with_extension("partial");
    let replace = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&partial, path)?;
        if let Some(folder) = path.parent() {
            std::fs::File::open(folder)?.sync_all()?;
        }
        Ok(())
    };
    replace().with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One message as `vessa messages --json` writes it.
    fn vessa_message(agent: &str, kind: &str, at: &str, text: &str, reply: Value) -> Value {
        serde_json::json!({
            "agent": agent, "session": format!("{agent}-s"), "parent": null, "kind": kind,
            "id": null, "at": at, "cwd": "/p/work", "text": text, "question": null,
            "reply": reply, "source": "/t/s.jsonl", "line": 4,
        })
    }

    fn vessa_document(messages: Vec<Value>, gaps: &[&str], limitations: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": "vessa-messages.v1", "messages": messages, "sessions": [],
            "coverage": {"human": messages.len(), "excluded": {}, "gaps": gaps,
                         "limitations": limitations},
            "warnings": [],
        }))
        .unwrap()
    }

    fn reply(text: &str, failed: bool) -> Value {
        serde_json::json!({"text": text, "truncated": false, "failed": failed, "line": 3})
    }

    #[test]
    fn vessas_messages_become_candidates_keyed_as_intake_kept_them() {
        let since = parse_time("2026-09-10").unwrap();
        let document = vessa_document(
            vec![
                vessa_message(
                    "claude",
                    "message",
                    "2026-09-20T10:00:00Z",
                    "please dont merge PRs u open ok",
                    reply("I pushed to main.", false),
                ),
                vessa_message(
                    "codex",
                    "message",
                    "2026-09-20T10:02:00.123Z",
                    "we have lima baba",
                    Value::Null,
                ),
                vessa_message(
                    "pi",
                    "queued",
                    "2026-09-20T10:03:00+02:00",
                    "x",
                    reply("ran it", true),
                ),
                serde_json::json!({
                    "agent": "claude", "session": "claude-s/agent-1", "parent": "claude-s",
                    "kind": "answer", "id": "u9", "at": "2026-09-20T10:04:00Z", "cwd": "/p/work",
                    "text": "neither, use lima", "question": "Docker or a VM?",
                    "reply": reply("Which one?", false), "source": "/t/a.jsonl", "line": 9,
                }),
                vessa_message(
                    "opencode2",
                    "message",
                    "2026-09-20T10:05:00Z",
                    "y",
                    Value::Null,
                ),
                // At or before the window's start: vessa's window is exclusive.
                vessa_message(
                    "claude",
                    "message",
                    "2026-09-10T00:00:00Z",
                    "too old",
                    Value::Null,
                ),
            ],
            &[],
            &[],
        );
        let mut warnings = Vec::new();
        let said = listed(
            Some(0),
            &document,
            "",
            since,
            &mut warnings,
            &mut Vec::new(),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(said.len(), 5, "{said:?}");
        let first = &said[0].candidate;
        assert_eq!(first.source, "claude");
        assert_eq!(first.text, "please dont merge PRs u open ok");
        assert_eq!(first.context, "I pushed to main.");
        assert_eq!(first.place, "/p/work");
        assert_eq!(first.who, "person (claude session claude-s)");
        // The key #52's progress is kept by: the time, and a digest of the
        // words exactly as vessa gives them, which are the words intake's own
        // reader gave (reminders stripped, trimmed). Nothing handled before
        // is sent again.
        assert_eq!(
            first.key(),
            Key {
                at_ms: parse_time("2026-09-20T10:00:00Z").unwrap(),
                digest: fnv1a(b"please dont merge PRs u open ok"),
            }
        );
        assert_eq!(said[0].reply.as_deref(), Some("I pushed to main."));
        assert_eq!(said[1].candidate.source, "codex");
        assert_eq!(said[1].candidate.at_ms % 1000, 123, "milliseconds kept");
        assert_eq!(said[1].reply, None);
        assert_eq!(said[1].candidate.context, "");
        // A reply that failed is shown as such, and is no reply to the rules.
        assert_eq!(said[2].candidate.source, "pi");
        assert_eq!(said[2].candidate.at, "2026-09-20T08:03:00Z");
        assert_eq!(said[2].reply, None);
        assert_eq!(
            said[2].candidate.context,
            "(the agent's turn failed) ran it"
        );
        assert!(said[2]
            .candidate
            .who
            .contains("typed while the agent worked"));
        // An answer travels with its question.
        assert_eq!(said[3].kind, "answer");
        assert_eq!(said[3].candidate.context, "asked: Docker or a VM?");
        assert!(said[3]
            .candidate
            .who
            .contains("claude session claude-s/agent-1"));
        assert_eq!(
            said[4].candidate.source, "agent",
            "an agent intake does not know"
        );
        assert!(said.iter().all(|s| SAID.contains(&s.candidate.source)));
    }

    #[test]
    fn what_vessa_could_not_read_is_a_warning_and_an_old_vessa_is_told_to_update() {
        let since = parse_time("2026-09-10").unwrap();
        let one = || {
            vec![vessa_message(
                "claude",
                "message",
                "2026-09-20T10:00:00Z",
                "never python",
                Value::Null,
            )]
        };
        // Exit 4: a limitation (or a source warning) is a warning, which
        // holds the cursor; a gap, a store vessa knows of and does not read,
        // is handed back apart, to be said without holding anything. What
        // was listed is kept either way.
        let (mut warnings, mut gaps) = (Vec::new(), Vec::new());
        let partial = vessa_document(
            one(),
            &["opencode: /h/.local/share/opencode/opencode.db not read"],
            &["codex/f: Codex fork of p: parent transcript not read"],
        );
        let said = listed(
            Some(4),
            &partial,
            "vessa: partial message list (exit 4)",
            since,
            &mut warnings,
            &mut gaps,
        );
        assert_eq!(said.len(), 1);
        assert_eq!(
            warnings,
            ["vessa: codex/f: Codex fork of p: parent transcript not read"]
        );
        assert_eq!(
            gaps,
            ["vessa: opencode: /h/.local/share/opencode/opencode.db not read"]
        );
        // Exit 4 for a gap alone is no warning.
        let (mut warnings, mut gaps) = (Vec::new(), Vec::new());
        let unread = vessa_document(one(), &["opencode: /h/opencode.db not read"], &[]);
        listed(Some(4), &unread, "partial", since, &mut warnings, &mut gaps);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(gaps.len(), 1);
        // A source warning is a warning.
        let (mut warnings, mut gaps) = (Vec::new(), Vec::new());
        let mut unreadable: Value =
            serde_json::from_slice(&vessa_document(one(), &[], &[])).unwrap();
        unreadable["warnings"] = serde_json::json!(["/h/r.jsonl: Permission denied"]);
        let unreadable = serde_json::to_vec(&unreadable).unwrap();
        listed(
            Some(4),
            &unreadable,
            "partial",
            since,
            &mut warnings,
            &mut gaps,
        );
        assert_eq!(warnings, ["vessa: /h/r.jsonl: Permission denied"]);
        assert!(gaps.is_empty());
        // Exit 4 with nothing named is still partial.
        let (mut warnings, mut gaps) = (Vec::new(), Vec::new());
        listed(
            Some(4),
            &vessa_document(one(), &[], &[]),
            "partial",
            since,
            &mut warnings,
            &mut gaps,
        );
        assert_eq!(
            warnings,
            ["vessa says its list is partial (exit 4: partial)"]
        );
        // A document vessa ended with an error keeps what it listed, and warns.
        let mut warnings = Vec::new();
        let failed = serde_json::to_vec(&serde_json::json!({
            "schema": "vessa-messages.v1", "messages": one(), "error": "disk on fire",
        }))
        .unwrap();
        assert_eq!(
            listed(Some(1), &failed, "", since, &mut warnings, &mut Vec::new()).len(),
            1
        );
        assert_eq!(
            warnings,
            ["vessa failed after listing 1 message(s) (exit 1): disk on fire"]
        );
        // A vessa older than `messages`: update it; nothing is read instead.
        let mut warnings = Vec::new();
        let old = listed(
            Some(2),
            b"",
            "error: unrecognized subcommand 'messages'",
            since,
            &mut warnings,
            &mut Vec::new(),
        );
        assert!(old.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("update vessa"), "{warnings:?}");
        // Another form, or no JSON at all.
        let mut warnings = Vec::new();
        let other = serde_json::to_vec(&serde_json::json!({
            "schema": "vessa-messages.v2", "messages": one(),
        }))
        .unwrap();
        assert!(listed(Some(0), &other, "", since, &mut warnings, &mut Vec::new()).is_empty());
        assert!(warnings[0].contains("`vessa-messages.v2`"), "{warnings:?}");
        let mut warnings = Vec::new();
        assert!(listed(Some(1), b"", "boom", since, &mut warnings, &mut Vec::new()).is_empty());
        assert_eq!(warnings, ["vessa listed no messages (exit 1: boom)"]);
        let mut warnings = Vec::new();
        assert!(listed(
            Some(0),
            b"{\"schema\"",
            "",
            since,
            &mut warnings,
            &mut Vec::new()
        )
        .is_empty());
        assert!(warnings[0].starts_with("vessa's messages are not vessa-messages.v1"));
        // A message no time can place cannot be windowed: said, not dropped.
        let mut warnings = Vec::new();
        let undated = vessa_document(
            vec![vessa_message(
                "claude",
                "message",
                "yesterday",
                "x",
                Value::Null,
            )],
            &[],
            &[],
        );
        assert!(listed(Some(0), &undated, "", since, &mut warnings, &mut Vec::new()).is_empty());
        assert_eq!(
            warnings,
            ["vessa listed 1 message(s) whose time intake cannot read"]
        );
    }

    #[test]
    fn long_pasted_briefs_are_not_candidates() {
        assert!(clean(&"x".repeat(MAX_MESSAGE + 1)).is_none());
        assert_eq!(clean("  short  "), Some("short".to_owned()));
        assert_eq!(
            clean("a <system-reminder>b</system-reminder> c"),
            Some("a  c".to_owned())
        );
        // Words a person starts as a harness would are still a person's: the
        // prefix skip-list is gone, and vessa's exact forms replace it.
        assert_eq!(
            clean("Caveat: it is lima"),
            Some("Caveat: it is lima".to_owned())
        );
        let brief = "y".repeat(MAX_MESSAGE + 1);
        let document = vessa_document(
            vec![vessa_message(
                "codex",
                "task",
                "2026-09-20T10:00:00Z",
                &brief,
                Value::Null,
            )],
            &[],
            &[],
        );
        let said = listed(Some(0), &document, "", 0, &mut Vec::new(), &mut Vec::new());
        assert!(said[0].long, "kept apart, to be counted");
        let (candidates, sifted) = sift(said, false);
        assert!(candidates.is_empty(), "never a candidate, filter or not");
        assert_eq!((sifted.read, sifted.long), (1, 1));
    }

    #[test]
    fn seulas_labelled_steering_cases_separate_feedback_from_tasks_and_quotes() {
        // seula's labelled cases at e0a3016, unchanged: synthetic, not an
        // accuracy estimate for real archives.
        for (text, after_response, expected) in [
            ("I already told you to check permissions.", false, true),
            ("You forgot the migration check.", false, true),
            ("No, that’s not what the request meant.", true, true),
            ("This is not enough to review the result.", true, true),
            (
                "Something is missing from the proposed workflow.",
                true,
                true,
            ),
            (
                "Please try again with the original constraints.",
                true,
                true,
            ),
            (
                "But we need the result in the existing interface.",
                true,
                true,
            ),
            ("That doesn't address the failure mode.", true, true),
            ("Actually, don't use a remote service.", true, true),
            ("Let's do it better; retain the evidence.", true, true),
            ("We need more than a count.", true, true),
            ("Looks close. You missed the empty-input case.", true, true),
            ("Don't use a database for this new task.", false, false),
            ("Do not use network access.", false, false),
            (
                "Something is missing from a customer file; investigate it.",
                false,
                false,
            ),
            (
                "This is not enough memory for a new workload.",
                false,
                false,
            ),
            ("We need the next feature now.", true, false),
            ("Thanks, this is enough.", true, false),
            ("The README says you forgot the flag.", true, false),
            ("Explain the phrase \"I already told you\".", true, false),
            ("\"This is not enough\" is the test input.", true, false),
            ("`you forgot` is a fixture string.", true, false),
            ("> You missed a field in this quoted example.", true, false),
            ("```text\nYou forgot the flag.\n```", true, false),
            ("~~~text\nI told you to retry.\n~~~", true, false),
            ("You forgottheflag is an identifier.", true, false),
            ("Try againlater is a command name.", true, false),
        ] {
            let reply = after_response.then_some("Here is the result.");
            assert_eq!(
                correction_signal(text, reply).is_some(),
                expected,
                "{text:?}; after_response={after_response}"
            );
        }
    }

    #[test]
    fn seulas_lowercase_and_unpunctuated_steering_keeps_its_traps() {
        // seula's second table at e0a3016, unchanged: lowercase, no
        // apostrophes, no commas; paraphrased, none copied from a transcript.
        let statement = Some("I updated the parser and ran its tests.");
        let question = Some("I can also delete the old fixtures. Want me to do that?");
        for (text, reply, expected) in [
            ("no no no", statement, true),
            ("no no no - read the design notes first", statement, true),
            ("dont use docker, we can do this locally", statement, true),
            (
                "why do you push without running the tests?",
                statement,
                true,
            ),
            ("no kafka man", statement, true),
            ("no i mean the other binary", statement, true),
            ("No, keep the old interface.", statement, true),
            ("No. Keep the old interface.", statement, true),
            ("thats not what i asked", None, true),
            ("why are u editing that file", statement, true),
            ("this doesnt address the failure", statement, true),
            ("as i said, run it locally", None, true),
            ("i asked you to keep the old name", statement, true),
            // A "no" that answers the assistant's question is an answer, unless
            // the person says the assistant misunderstood.
            ("no", question, false),
            ("no but keep the tests", question, false),
            ("no no, leave them", question, false),
            ("no i mean the other fixtures", question, true),
            ("no no no", None, false),
            ("i asked you to keep the old name", None, false),
            // "it" may be another agent or tool the person is talking about.
            ("i asked it twice", statement, false),
            ("i asked that other agent to use rust", statement, false),
            ("no target dir", statement, false),
            ("No files changed.", statement, false),
            ("No new information.", statement, false),
            ("no need to back anything up", statement, false),
            ("No thanks, that is all.", statement, false),
            ("no-config builds are fine", statement, false),
            ("Why do you think the parser fails?", statement, false),
            ("I asked the reviewer for a second pass.", statement, false),
            ("I said hello to the team.", statement, false),
            ("Wrong tab, ignore that.", statement, false),
            ("Stop the staging box.", statement, false),
            ("dont forget the changelog", statement, false),
            ("Notes: no kafka needed here.", statement, false),
            ("The fixture says \"no no no\".", statement, false),
            ("no way dude", statement, false),
        ] {
            assert_eq!(
                correction_signal(text, reply).is_some(),
                expected,
                "{text:?}; reply={reply:?}"
            );
        }
    }

    #[test]
    fn the_prefilter_keeps_corrections_repeats_and_refusals_and_counts_what_it_drops() {
        let at = |minute: u32| format!("2026-09-20T10:{minute:02}:00Z");
        let after = reply("Here is the result.", false);
        let document = vessa_document(
            vec![
                vessa_message("claude", "message", &at(1), "no no no", after.clone()),
                // The same words after a failed turn answer the failure: a
                // retry, not a correction.
                vessa_message(
                    "claude",
                    "message",
                    &at(2),
                    "try again",
                    reply("API error", true),
                ),
                vessa_message("codex", "message", &at(3), "try again", after.clone()),
                vessa_message(
                    "codex",
                    "message",
                    &at(4),
                    "we have lima baba",
                    after.clone(),
                ),
                vessa_message(
                    "codex",
                    "message",
                    &at(5),
                    "Run the checks in the VM first",
                    Value::Null,
                ),
                vessa_message(
                    "pi",
                    "message",
                    &at(6),
                    "run the checks  in the VM first",
                    Value::Null,
                ),
                vessa_message(
                    "claude",
                    "rejection",
                    &at(7),
                    "use the existing helper",
                    Value::Null,
                ),
                vessa_message("claude", "message", &at(8), "next", after.clone()),
                vessa_message(
                    "claude",
                    "task",
                    &at(9),
                    &"z".repeat(MAX_MESSAGE + 1),
                    Value::Null,
                ),
            ],
            &[],
            &[],
        );
        let said = listed(Some(0), &document, "", 0, &mut Vec::new(), &mut Vec::new());
        let verdicts = prefilter(&said[..8]);
        assert_eq!(
            verdicts,
            [
                Some("correction"),
                None,
                Some("correction"),
                None,
                Some("repeat"),
                Some("repeat"),
                Some("rejection"),
                None,
            ]
        );
        let (kept, sifted) = sift(said.clone(), true);
        assert_eq!(kept.len(), 5);
        assert!(
            kept.iter().all(|c| c.text != "we have lima baba"),
            "low recall: dropped"
        );
        assert_eq!(
            sifted,
            Sifted {
                read: 9,
                long: 1,
                filtered: true,
                kept: BTreeMap::from([("correction", 2), ("repeat", 2), ("rejection", 1)]),
                dropped: BTreeMap::from([("claude", 2), ("codex", 1)]),
            }
        );
        assert_eq!(
            sifted.said(),
            "9 message(s) people wrote, listed by vessa; 1 longer than 1500 characters left \
             out as pasted briefs; the pre-filter (phrase-rules.v3) kept 5 (correction 2, \
             rejection 1, repeat 2) and dropped 3 (claude 2, codex 1), keeping none of them: \
             `--no-prefilter` sends every message, and `--since` reads a stretch again"
        );
        // Without it, every message but the brief is a candidate.
        let (all, sifted) = sift(said, false);
        assert_eq!(all.len(), 8);
        assert_eq!(
            sifted.said(),
            "9 message(s) people wrote, listed by vessa; 1 longer than 1500 characters left \
             out as pasted briefs; no pre-filter (--no-prefilter): 8 candidate(s)"
        );
        // Nothing dropped is said as such.
        let (_, sifted) = sift(Vec::new(), true);
        assert_eq!(
            sifted.said(),
            "0 message(s) people wrote, listed by vessa; the pre-filter (phrase-rules.v3) \
             kept 0 and dropped 0"
        );
    }

    /// Secret-shaped strings, put together at run time so that none sits in
    /// the source for a scanner to find. None is a real credential.
    fn fake(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn secret_looking_tokens_are_redacted_and_counted() {
        let body = "Ab3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3hJ5kL7";
        let cases = [
            ("an OpenAI key", fake(&["s", "k-proj-", &body[..32]])),
            (
                "an Anthropic key",
                fake(&["s", "k-ant-api03-", &body[..36]]),
            ),
            ("a GitHub token", fake(&["gh", "p_", &body[..36]])),
            ("a GitHub OAuth token", fake(&["gh", "o_", &body[..36]])),
            (
                "a GitHub fine-grained token",
                fake(&["github", "_pat_11", &body[..30]]),
            ),
            (
                "a Slack token",
                fake(&["xo", "xb-", "1234567890-", &body[..24]]),
            ),
            (
                "a Slack user token",
                fake(&["xo", "xp-", "1234567890-", &body[..24]]),
            ),
            ("an AWS key id", fake(&["AK", "IA", "Q3SWZ7YT2NB5XK4M"])),
            (
                "an AWS session key id",
                fake(&["AS", "IA", "Q3SWZ7YT2NB5XK4M"]),
            ),
            ("a GitLab token", fake(&["gl", "pat-", &body[..20]])),
            ("a Google API key", fake(&["AI", "za", "Sy", &body[..33]])),
            (
                "a JWT",
                fake(&[
                    "ey",
                    "JhbGciOiJIUzI1NiJ9",
                    ".",
                    "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                    ".",
                    &body[..28],
                ]),
            ),
        ];
        for (what, secret) in &cases {
            for text in [
                secret.clone(),
                format!("use {secret} for it."),
                format!("({secret}), then retry"),
            ] {
                let (scrubbed, n) = scrub(&text);
                assert_eq!(n, 1, "{what}: {scrubbed}");
                assert!(!scrubbed.contains(secret.as_str()), "{what}: {scrubbed}");
                assert!(scrubbed.contains("[redacted]"), "{what}: {scrubbed}");
            }
        }
        // A private key block, whole, with or without its end.
        let pem = fake(&[
            "-----BEGIN RSA PRIV",
            "ATE KEY-----\n",
            body,
            "\n",
            body,
            "\n-----END RSA PRIV",
            "ATE KEY-----",
        ]);
        let (scrubbed, n) = scrub(&format!("here:\n{pem}\nthanks"));
        assert_eq!((scrubbed.as_str(), n), ("here:\n[redacted]\nthanks", 1));
        let open = fake(&["-----BEGIN OPENSSH PRIV", "ATE KEY-----\n", body]);
        assert_eq!(scrub(&format!("x {open}")), ("x [redacted]".to_owned(), 1));
        // A certificate is no secret.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(scrub(cert), (cert.to_owned(), 0));
        // A long, mixed token right after a word that names a secret.
        let value = &body[..24];
        for text in [
            format!("api_key={value}"),
            format!("API_KEY: {value}"),
            format!("apiKey = '{value}'"),
            format!("\"access_token\": \"{value}\""),
            format!("client_secret {value}"),
            format!("password: {value}"),
            format!("Authorization: Bearer {value}"),
            format!("export GITHUB_TOKEN={value}=="),
        ] {
            let (scrubbed, n) = scrub(&text);
            assert_eq!(n, 1, "{text}: {scrubbed}");
            assert!(!scrubbed.contains(value), "{scrubbed}");
        }
        // A key of some vendor ohjaa knows nothing of, with no prefix and no
        // name before it: a long token mixing capitals, lower case and
        // digits, or an id and a secret joined by a dot (`<hex>.<alnum>`, as
        // some providers issue them; synthetic, like every key here).
        let unnamed = [
            fake(&["be3f", "Q9xT2mLk7RvA4nWp8ZcH1sYd6JgU3eBo5KfN0iMq2"]),
            fake(&[
                "3f9a1c0b7e2d4f6a",
                "8b1c3d5e7f9a0b2c",
                ".",
                "Xk9Qm2Lp7RtA4nWz",
            ]),
            fake(&["3f9a1c0b7e2d4f6a8b1c3d5e", ".", "k9qm2lp7rtaz"]),
            fake(&["Ab3dE5gH7jK9mN1pQ3sT", "/", "5vW7yZ9bC1dF3hJ5kL7mN9p"]),
        ];
        for text in unnamed.iter().flat_map(|key| {
            [
                key.clone(),
                format!("use this one: {key}."),
                format!("({key})"),
            ]
        }) {
            let unnamed = unnamed
                .iter()
                .find(|key| text.contains(key.as_str()))
                .unwrap();
            let (scrubbed, n) = scrub(&text);
            assert_eq!(n, 1, "{scrubbed}");
            assert!(!scrubbed.contains(unnamed.as_str()), "{scrubbed}");
        }
        // Two in one message are two.
        assert_eq!(scrub(&format!("{} and {}", cases[2].1, cases[7].1)).1, 2);
    }

    #[test]
    fn what_only_looks_long_is_not_redacted() {
        for text in [
            // A git sha, a UUID, alone or after ordinary words.
            "commit 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b is on main",
            "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
            "session 8c1f2e3a-4b5c-4d6e-8f70-123456789abc ended",
            "8c1f2e3a-4b5c-4d6e-8f70-123456789abc",
            // Words that name a secret, followed by no secret.
            "the key thing is to keep the token budget small",
            "password: hunter2",
            "the token count was 1234",
            "key /Users/yair/.ssh/id_ed25519_github_2026",
            // Prefixes inside ordinary words, or too short to be keys.
            "use sk-learn, or task-1234567890abcdefghij",
            "the AKIA prefix names an AWS key",
            "eyJ is how base64 JSON starts",
            "monkey Ab3dE5gH7jK9mN1pQ3sT",
            // Hashes and ids in either case, and long names and paths with no
            // capital, digit or long segment that mixes them.
            "1A2B3C4D5E6F7A8B9C0D1E2F3A4B5C6D7E8F9A0B and 8C1F2E3A-4B5C-4D6E-8F70-123456789ABC",
            "sha256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "image sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "a 9f86d081884c7d659a2f.feaa0c55ad01 short id",
            "test_the_cursor_moves_only_past_what_was_handled_and_never_back_2",
            "/Users/yair/projects/_worktrees/ohjaa-intake-vessa/src/IntakeVessa2/main.rs",
            "~/.codex/sessions/2026/10/26/rollout-2026-10-26T10-00-00-8c1f2e3a.jsonl",
            "ProjectManagementDashboardSettingsController is long but has no digit",
            // A path is judged by the pieces of its names: versioned and dated
            // file names are made of short words.
            "/Users/yair/Downloads/Ohjaa-1.2.3-macOS-arm64-applesilicon.zip",
            "~/Desktop/2026-10-26T10-20-33Z_Screen-Recording-8c1f2e3a-Final.mov",
        ] {
            assert_eq!(scrub(text), (text.to_owned(), 0), "{text}");
        }
    }

    #[test]
    fn the_prompt_carries_no_secret_from_any_candidate() {
        let key = fake(&["gh", "p_", "Ab3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3hJ5"]);
        let candidate = |source: &'static str, text: String, context: String| Candidate {
            source,
            at_ms: 0,
            at: rfc3339(0),
            place: "/p/work".to_owned(),
            who: "person".to_owned(),
            context,
            text,
            session: None,
        };
        let batch = [
            candidate(
                "claude",
                format!("use {key} please"),
                format!("I need {key}"),
            ),
            candidate("review", format!("token={key}"), String::new()),
            candidate("handover", "next: nothing secret".to_owned(), String::new()),
        ];
        let (prompt, redacted) = prompt(&Known::default(), &batch);
        assert!(!prompt.contains(&key), "{prompt}");
        assert_eq!(redacted, 3);
        assert!(prompt.contains("said: use [redacted] please"), "{prompt}");
        assert!(prompt.contains("before: I need [redacted]"), "{prompt}");
        assert!(prompt.contains("said: next: nothing secret"), "{prompt}");
    }

    fn lesson(id: &str, status: Status, body: &str) -> Node {
        Node {
            id: id.to_owned(),
            kind: Kind::Lesson,
            status,
            as_of: parse_date("2026-10-26").unwrap(),
            origin: vec!["a test".to_owned()],
            supersedes: Vec::new(),
            refines: None,
            holds_while: Vec::new(),
            body: body.to_owned(),
        }
    }

    /// One of `nodes` each: a proposed one waits among the proposals, any
    /// other is in a scope directory, whose repo nodes are `/r/here`'s.
    fn known(nodes: &[(Scope, Option<&str>, &str, Status)]) -> Known {
        let (waiting, current): (Vec<_>, Vec<_>) = nodes
            .iter()
            .partition(|(_, _, _, status)| *status == Status::Proposed);
        let loaded = Loaded {
            nodes: current
                .iter()
                .map(|(scope, _, id, status)| faija_knowledge::Located {
                    scope: *scope,
                    node: lesson(id, *status, "A lesson in a scope."),
                })
                .collect(),
            ..Loaded::default()
        };
        let waiting: Vec<knowledge::Pending> = waiting
            .iter()
            .map(|(scope, repo, id, _)| knowledge::Pending {
                scope: *scope,
                repo: repo.map(PathBuf::from),
                path: PathBuf::from(format!("/proposals/{}/{id}.md", scope.name())),
                node: lesson(id, Status::Proposed, "A lesson waiting."),
                staged: true,
            })
            .collect();
        Known::of(&loaded, &waiting, Some(Path::new("/r/here")))
    }

    fn proposing(
        scope: Scope,
        repo: Option<&str>,
        id: &str,
        refines: Option<&str>,
        supersedes: &[&str],
    ) -> Proposal {
        Proposal {
            scope,
            repo: repo.map(PathBuf::from),
            id: id.to_owned(),
            kind: Kind::Lesson,
            body: "b".to_owned(),
            origin: vec!["o".to_owned()],
            supersedes: supersedes.iter().map(|id| (*id).to_owned()).collect(),
            refines: refines.map(str::to_owned),
            holds_while: Vec::new(),
        }
    }

    #[test]
    fn a_title_is_its_words_not_their_spelling() {
        let words = |id: &str| title_words(id).into_iter().collect::<Vec<_>>();
        assert_eq!(
            words("start-a-session-through-ohjaa-run"),
            words("start-sessions-through-ohjaa-run")
        );
        assert_eq!(
            words("prove-with-ohjaa-gate-before-merging"),
            ["befor", "gat", "merg", "ohjaa", "prov"]
        );
        assert_eq!(words("merge"), words("merged"));
        // `-ss` is no plural, and a short word keeps its ending.
        assert_eq!(
            words("the-process-passes-its-uses"),
            ["pass", "process", "use"]
        );
        assert_eq!(words("Rust_or.ELIXIR"), ["elixir", "rust"]);
    }

    #[test]
    fn titles_are_near_when_they_say_mostly_the_same_words() {
        let near = |a: &str, b: &str| near(&title_words(a), &title_words(b));
        // Duplicates intake's first real run wrote, 2026-10-27, and one of a
        // node already current.
        for (a, b) in [
            (
                "read-back-briefing-and-leave-handover",
                "read-briefing-and-leave-handover",
            ),
            (
                "read-back-briefing-and-leave-handover",
                "ohjaa-read-back-briefing-and-leave-handover",
            ),
            (
                "ohjaa-read-back-briefing-and-leave-handover",
                "read-back-briefing-with-ohjaa-oriented",
            ),
            (
                "start-agents-through-ohjaa-run",
                "start-sessions-through-ohjaa-run",
            ),
            ("code-is-rust-or-elixir", "rust-elixir-only"),
        ] {
            assert!(near(a, b).is_some(), "{a} | {b}");
            assert_eq!(near(a, b), near(b, a));
        }
        // The nearest pairs of its 131 ids that are not one lesson.
        for (a, b) in [
            (
                "ohjaa-gate-refuses-until-adopt-after-merge",
                "prove-with-ohjaa-gate-before-merging",
            ),
            (
                "gate-proof-lives-in-its-worktree",
                "scratch-trees-hold-live-worktrees",
            ),
            (
                "linux-dev-git-is-too-old-for-reftable",
                "linux-dev-has-nftables",
            ),
            // Half the words, but one word in common is never enough.
            ("lean-first", "local-first"),
        ] {
            assert_eq!(near(a, b), None, "{a} | {b}");
        }
        // One lesson in other words is the drafter's to see, shown both.
        assert_eq!(
            near(
                "gate-before-pushing-main",
                "prove-with-ohjaa-gate-before-merging"
            ),
            None
        );
        // Nearer is more.
        assert!(
            near(
                "read-back-briefing-and-leave-handover",
                "ohjaa-read-back-briefing-and-leave-handover"
            ) > near(
                "read-back-briefing-and-leave-handover",
                "read-back-briefing-with-ohjaa-oriented"
            )
        );
    }

    #[test]
    fn a_draft_may_repeat_only_what_is_present_in_its_own_scope_and_repository() {
        use Scope::{Mine, Ours, Repo};
        let present = known(&[
            (
                Ours,
                None,
                "start-sessions-through-ohjaa-run",
                Status::Proposed,
            ),
            (Mine, None, "read-back-briefing", Status::Current),
            (Mine, None, "leave-a-handover-always", Status::Retired),
            (Repo, Some("/r/rauha"), "rauha-linux-only", Status::Proposed),
        ]);
        let repeats = |proposal: Proposal| present.repeated_by(&proposal).map(str::to_owned);
        let of = |id: &str| Some(id.to_owned());
        assert_eq!(
            repeats(proposing(
                Ours,
                None,
                "start-agents-through-ohjaa-run",
                None,
                &[]
            )),
            of("start-sessions-through-ohjaa-run"),
            "near a waiting proposal"
        );
        assert_eq!(
            repeats(proposing(Mine, None, "read-the-briefing-back", None, &[])),
            of("read-back-briefing"),
            "near current knowledge"
        );
        assert_eq!(
            repeats(proposing(
                Repo,
                Some("/r/rauha"),
                "rauha-is-linux-only",
                None,
                &[]
            )),
            of("rauha-linux-only")
        );
        // The same words in another scope, or of another repository, are
        // another lesson (the review of #61: `tapio-linux-only`).
        assert_eq!(
            repeats(proposing(
                Mine,
                None,
                "start-agents-through-ohjaa-run",
                None,
                &[]
            )),
            None
        );
        assert_eq!(
            repeats(proposing(
                Repo,
                Some("/r/tapio"),
                "tapio-linux-only",
                None,
                &[]
            )),
            None
        );
        // What a draft refines, current or waiting, or supersedes, it says
        // again by design.
        for refinement in [
            proposing(
                Ours,
                None,
                "start-pi-sessions-through-ohjaa-run",
                Some("start-sessions-through-ohjaa-run"),
                &[],
            ),
            proposing(
                Mine,
                None,
                "read-back-briefing-in-codex",
                Some("read-back-briefing"),
                &[],
            ),
            proposing(
                Mine,
                None,
                "read-back-briefing-first",
                None,
                &["read-back-briefing"],
            ),
        ] {
            assert_eq!(repeats(refinement.clone()), None, "{}", refinement.id);
        }
        assert_eq!(
            repeats(proposing(Mine, None, "always-leave-a-handover", None, &[])),
            None,
            "a retired node is not present"
        );

        // Of several, the nearest; and what is written is present after.
        let mut two = known(&[
            (
                Ours,
                None,
                "read-briefing-and-leave-handover",
                Status::Proposed,
            ),
            (
                Ours,
                None,
                "read-back-briefing-and-leave-handover",
                Status::Proposed,
            ),
        ]);
        let draft = proposing(
            Ours,
            None,
            "ohjaa-read-back-briefing-and-leave-handover",
            None,
            &[],
        );
        assert_eq!(
            two.repeated_by(&draft),
            Some("read-back-briefing-and-leave-handover")
        );
        let lima = proposing(Ours, None, "lima-for-linux", None, &[]);
        assert_eq!(two.repeated_by(&lima), None);
        let written = two.present(&lima);
        two.add(written);
        assert_eq!(
            two.repeated_by(&proposing(Ours, None, "linux-for-lima", None, &[])),
            Some("lima-for-linux")
        );
        // As near as each other: current knowledge is named first.
        let tie = proposing(Mine, None, "code-in-rust-or-elixir-only", None, &[]);
        assert_eq!(
            near(&title_words(&tie.id), &title_words("rust-elixir-only")),
            near(
                &title_words(&tie.id),
                &title_words("code-is-rust-or-elixir")
            )
        );
        assert_eq!(
            known(&[
                (Mine, None, "rust-elixir-only", Status::Current),
                (Mine, None, "code-is-rust-or-elixir", Status::Proposed),
            ])
            .repeated_by(&tie),
            Some("rust-elixir-only")
        );
    }

    #[test]
    fn a_repo_proposal_naming_no_repository_is_compared_in_the_one_it_is_made_in() {
        // The re-review of #61: `propose` files it under the repository it
        // runs in, and the comparison looked in none.
        let present = known(&[(
            Scope::Repo,
            Some("/r/here"),
            "rauha-linux-only",
            Status::Proposed,
        )]);
        let unnamed = proposing(Scope::Repo, None, "rauha-is-linux-only", None, &[]);
        assert_eq!(present.repeated_by(&unnamed), Some("rauha-linux-only"));
        let mut empty = Known::of(&Loaded::default(), &[], Some(Path::new("/r/here")));
        let written = empty.present(&unnamed);
        empty.add(written);
        assert_eq!(
            empty.repeated_by(&proposing(
                Scope::Repo,
                Some("/r/here"),
                "rauha-runs-linux-only",
                None,
                &[]
            )),
            Some("rauha-is-linux-only")
        );
    }

    #[test]
    fn a_waiting_proposal_is_shown_as_a_possible_duplicate_of_what_came_before_it() {
        // The re-review of #61: the mark is derived when a person reviews,
        // and stored never.
        use Scope::{Mine, Ours, Repo};
        let loaded = Loaded {
            nodes: [
                ("rust-elixir-only", Status::Current),
                ("leave-a-handover-always", Status::Retired),
            ]
            .into_iter()
            .map(|(id, status)| faija_knowledge::Located {
                scope: Mine,
                node: lesson(id, status, "In a scope."),
            })
            .collect(),
            ..Loaded::default()
        };
        let waiting = |scope: Scope, repo: Option<&str>, id: &str, day: &str| knowledge::Pending {
            scope,
            repo: repo.map(PathBuf::from),
            path: PathBuf::from(format!("/proposals/{id}.md")),
            node: Node {
                as_of: parse_date(day).unwrap(),
                ..lesson(id, Status::Proposed, "Waiting.")
            },
            staged: true,
        };
        let all = vec![
            waiting(
                Ours,
                None,
                "read-back-briefing-and-leave-handover",
                "2026-10-27",
            ),
            waiting(Ours, None, "read-briefing-and-leave-handover", "2026-10-26"),
            waiting(Mine, None, "code-is-rust-or-elixir", "2026-10-27"),
            waiting(Mine, None, "always-leave-a-handover", "2026-10-27"),
            waiting(Repo, Some("/r/tapio"), "tapio-linux-only", "2026-10-27"),
            waiting(Repo, Some("/r/rauha"), "rauha-linux-only", "2026-10-26"),
            waiting(Repo, Some("/r/rauha"), "rauha-is-linux-only", "2026-10-27"),
            // As old as each other: the one stored first came before.
            waiting(Ours, None, "start-sessions-through-ohjaa-run", "2026-10-27"),
            waiting(Ours, None, "start-agents-through-ohjaa-run", "2026-10-27"),
        ];
        let marks = possible_duplicates(&loaded, &all, None);
        let of = |id: &str| Some(id.to_owned());
        assert_eq!(
            marks,
            [
                // Of the older one; never the other way round.
                of("read-briefing-and-leave-handover"),
                None,
                of("rust-elixir-only"),
                // A retired node is not present.
                None,
                // Another repository's words are another lesson.
                None,
                None,
                of("rauha-linux-only"),
                None,
                of("start-sessions-through-ohjaa-run"),
            ]
        );
        let order: Vec<String> = review_order(all.into_iter().zip(marks).collect())
            .into_iter()
            .map(|(pending, _)| pending.node.id)
            .collect();
        assert_eq!(
            order,
            [
                "read-briefing-and-leave-handover",
                "read-back-briefing-and-leave-handover",
                "code-is-rust-or-elixir",
                "always-leave-a-handover",
                "tapio-linux-only",
                "rauha-linux-only",
                "rauha-is-linux-only",
                "start-sessions-through-ohjaa-run",
                "start-agents-through-ohjaa-run",
            ]
        );
    }

    #[test]
    fn a_proposed_node_in_a_scope_directory_is_listed_once() {
        let node = lesson("x-y-z", Status::Proposed, "Committed for review.");
        let loaded = Loaded {
            nodes: vec![faija_knowledge::Located {
                scope: Scope::Ours,
                node: node.clone(),
            }],
            ..Loaded::default()
        };
        let waiting = [knowledge::Pending {
            scope: Scope::Ours,
            repo: None,
            path: PathBuf::from("/ours/knowledge/x-y-z.md"),
            node,
            staged: false,
        }];
        assert_eq!(Known::of(&loaded, &waiting, None).nodes.len(), 1);
    }

    #[test]
    fn the_drafter_is_shown_what_is_present_within_a_budget() {
        let (text, _) = prompt(
            &known(&[
                (Scope::Mine, None, "rust-elixir-only", Status::Current),
                (
                    Scope::Ours,
                    None,
                    "start-sessions-through-ohjaa-run",
                    Status::Proposed,
                ),
            ]),
            &[],
        );
        let current = text
            .find("- rust-elixir-only [mine/lesson/current]: A lesson in a scope.")
            .expect(&text);
        let heading = text.find("\nProposals waiting for a person").expect(&text);
        let waiting = text
            .find("- start-sessions-through-ohjaa-run [ours/lesson/proposed]: A lesson waiting.")
            .expect(&text);
        assert!(current < heading && heading < waiting, "{text}");

        // However many wait, the list stays within its budget, each line
        // cut, and what is left out is counted.
        let body = "word ".repeat(100);
        let waiting: Vec<knowledge::Pending> = (0..1_000)
            .map(|n| knowledge::Pending {
                scope: Scope::Ours,
                repo: None,
                path: PathBuf::new(),
                node: lesson(&format!("lesson-{n}"), Status::Proposed, &body),
                staged: true,
            })
            .collect();
        let (text, _) = prompt(&Known::of(&Loaded::default(), &waiting, None), &[]);
        let list = &text[INSTRUCTIONS.len()..text.find("\nCandidates:").unwrap()];
        assert!(list.len() < KNOWN_BUDGET + 300, "{}", list.len());
        assert!(
            list.contains("- lesson-0 [ours/lesson/proposed]: word"),
            "{list}"
        );
        let shown = list.matches("\n- lesson-").count();
        assert!(
            list.contains(&format!("- … and {} more, not shown", 1_000 - shown)),
            "{list}"
        );
        assert!(!list.contains(&"word ".repeat(21)), "a summary is cut");
    }

    #[test]
    fn the_drafter_is_shown_only_the_repositories_its_batch_comes_from() {
        // The review of #61: every batch carried the waiting proposals of
        // all eleven repositories.
        let present = known(&[
            (Scope::Ours, None, "ours-lesson", Status::Proposed),
            (Scope::Mine, None, "mine-lesson", Status::Current),
            (Scope::Machine, None, "machine-fact", Status::Proposed),
            (
                Scope::Repo,
                Some("/r/projects/valvo"),
                "valvo-lesson",
                Status::Proposed,
            ),
            (
                Scope::Repo,
                Some("/r/projects/rauha"),
                "rauha-lesson",
                Status::Proposed,
            ),
            (
                Scope::Repo,
                Some("/r/projects/ohjaa"),
                "ohjaa-lesson",
                Status::Proposed,
            ),
        ]);
        let candidate = |source: &'static str, place: &str, context: &str| Candidate {
            source,
            at_ms: 0,
            at: rfc3339(0),
            place: place.to_owned(),
            who: "person".to_owned(),
            context: context.to_owned(),
            text: "no".to_owned(),
            session: None,
        };
        let shown = |batch: &[Candidate]| -> Vec<&'static str> {
            let (text, _) = prompt(&present, batch);
            [
                "ours-lesson",
                "mine-lesson",
                "machine-fact",
                "valvo-lesson",
                "rauha-lesson",
                "ohjaa-lesson",
            ]
            .into_iter()
            .filter(|id| text.contains(&format!("- {id} [")))
            .collect()
        };
        let everywhere = ["ours-lesson", "mine-lesson", "machine-fact"];
        assert_eq!(shown(&[]), everywhere);
        let with = |id: &'static str| {
            let mut ids = everywhere.to_vec();
            ids.push(id);
            ids
        };
        assert_eq!(
            shown(&[candidate("claude", "/r/projects/valvo/formal", "")]),
            with("valvo-lesson")
        );
        // A worktree named after its repository, and a pull request's URL.
        assert_eq!(
            shown(&[candidate(
                "codex",
                "/r/projects/_worktrees/ohjaa-intake-o8",
                ""
            )]),
            with("ohjaa-lesson")
        );
        assert_eq!(
            shown(&[candidate("review", "https://github.com/o/rauha/pull/3", "")]),
            with("rauha-lesson")
        );
        // A review, by the repository it was read from.
        assert_eq!(
            shown(&[candidate(
                "review",
                "https://github.com/o/x/pull/1",
                "/r/projects/rauha"
            )]),
            with("rauha-lesson")
        );
        // The agent's words before a message are no place.
        assert_eq!(
            shown(&[candidate("claude", "/r/projects", "see /r/projects/valvo/")]),
            everywhere
        );
        let (text, _) = prompt(&present, &[candidate("claude", "/r/projects/valvo", "")]);
        assert!(
            text.contains("- valvo-lesson [repo/lesson/proposed] in /r/projects/valvo: A lesson"),
            "a repo node is shown with its repository: {text}"
        );
    }

    #[test]
    fn the_knowledge_shown_is_scrubbed_whole_before_it_is_cut_and_counted() {
        // The review of #61: a token in a waiting proposal's first line
        // reached the drafter verbatim, and the count stayed 0.
        let key = fake(&["gh", "p_", "Ab3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3hJ5"]);
        let loaded = Loaded {
            nodes: vec![faija_knowledge::Located {
                scope: Scope::Mine,
                node: lesson(
                    "use-the-api",
                    Status::Current,
                    &format!("Call it with {key}."),
                ),
            }],
            ..Loaded::default()
        };
        // Across the cut at 100 characters: cut first, a piece too short to
        // look secret would go out.
        let straddling = format!("{} {key} for the API.", "x".repeat(SUMMARY_CHARS - 10));
        let waiting = [knowledge::Pending {
            scope: Scope::Ours,
            repo: None,
            path: PathBuf::from("/proposals/ours/api-token.md"),
            node: lesson("api-token", Status::Proposed, &straddling),
            staged: true,
        }];
        let (text, redacted) = prompt(&Known::of(&loaded, &waiting, None), &[]);
        assert!(!text.contains(&key[..8]), "{text}");
        assert_eq!(redacted, 2, "{text}");
        assert!(
            text.contains("- use-the-api [mine/lesson/current]: Call it with [redacted]."),
            "{text}"
        );
        assert!(
            text.contains("- api-token [ours/lesson/proposed]: xxx"),
            "{text}"
        );
    }

    #[test]
    fn git_is_asked_in_the_c_locale_whatever_language_is_set() {
        use std::ffi::OsStr;
        let command = git_command(Path::new("/r"), &["log"]);
        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        assert!(envs.contains(&(OsStr::new("LC_ALL"), Some(OsStr::new("C")))));
        // Homebrew's git, with macOS libintl, honours LANGUAGE over LC_ALL=C.
        assert!(envs.contains(&(OsStr::new("LANGUAGE"), None)), "{envs:?}");
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
        // The answer as it was before the drafter was asked what each
        // candidate re-teaches: an array of drafts.
        let answer = "Here you go:\n```json\n[{\"scope\":\"ours\",\"id\":\"a\",\"kind\":\"taste\",\"body\":\"B\",\"origin\":[\"o\"]}]\n```";
        let answered = parse_answer(answer).unwrap();
        assert_eq!(answered.drafts.len(), 1);
        assert_eq!(answered.drafts[0].refines, None);
        assert!(answered.reteaches.is_empty());
        assert!(parse_answer("[]").unwrap().drafts.is_empty());
        assert!(parse_answer("no array").is_err());
        assert!(parse_answer("] [").is_err());
    }

    #[test]
    fn the_drafter_answers_what_each_candidate_reteaches_beside_its_drafts() {
        let draft = r#"{"scope":"ours","id":"a","kind":"taste","body":"B","origin":["o"]}"#;
        let answer = format!(
            "Sure.\n```json\n{{\"drafts\": [{draft}], \"reteaches\": [{{\"candidate\": 1, \
             \"ids\": [\"rust-elixir-only\"]}}, {{\"candidate\": 3, \"ids\": [\"x\", \"y\"]}}]}}\n```"
        );
        let answered = parse_answer(&answer).unwrap();
        assert_eq!(answered.drafts.len(), 1);
        assert_eq!(answered.reteaches.len(), 2);
        assert_eq!(answered.reteaches[1]["ids"][1], "y");
        // A message that only re-teaches: no draft.
        let only = parse_answer(r#"{"reteaches":[{"candidate":1,"ids":["a"]}]}"#).unwrap();
        assert!(only.drafts.is_empty() && only.reteaches.len() == 1);
        let none = parse_answer(r#"{"drafts": [], "reteaches": []}"#).unwrap();
        assert!(none.drafts.is_empty() && none.reteaches.is_empty());
        // Brackets in the prose before it are no answer.
        let noted =
            parse_answer(r#"Notes [1]: {"drafts":[],"reteaches":[{"candidate":1,"ids":["a"]}]}"#)
                .unwrap();
        assert_eq!(noted.reteaches.len(), 1);
        // One draft alone, not inside an answer, is no answer; nor are
        // drafts that are not drafts: either is refused, and the batch split.
        assert!(parse_answer(draft).is_err());
        assert!(parse_answer(r#"{"drafts": [{"id": "no-kind"}], "reteaches": []}"#).is_err());
        // Each re-teaching is read on its own, later: one written wrong
        // costs only itself.
        let odd =
            parse_answer(r#"{"drafts":[],"reteaches":["nonsense",{"candidate":"two"}]}"#).unwrap();
        assert_eq!(odd.reteaches.len(), 2);
    }

    #[test]
    fn a_reteaching_is_kept_only_of_a_current_node_shown_with_a_persons_words() {
        let present = known(&[
            (Scope::Mine, None, "rust-elixir-only", Status::Current),
            (Scope::Mine, None, "waiting-lesson", Status::Proposed),
            (Scope::Mine, None, "old-way", Status::Retired),
            (Scope::Repo, None, "here-lesson", Status::Current),
            // Defined in two scopes: which one is meant cannot be told.
            (Scope::Ours, None, "twice", Status::Current),
            (Scope::Mine, None, "twice", Status::Current),
        ]);
        let candidate =
            |source: &'static str, at_ms: i64, place: &str, session: Option<&str>| Candidate {
                source,
                at_ms,
                at: rfc3339(at_ms.div_euclid(1000)),
                place: place.to_owned(),
                who: "person".to_owned(),
                context: String::new(),
                text: format!("said at {at_ms}"),
                session: session.map(str::to_owned),
            };
        let batch = [
            candidate("claude", 1_000, "/r/here/src", Some("s1")),
            candidate("review", 2_000, "https://github.com/o/here/pull/1", None),
            candidate("observed", 3_000, "/r/here", Some("ohjaa-1")),
            candidate("handover", 4_000, "/r/here", Some("ohjaa-1")),
            candidate("codex", 5_000, "/r/elsewhere", Some("s5")),
        ];
        let reteaches: Vec<Value> = serde_json::from_str(
            r#"[
              {"candidate": 1, "ids": ["rust-elixir-only", "rust-elixir-only", "waiting-lesson",
                                       "old-way", "here-lesson", "twice", 7]},
              {"candidate": "2", "ids": ["rust-elixir-only", "here-lesson"]},
              {"candidate": 3, "ids": ["rust-elixir-only"]},
              {"candidate": 4, "ids": ["rust-elixir-only"]},
              {"candidate": 5, "ids": ["here-lesson", "rust-elixir-only"]},
              {"candidate": 0, "ids": ["rust-elixir-only"]},
              {"candidate": 6, "ids": ["a", "b"]},
              {"ids": ["rust-elixir-only"]},
              {"candidate": 1},
              "nonsense"
            ]"#,
        )
        .unwrap();
        let (_, _, present) = prompt_with_evidence(&present, &batch);
        let (kept, dropped) = present.retaught(&reteaches, &batch);
        let mine = crate::evidence::NodeKey::new(Scope::Mine, None, "rust-elixir-only");
        let here =
            crate::evidence::NodeKey::new(Scope::Repo, Some(Path::new("/r/here")), "here-lesson");
        assert_eq!(here.clone.as_deref(), Some(Path::new("/r/here")));
        let got: Vec<(&crate::evidence::NodeKey, i64, Option<&str>)> = kept
            .iter()
            .map(|r| (&r.node, r.key.at_ms, r.session.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (&mine, 1_000, Some("s1")),
                (&here, 1_000, Some("s1")),
                (&mine, 2_000, None),
                (&here, 2_000, None),
                // A repo node is re-taught only from its own repository.
                (&mine, 5_000, Some("s5")),
            ]
        );
        assert_eq!(kept[0].key, batch[0].key(), "keyed as intake keys it");
        assert_eq!(
            (kept[0].source, kept[0].at.as_str()),
            ("claude", "1970-01-01T00:00:01Z")
        );
        assert_eq!(
            dropped,
            Dropped {
                not_current: 4,
                not_said: 2,
                no_candidate: 3,
                unreadable: 4,
            }
        );
        assert_eq!(
            dropped.describe(),
            "4 naming no current node shown with its candidate, 2 of a candidate that is not a \
             person's words, 3 of no candidate in its batch, 4 unreadable"
        );

        // A draft cites the current nodes it refines or supersedes that the
        // drafter was shown with its batch, each once.
        let named: Vec<String> = [
            "rust-elixir-only",
            "waiting-lesson",
            "rust-elixir-only",
            "here-lesson",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            present.cited(&named, &batch[4..]),
            std::slice::from_ref(&mine)
        );
        assert_eq!(present.cited(&named, &batch[..1]), {
            let mut both = vec![mine, here];
            both.sort();
            both
        });
    }

    #[test]
    fn evidence_cannot_name_a_node_omitted_by_the_prompt_budget() {
        let ids: Vec<String> = (0..600).map(|n| format!("current-lesson-{n:04}")).collect();
        let nodes: Vec<_> = ids
            .iter()
            .map(|id| (Scope::Mine, None, id.as_str(), Status::Current))
            .collect();
        let known = known(&nodes);
        let batch = vec![Candidate {
            source: "claude",
            at_ms: 1_000,
            at: "1970-01-01T00:00:01Z".into(),
            place: "/r/here".into(),
            who: "person".into(),
            context: String::new(),
            text: "Follow the rule I already gave you".into(),
            session: Some("s1".into()),
        }];
        let (sent, _, shown) = prompt_with_evidence(&known, &batch);
        assert!(!sent.contains("current-lesson-0599"));
        let reteaches = vec![serde_json::json!({"candidate": 1, "ids": ["current-lesson-0599"]})];
        let (kept, dropped) = shown.retaught(&reteaches, &batch);
        assert!(kept.is_empty());
        assert_eq!(dropped.not_current, 1);
        assert!(shown
            .cited(&["current-lesson-0599".into()], &batch)
            .is_empty());
    }

    #[test]
    fn evidence_cannot_name_a_redacted_identifier_but_can_name_a_visible_one() {
        let hidden = fake(&["s", "k-", &"a1".repeat(12)]);
        let repo_token = fake(&["s", "k-", &"b2".repeat(12)]);
        let secret_repo = format!("/r/project {repo_token}");
        let loaded = Loaded {
            nodes: vec![
                faija_knowledge::Located {
                    scope: Scope::Mine,
                    node: lesson(&hidden, Status::Current, "A lesson in a scope."),
                },
                faija_knowledge::Located {
                    scope: Scope::Repo,
                    node: lesson("visible-lesson", Status::Current, "A lesson in a scope."),
                },
            ],
            ..Loaded::default()
        };
        let known = Known::of(&loaded, &[], Some(Path::new(&secret_repo)));
        let batch = vec![Candidate {
            source: "claude",
            at_ms: 1_000,
            at: "1970-01-01T00:00:01Z".into(),
            place: secret_repo.clone(),
            who: "person".into(),
            context: String::new(),
            text: "Follow the rule I already gave you".into(),
            session: Some("s1".into()),
        }];
        let (sent, _, shown) = prompt_with_evidence(&known, &batch);
        assert!(!sent.contains(&hidden));
        assert!(sent.contains("- [redacted] [mine/lesson/current]"));
        assert!(
            sent.contains("- visible-lesson [repo/lesson/current] in /r/project [redacted]"),
            "{sent}"
        );
        let reteaches = vec![serde_json::json!({
            "candidate": 1, "ids": [&hidden, "visible-lesson"]
        })];
        let (kept, dropped) = shown.retaught(&reteaches, &batch);
        let visible = crate::evidence::NodeKey::new(
            Scope::Repo,
            Some(Path::new(&secret_repo)),
            "visible-lesson",
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].node, visible);
        assert_eq!(dropped.not_current, 1);
        assert_eq!(
            shown.cited(&[hidden, "visible-lesson".into()], &batch),
            vec![visible]
        );
    }

    #[test]
    fn malformed_answer_objects_cannot_become_nested_legacy_arrays() {
        for answer in [
            r#"{"drafts":[],"reteaches":"not an array"}"#,
            r#"{"drafts":{},"reteaches":[]}"#,
            r#"{"drafts":[]"#,
        ] {
            assert!(parse_answer(answer).is_err(), "{answer}");
        }
    }

    #[test]
    fn the_drafter_is_asked_what_each_candidate_reteaches_and_shown_current_knowledge_first() {
        assert!(
            INSTRUCTIONS.contains("\"reteaches\": [{\"candidate\""),
            "{INSTRUCTIONS}"
        );
        assert!(INSTRUCTIONS.contains("Name only nodes\nlisted with status current"));
        // Stored after what was put aside, current knowledge is still shown
        // first: the last to be cut by the budget.
        let (text, _) = prompt(
            &known(&[
                (Scope::Mine, None, "old-way", Status::Retired),
                (Scope::Mine, None, "superseded-way", Status::Superseded),
                (Scope::Mine, None, "rust-elixir-only", Status::Current),
            ]),
            &[],
        );
        let at = |id: &str| text.find(&format!("- {id} [")).expect(id);
        assert!(at("rust-elixir-only") < at("old-way"), "{text}");
        assert!(at("old-way") < at("superseded-way"), "stored order: {text}");
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
    fn times_reject_signed_fields_and_out_of_range_offsets() {
        for bad in [
            "2026-09-25T-1:00:00Z",
            "2026-09-25T+1:00:00Z",
            "2026-09-25T10:-1:00Z",
            "2026-09-25T10:00:-1Z",
            "2026-09-25T10:00:00+24:00",
            "2026-09-25T10:00:00+00:60",
            "2026-09-25T10:00:00+-1:00",
            "2026-09-25T10:00:00+00:-1",
            "+026-09-25",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
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
            launched_ms: None,
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
            native_env: Vec::new(),
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
            session: None,
        };
        let mut handled = Handled::default();
        handled.answered.insert(said(-5, "never python").key());
        handled.answered.insert(said(7, "we have lima").key());
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
            session: None,
        };
        let (old, a, b, z) = (c(3, "old"), c(10, "a"), c(20, "b"), c(30, "c"));
        let settle_with = |found: &[Candidate],
                           done: &[&Candidate],
                           sessions: Option<&[Undrafted]>,
                           since,
                           complete| {
            let mut handled = Handled::default();
            handled.answered.extend(done.iter().map(|c| c.key()));
            let window = Window {
                since,
                started: 100,
                complete,
            };
            settle(found, &mut handled, sessions, window)
        };
        let found = [a.clone(), b.clone(), z.clone()];
        // The first candidate not handled holds it.
        let held = settle_with(&found, &[&a, &z], None, 5, true);
        assert_eq!((held.cursor, held.left), (Some(10), 1));
        // Nothing left: to where the run started, but only when every
        // source was read; after a warning, or a run of some sources, it
        // stays: it is one cursor for all of them.
        assert_eq!(
            settle_with(&found, &[&a, &b, &z], None, 5, true).cursor,
            Some(100)
        );
        assert_eq!(
            settle_with(&found, &[&a, &b, &z], None, 5, false).cursor,
            None
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
            session: None,
        };
        let mut handled = Handled::default();
        for (at, text) in [(5, "a"), (-7, "b"), (1_793_000_000_123, "c")] {
            handled.answered.insert(said(at, text).key());
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
        // A digest is exactly 16 lowercase hex digits, even when the count
        // agrees.
        for line in [
            "5 000000000000000",
            "5 00000000000000000",
            "5 000000000000000A",
        ] {
            let file = format!("{HANDLED_HEADER}\n{line}\nend 1\n");
            assert!(read(file.as_bytes()).is_err(), "{line}");
        }
        let file =
            format!("{HANDLED_HEADER}\n5 0123456789abcdef failed 2 review no JSON array\nend 1\n");
        let failing = read(file.as_bytes()).unwrap();
        assert_eq!(failing.failing.values().next().map(|f| f.attempts), Some(2));
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
                launched_ms: None,
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
                native_env: Vec::new(),
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
        /// Candidates the drafter never takes: it refuses any batch that
        /// holds one (a refusal answered in prose).
        poison: HashSet<Key>,
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
            session: None,
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
        let poison = if out.is_empty() || !rng.chance(30) {
            HashSet::new()
        } else {
            (0..1 + rng.below(3))
                .map(|_| out[rng.below(out.len() as u64) as usize].key())
                .collect()
        };
        World {
            since,
            window: 1 + rng.below(40_000) as i64,
            said: out,
            sessions,
            reported,
            poison,
            end,
        }
    }

    #[derive(Default)]
    struct State {
        cursor: Option<i64>,
        handled: Handled,
        drafted: HashSet<String>,
    }

    /// What the model saw, across runs.
    #[derive(Default)]
    struct Seen {
        /// Every candidate whose answer was kept.
        kept: HashSet<Key>,
        /// Every candidate set aside, as kept.
        aside: HashSet<Key>,
        /// How often each candidate was refused alone.
        alone: std::collections::HashMap<Key, u8>,
    }

    #[derive(Clone, Copy)]
    enum Fail {
        None,
        /// The drafter refuses this send (1-based), and only this one.
        Once(usize),
        /// It refuses every send from this one on: it answers nothing.
        Outage(usize),
        /// This send gets no answer at all (it did not finish in time).
        Silent(usize),
        /// Killed after this batch was answered, before it was kept.
        Killed(usize),
    }

    struct Plan {
        cap: usize,
        batch: usize,
        /// Every source read, none with a warning.
        complete: bool,
        /// Every transcript read: each session's whole window.
        whole: bool,
        fail: Fail,
        /// `--retry-failed`.
        retry: bool,
        /// The drafter still refuses the world's poison candidates.
        poisoned: bool,
    }

    struct Ran {
        /// Found and not handled when the run began.
        before: usize,
        sent: usize,
        /// Found and not handled when it ended; `None` when it was killed.
        after: Option<usize>,
        /// The work left before and after: each candidate not handled
        /// weighs one more than the failures it has left before it is set
        /// aside.
        work: (usize, usize),
    }

    fn work(found: &[Candidate], handled: &Handled) -> usize {
        found
            .iter()
            .filter(|c| !handled.contains(c))
            .map(|c| {
                let attempts = handled.failing.get(&c.key()).map_or(0, |f| f.attempts);
                usize::from((MAX_ATTEMPTS + 1).saturating_sub(attempts).max(1))
            })
            .sum()
    }

    /// One `ohjaa intake` at `now`, as `cmd_intake` runs it: the same
    /// gathering, then the same calls. Asserts that nothing kept or set aside
    /// is ever sent again and that the cursor never goes before the run's
    /// window.
    fn run(world: &World, state: &mut State, now: i64, plan: &Plan, seen: &mut Seen) -> Ran {
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
        candidates.extend(older.into_iter().filter(|c| within_sessions(c, &queued)));
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
        // A retry reads back to the oldest candidate set aside, for those
        // alone, as `cmd_intake` does.
        let retry: HashSet<Key> = if plan.retry {
            state.handled.set_aside().map(|(key, _)| *key).collect()
        } else {
            HashSet::new()
        };
        if let Some(floor) = retry
            .iter()
            .map(|key| key.at_ms - 1)
            .min()
            .filter(|floor| *floor < since)
        {
            candidates.extend(
                visible()
                    .filter(|c| c.at_ms > floor && retry.contains(&c.key()))
                    .cloned(),
            );
            candidates.extend(
                world
                    .reported
                    .iter()
                    .filter(|(landed, c)| {
                        *landed > floor && *landed <= now && retry.contains(&c.key())
                    })
                    .map(|(_, c)| c.clone()),
            );
        }
        let found = found(candidates);

        let mut handled = state.handled.clone();
        if plan.retry {
            handled.retry_failed();
        }
        let before = found.iter().filter(|c| !handled.contains(c)).count();
        let work_before = work(&found, &state.handled);
        let mut durable = handled.clone();
        let (mut sends, mut keeps, mut killed) = (0, 0, false);
        let (kept, aside, alone) = (&seen.kept, &seen.aside, &mut seen.alone);
        let drafted = draft_pending(
            &found,
            &mut handled,
            plan.batch,
            plan.cap,
            |chunk| {
                sends += 1;
                for c in chunk {
                    assert!(
                        !kept.contains(&c.key()),
                        "`{}` sent after it was kept",
                        c.text
                    );
                    assert!(
                        plan.retry || !aside.contains(&c.key()),
                        "`{}` sent after it was set aside",
                        c.text
                    );
                }
                if matches!(plan.fail, Fail::Silent(n) if n == sends) {
                    return Err(Failure::Silent(anyhow!("did not finish in time")));
                }
                let refused = match plan.fail {
                    Fail::Once(n) => n == sends,
                    Fail::Outage(n) => sends >= n,
                    _ => false,
                } || (plan.poisoned
                    && chunk.iter().any(|c| world.poison.contains(&c.key())));
                if refused {
                    if let [one] = chunk {
                        *alone.entry(one.key()).or_default() += 1;
                    }
                    return Err(Failure::Refused(anyhow!("no JSON array")));
                }
                Ok(())
            },
            |handled| {
                keeps += 1;
                if matches!(plan.fail, Fail::Killed(n) if n == keeps) {
                    killed = true;
                    bail!("killed");
                }
                durable = handled.clone();
                Ok(())
            },
        );
        seen.kept.extend(durable.answered.iter().copied());
        seen.aside.extend(durable.set_aside().map(|(key, _)| *key));
        if killed {
            // Only what was kept survives the kill.
            durable.done_retrying();
            state.handled = durable;
            return Ran {
                before,
                sent: drafted.sent,
                after: None,
                work: (work_before, work(&found, &state.handled)),
            };
        }
        assert_eq!(
            durable, handled,
            "every answer and every failure alone was kept before the next send"
        );
        handled.done_retrying();
        let taken = before.min(plan.cap);
        let failed = drafted.failed.len();
        if matches!(plan.fail, Fail::None | Fail::Once(_)) {
            // Each candidate taken was answered for or failed alone, unless
            // two failed alone in a row (and the run stopped).
            assert!(
                drafted.stopped.is_some() || drafted.sent + failed == taken,
                "{} answered and {failed} failed of {taken}",
                drafted.sent
            );
            assert!(taken == 0 || drafted.sent + failed >= 1, "no progress");
        }

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
            complete: plan.complete,
        };
        let settled = settle(&found, &mut handled, Some(&sessions), window);
        assert!(
            settled
                .cursor
                .is_none_or(|cursor| cursor > since && cursor <= now),
            "the cursor {:?} is outside ({since}, {now}]",
            settled.cursor
        );
        assert!(
            plan.complete || settled.cursor.is_none(),
            "it moved after a partial run"
        );
        let newly_aside = drafted
            .failed
            .iter()
            .filter(|(_, f)| f.attempts >= MAX_ATTEMPTS)
            .count();
        assert_eq!(
            settled.left,
            before - drafted.sent - newly_aside,
            "what was drafted, or set aside, is no longer left"
        );
        let work_after = work(&found, &handled);
        // `write_cursor`: forward only.
        state.cursor = state.cursor.max(settled.cursor);
        state.drafted.extend(settled.drafted);
        state.handled = handled;
        Ran {
            before,
            sent: drafted.sent,
            after: Some(settled.left),
            work: (work_before, work_after),
        }
    }

    #[test]
    fn model_every_run_makes_progress_and_repeated_runs_drain_the_queue() {
        for seed in 0..3_000 {
            let mut rng = Rng(seed);
            let world = world(&mut rng);
            let mut state = State::default();
            let mut seen = Seen::default();
            let mut cursors = Vec::new();
            let mut now = world.since + world.window;
            // While the world changes: runs at random times, with random caps,
            // partial runs and warnings, refusals, outages, silences and kills.
            loop {
                // A transcript that cannot be read is a warning too.
                let whole = !rng.chance(10);
                let plan = Plan {
                    cap: 1 + rng.below(6) as usize,
                    batch: 1 + rng.below(3) as usize,
                    complete: whole && !rng.chance(15),
                    whole,
                    fail: match rng.below(20) {
                        0 | 1 => Fail::Once(1 + rng.below(4) as usize),
                        2 => Fail::Outage(1 + rng.below(3) as usize),
                        3 => Fail::Silent(1 + rng.below(3) as usize),
                        4 | 5 => Fail::Killed(1 + rng.below(3) as usize),
                        _ => Fail::None,
                    },
                    retry: false,
                    poisoned: true,
                };
                run(&world, &mut state, now, &plan, &mut seen);
                cursors.push(state.cursor);
                if now >= world.end {
                    break;
                }
                now += 1 + rng.below(30_000) as i64;
            }
            // Then the world stands still: every run strictly reduces the work
            // left, even past a candidate the drafter never takes, until one
            // reports that nothing is left.
            let mut runs = 0;
            loop {
                now += 1_000;
                let plan = Plan {
                    cap: 1 + rng.below(3) as usize,
                    batch: 1 + rng.below(4) as usize,
                    complete: true,
                    whole: true,
                    fail: Fail::None,
                    retry: false,
                    poisoned: true,
                };
                let ran = run(&world, &mut state, now, &plan, &mut seen);
                cursors.push(state.cursor);
                if ran.before == 0 {
                    assert_eq!((ran.sent, ran.after), (0, Some(0)), "seed {seed}");
                    break;
                }
                assert!(ran.work.1 < ran.work.0, "seed {seed}: no progress");
                runs += 1;
                assert!(runs < 500, "seed {seed}: the queue never drained");
            }
            assert!(
                cursors.windows(2).all(|pair| pair[0] <= pair[1]),
                "seed {seed}: the cursor moved back: {cursors:?}"
            );
            // Every session was drafted from but those holding a candidate set
            // aside, which stay queued so that a retry finds it; and every
            // candidate there was to draft from was drafted from or set aside,
            // never both: those said after the first window began, and every
            // session's own.
            let holding: HashSet<&str> = world
                .sessions
                .iter()
                .filter(|s| {
                    members(&s.at(world.end), &world.said)
                        .iter()
                        .any(|key| seen.aside.contains(key))
                })
                .map(|s| s.id.as_str())
                .collect();
            assert!(
                holding.iter().all(|id| !state.drafted.contains(*id)),
                "seed {seed}: drafted while holding one set aside"
            );
            assert_eq!(
                state.drafted.len() + holding.len(),
                world.sessions.len(),
                "seed {seed}"
            );
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
            let done: HashSet<Key> = seen.kept.union(&seen.aside).copied().collect();
            assert_eq!(
                done, owed,
                "seed {seed}: drafted from exactly what was owed"
            );
            assert!(seen.kept.is_disjoint(&seen.aside), "seed {seed}");
            // Set aside: every candidate the drafter never takes, and only
            // after it failed alone that often.
            for key in world.poison.intersection(&owed) {
                assert!(
                    seen.aside.contains(key),
                    "seed {seed}: poison not set aside"
                );
            }
            for key in &seen.aside {
                assert!(
                    seen.alone[key] >= MAX_ATTEMPTS,
                    "seed {seed}: set aside early"
                );
            }
            // One more run: nothing to draft, and nothing left kept that no
            // run could find again.
            now += 1_000;
            let plan = Plan {
                cap: 1,
                batch: 1,
                complete: true,
                whole: true,
                fail: Fail::None,
                retry: false,
                poisoned: true,
            };
            let ran = run(&world, &mut state, now, &plan, &mut seen);
            assert_eq!((ran.before, ran.sent), (0, 0), "seed {seed}");
            // Kept: what is set aside, until the drafter answers, and what a
            // session still queued may bring back (not before it started).
            let floor = world
                .sessions
                .iter()
                .filter(|s| holding.contains(s.id.as_str()))
                .map(|s| s.started)
                .min()
                .unwrap_or(i64::MAX);
            assert!(
                state.handled.answered.iter().all(|key| key.at_ms >= floor),
                "seed {seed}"
            );
            assert_eq!(state.handled.failing.len(), seen.aside.len(), "seed {seed}");
            // Then the drafter takes them: one `--retry-failed` sends every one
            // set aside again, however far the cursor has moved past it, and
            // the sessions holding them are drafted from at last.
            now += 1_000;
            let retry = Plan {
                cap: 10_000,
                batch: 1 + rng.below(4) as usize,
                complete: true,
                whole: true,
                fail: Fail::None,
                retry: true,
                poisoned: false,
            };
            let ran = run(&world, &mut state, now, &retry, &mut seen);
            assert_eq!(
                ran.sent,
                seen.aside.len(),
                "seed {seed}: not all sent again"
            );
            assert_eq!(state.handled.set_aside().count(), 0, "seed {seed}");
            assert_eq!(state.drafted.len(), world.sessions.len(), "seed {seed}");
            assert!(seen.aside.is_subset(&seen.kept), "seed {seed}");
            now += 1_000;
            let ran = run(&world, &mut state, now, &plan, &mut seen);
            assert_eq!((ran.before, ran.sent), (0, 0), "seed {seed}");
            assert!(state.handled.is_empty(), "seed {seed}: {:?}", state.handled);
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
            poison: HashSet::new(),
            end: 100_000 + 842,
        };
        let mut state = State::default();
        let mut seen = Seen::default();
        let plan = Plan {
            cap: 400,
            batch: 40,
            complete: true,
            whole: true,
            fail: Fail::None,
            retry: false,
            poisoned: true,
        };
        let mut lefts = Vec::new();
        for run_number in 1..=5 {
            let ran = run(&world, &mut state, 200_000 + run_number, &plan, &mut seen);
            lefts.push(ran.after.unwrap());
        }
        assert_eq!(lefts, [1328 - 400, 528, 128, 0, 0]);
        assert_eq!(state.drafted.len(), 65);
    }
}
