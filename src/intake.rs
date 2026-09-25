//! Intake: turning what people actually said into proposed Knowledge (law 5).
//!
//! A correction a person has to repeat is Knowledge that failed to reach the
//! agent. Intake reads the places corrections happen (agent transcripts, pull
//! request comments, reverts), asks a drafter (an agent CLI such as
//! `claude -p`) which of them carry durable Knowledge, and writes each draft as
//! a *proposal*. Nothing becomes current until a person accepts it.

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
    /// RFC 3339, UTC.
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

/// Accept `YYYY-MM-DD` or a full RFC 3339 UTC time; return RFC 3339.
pub fn normalize_since(text: &str) -> Result<String> {
    let text = text.trim();
    let date = parse_date(text).with_context(|| format!("`{text}` is not a date"))?;
    Ok(if text.len() > 10 {
        text.to_owned()
    } else {
        format!("{date}T00:00:00Z")
    })
}

fn since_seconds(since: &str) -> i64 {
    parse_date(since).map_or(0, |date| day_number(date) * 86_400)
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
pub fn claude_transcript(text: &str, since: &str, out: &mut Vec<Candidate>) {
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
                let at = entry["timestamp"].as_str().unwrap_or("");
                let Some(message) = texts(content, "text").and_then(|t| clean(&t)) else {
                    continue;
                };
                if at > since {
                    out.push(Candidate {
                        source: "claude",
                        at: at.to_owned(),
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
pub fn codex_session(text: &str, since: &str, out: &mut Vec<Candidate>) {
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
                let at = entry["timestamp"].as_str().unwrap_or("");
                if let Some(message) = clean(&said) {
                    if at > since {
                        out.push(Candidate {
                            source: "codex",
                            at: at.to_owned(),
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

pub fn transcripts(home: &Path, since: &str) -> Vec<Candidate> {
    let floor = since_seconds(since);
    let mut out = Vec::new();
    let mut files = Vec::new();
    jsonl_files(&home.join(".claude").join("projects"), floor, &mut files);
    for path in files.drain(..) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            claude_transcript(&text, since, &mut out);
        }
    }
    jsonl_files(&home.join(".codex").join("sessions"), floor, &mut files);
    for path in files {
        if let Ok(text) = std::fs::read_to_string(&path) {
            codex_session(&text, since, &mut out);
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
pub fn reviews(repos: &[PathBuf], since: &str, warnings: &mut Vec<String>) -> Vec<Candidate> {
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
                if let Some(text) = clean(field(4)) {
                    out.push(Candidate {
                        source: "review",
                        at: field(0).to_owned(),
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
pub fn reverts(repos: &[PathBuf], since: &str, warnings: &mut Vec<String>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for repo in repos {
        let log = git(
            repo,
            &[
                "log",
                &format!("--since={since}"),
                "--regexp-ignore-case",
                "--grep=^revert",
                "--max-count=50",
                "--format=%aI%x1f%an%x1f%s%x1f%b%x1e",
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
            if let Some(text) = clean(&format!("{}\n\n{}", fields[2], fields[3])) {
                out.push(Candidate {
                    source: "revert",
                    at: fields[0].to_owned(),
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

// ---- drafting ---------------------------------------------------------------

const INSTRUCTIONS: &str = "\
You maintain a knowledge graph that steers coding agents (Claude, Codex) for the
people they work for. Below are candidates: messages people wrote while working
with agents, comments on pull requests, and reverted commits. Find the ones that
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

pub fn read_cursor(dirs: &Dirs) -> Option<String> {
    std::fs::read_to_string(cursor_path(dirs))
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

pub fn write_cursor(dirs: &Dirs, at: &str) -> Result<()> {
    std::fs::create_dir_all(&dirs.state)?;
    std::fs::write(cursor_path(dirs), format!("{at}\n"))?;
    Ok(())
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
        claude_transcript(&lines.join("\n"), "2026-09-10T00:00:00Z", &mut out);
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
        codex_session(&own.join("\n"), "2026-09-10T00:00:00Z", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "we have lima baba");
        assert_eq!(out[0].context, "Blocked on macOS.");
        assert_eq!(out[0].place, "/p/y");

        let sub = own
            .join("\n")
            .replace(r#""source":"cli""#, r#""source":{"subagent":"review"}"#);
        let mut out = Vec::new();
        codex_session(&sub, "2026-09-10T00:00:00Z", &mut out);
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
            "2026-09-10T00:00:00Z"
        );
        assert!(normalize_since("yesterday").is_err());
    }
}
