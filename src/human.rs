//! The human view: what happened, for the person at the terminal.
//!
//! An agent learns the state of the work from its briefing; a person learns
//! it here. `ohjaa line` runs as a terminal opens: one line when something
//! happened since the person last saw the view, else nothing, and the first
//! `line` of each local day prints the day's digest instead. `ohjaa digest`
//! is all of it. Records only: nothing is sent to anyone, decided or refused.
//!
//! What a person saw is the cursor: a line or a digest printed for them
//! moves it. Nothing is printed, read or moved unless stdout is a terminal
//! and no agent session's marker is set ([`person_watching`]): an agent's
//! shell reads `~/.zshrc` too.
//!
//! `line` starts no git and touches no network. It reads the session log
//! and the proposals itself, and the rest from the snapshot `ohjaa collect`
//! writes ([`crate::collect`]), which it starts in the background when the
//! snapshot is old.
//!
//! What stays until it is resolved comes first, in this order: a branch
//! with a failed commit; what is unknown (a session whose ohjaa is gone, an
//! orphan, a commit sykli could say nothing about); a branch whose commits
//! lack evidence; what waits for a person (a session open a day, the
//! proposals).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::collect::{self, Collected, Cost, Pr};
use crate::knowledge::{self, Dirs};
use crate::process;
use crate::report::{self, Waiting};
use crate::sessions::{self, clock, Exit, Finished, Left, Session};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// A record may reach its source a while after it happened: a session's
/// record is written as it leaves, GitHub's search lags, and a snapshot is
/// as old as its collect. What happened this close to what was seen is told
/// apart by its key, never missed for its time.
const OVERLAP_MS: i64 = 2 * HOUR_MS;

/// `line` starts a collect in the background past this age.
const LINE_COLLECTS_AFTER_MS: i64 = 15 * MINUTE_MS;

/// `digest` collects first past this age, and waits for it this long.
const DIGEST_COLLECTS_AFTER_MS: i64 = 5 * MINUTE_MS;
const DIGEST_WAITS: Duration = Duration::from_secs(10);

/// An open session older than this waits for a person to look at it.
const OPEN_TOO_LONG_MS: i64 = DAY_MS;

/// The widest line the view prints, in characters.
pub const WIDTH: usize = 100;

/// The day's digest is at most this many lines.
const DAILY_LINES: usize = 15;

/// What the shells an agent runs its tools in carry, each set by the agent
/// (or by `ohjaa run` and `toimija run` around it), never by a person's own
/// startup files. A variable a person may set (`CODEX_HOME`) is not one.
pub const AGENT_MARKERS: [&str; 13] = [
    "OHJAA_SESSION",
    "TOIMIJA_SESSION",
    "TOIMIJA_RUN",
    // Claude Code.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    // Codex: its thread, its sandbox, and the launcher npm installs.
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_MANAGED_BY_NPM",
    "PI_SESSION_ID",
    "OPENCODE",
    // What Claude Code and opencode set for any tool to read.
    "AI_AGENT",
];

/// The marker an agent's shell carries, when one does.
pub fn agent_marker(var: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    AGENT_MARKERS
        .into_iter()
        .find(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// A person reads stdout: it is a terminal, outside every agent session.
pub fn person_watching() -> bool {
    std::io::stdout().is_terminal() && agent_marker(|name| std::env::var_os(name)).is_none()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The person's day by their clock, and its number since 1970-01-01.
/// `OHJAA_TODAY=YYYY-MM-DD` pins it.
fn local_day(now_ms: i64) -> (String, i64) {
    if let Some(date) = std::env::var("OHJAA_TODAY")
        .ok()
        .and_then(|text| knowledge::parse_date(&text))
    {
        return (date.to_string(), knowledge::day_number(date));
    }
    let days = (now_ms.div_euclid(1000) + local_offset_s(now_ms)).div_euclid(86_400);
    (knowledge::date_from_day_number(days).to_string(), days)
}

#[cfg(unix)]
fn local_offset_s(now_ms: i64) -> i64 {
    let at = now_ms.div_euclid(1000) as libc::time_t;
    // SAFETY: localtime_r reads `at` and writes only `tm`, both ours.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&at, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn local_offset_s(_now_ms: i64) -> i64 {
    0
}

// ---- what the person saw ----------------------------------------------------

const SEEN_SCHEMA: &str = "ohjaa-seen.v1";

/// The cursor: what the person last saw, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    schema: String,
    /// When they last saw a line or the digest.
    at_ms: i64,
    /// How far GitHub had been read by then (the snapshot's own time).
    github_ms: i64,
    /// What happened that they saw, still within the overlap.
    events: BTreeSet<String>,
    /// What stood, and the proposals that waited, when they saw it.
    items: BTreeSet<String>,
    /// When the digest was last shown, and on which day of theirs.
    digest_ms: Option<i64>,
    day: Option<String>,
}

fn seen_path(dirs: &Dirs) -> PathBuf {
    collect::dir(dirs).join("seen.json")
}

impl Seen {
    /// Before anything was seen: the last day is new.
    fn first(now_ms: i64) -> Seen {
        Seen {
            schema: SEEN_SCHEMA.to_owned(),
            at_ms: now_ms - DAY_MS,
            github_ms: now_ms - DAY_MS,
            ..Seen::default()
        }
    }

    fn read(dirs: &Dirs) -> Option<Seen> {
        let text = std::fs::read_to_string(seen_path(dirs)).ok()?;
        serde_json::from_str::<Seen>(&text)
            .ok()
            .filter(|seen| seen.schema == SEEN_SCHEMA)
    }

    fn write(&self, dirs: &Dirs) -> Result<()> {
        let path = seen_path(dirs);
        std::fs::create_dir_all(collect::dir(dirs))
            .with_context(|| format!("cannot create {}", collect::dir(dirs).display()))?;
        let partial = path.with_extension("json.partial");
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(&partial, text)
            .and_then(|()| std::fs::rename(&partial, &path))
            .with_context(|| format!("cannot write {}", path.display()))
    }
}

// ---- the view -----------------------------------------------------------------

/// Where a standing item comes in the order a person must see them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Failed,
    Unknown,
    /// Incomplete evidence: no receipt, another contract's, a stale one.
    Evidence,
    Person,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    // What stands until it is resolved.
    Main,
    Gone,
    Orphaned,
    OpenLong,
    // What happened.
    /// `told`: it read back and handed over.
    Ended {
        told: bool,
    },
    Merged,
    Opened,
    Closed,
}

#[derive(Debug, Clone)]
struct Item {
    kind: Kind,
    /// For what stands: its place in the order.
    class: Option<Class>,
    key: String,
    /// For what happened: when, and whether by GitHub's clock.
    at_ms: i64,
    github: bool,
    repo: String,
    text: String,
}

struct View {
    /// In the order a person must see them.
    standing: Vec<Item>,
    /// Newest first.
    events: Vec<Item>,
    /// The proposals waiting for a person, oldest first, and today's.
    waiting: Vec<Waiting>,
    today: Option<usize>,
    live: Vec<collect::Live>,
    /// What could not be read, or is said as of an earlier time.
    sources: Vec<String>,
    /// What the sources said beside their answers.
    notes: Vec<String>,
    collected_ms: Option<i64>,
}

/// Text from outside (an intent, a title, a tool's words) made safe for a
/// terminal: tabs and line breaks become spaces; every other control
/// character and every bidi control becomes U+FFFD, visibly.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\t' | '\n' | '\r' => ' ',
            c if c.is_control() => '\u{fffd}',
            '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            c => c,
        })
        .collect()
}

/// At most `max` characters, the cut said with `…`.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The last part of a path or an `owner/name`.
fn name(place: &str) -> String {
    place
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(place)
        .to_owned()
}

fn span(ms: i64) -> String {
    match ms.max(0) / 1000 {
        s if s < 90 => format!("{s}s"),
        s if s < 90 * 60 => format!("{}m", s / 60),
        s if s < 48 * 3600 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn cost(cost: &Cost) -> Option<String> {
    if let Some(usd) = cost.usd_micros {
        return Some(format!("est. ${:.2}", usd as f64 / 1e6));
    }
    cost.tokens.map(|n| match n {
        n if n >= 1_000_000 => format!("{:.1}M tokens", n as f64 / 1e6),
        n if n >= 1_000 => format!("{}k tokens", n / 1_000),
        n => format!("{n} tokens"),
    })
}

fn proposal_key(waiting: &Waiting) -> String {
    format!("proposal:{}:{}", waiting.scope, waiting.name)
}

fn standing(kind: Kind, class: Class, key: String, repo: &str, text: String) -> Item {
    Item {
        kind,
        class: Some(class),
        key,
        at_ms: 0,
        github: false,
        repo: name(repo),
        text,
    }
}

fn event(kind: Kind, key: String, at_ms: i64, github: bool, repo: &str, text: String) -> Item {
    Item {
        kind,
        class: None,
        key,
        at_ms,
        github,
        repo: name(repo),
        text,
    }
}

/// How a session ended, and what it left out, in a few words.
fn ended(session: &Session, left: &Left, costed: Option<&Cost>) -> Item {
    let mut parts = vec![format!(
        "{} \"{}\"",
        clean(&session.agent),
        cut(&clean(&session.intent), 40)
    )];
    parts.push(match (left.orphaned, left.exit) {
        (true, _) => "orphaned".to_owned(),
        (_, Exit::Code { code }) => format!("exit {code}"),
        (_, Exit::Signal { signal }) => format!("killed (signal {signal})"),
        (_, Exit::Unknown) => "exit unknown".to_owned(),
    });
    if !left.orphaned {
        parts.push(span(left.ended_ms - session.started_ms));
    }
    let (readback, handover) = (session.readback.is_some(), session.handover.is_some());
    match (readback, handover) {
        (false, false) => parts.push("no read-back or handover".to_owned()),
        (false, true) => parts.push("no read-back".to_owned()),
        (true, false) => parts.push("no handover".to_owned()),
        (true, true) => {}
    }
    if !left.landed.is_empty() {
        parts.push(format!("{} landed", left.landed.len()));
    }
    if let Finished::PullRequest { state, .. } = &left.finished {
        parts.push(format!("PR {}", clean(state).to_lowercase()));
    }
    parts.extend(costed.and_then(cost));
    event(
        Kind::Ended {
            told: readback && handover,
        },
        format!("session:{}", session.id),
        left.ended_ms,
        false,
        &session.repo,
        parts.join(" · "),
    )
}

fn pr_events(pr: &Pr, out: &mut Vec<Item>) {
    let key = format!("pr:{}#{}", pr.repo, pr.number);
    let title = clean(&pr.title);
    let who = |login: &Option<String>| clean(login.as_deref().unwrap_or("?"));
    let mut say = |kind, what: &str, at_ms| {
        out.push(event(
            kind,
            format!("{key}:{what}"),
            at_ms,
            true,
            &pr.repo,
            String::new(),
        ));
    };
    say(Kind::Opened, "opened", pr.created_ms);
    let opened = out.len() - 1;
    out[opened].text = format!("#{} opened by {}: {title}", pr.number, who(&pr.author));
    let (kind, what, at_ms, text) = match (pr.merged_ms, pr.closed_ms) {
        (Some(at), _) if pr.landed() => (
            Kind::Merged,
            "merged",
            at,
            format!("#{} merged by {}: {title}", pr.number, who(&pr.merged_by)),
        ),
        (Some(at), _) => (
            Kind::Closed,
            "merged",
            at,
            format!(
                "#{} merged into {}: {title}",
                pr.number,
                who(&pr.base).trim_start_matches('?')
            ),
        ),
        (None, Some(at)) => (
            Kind::Closed,
            "closed",
            at,
            format!("#{} closed: {title}", pr.number),
        ),
        (None, None) => return,
    };
    out.push(event(
        kind,
        format!("{key}:{what}"),
        at_ms,
        true,
        &pr.repo,
        text,
    ));
}

/// Everything the view can say now: from the session log, the proposals
/// and the snapshot (when there is one), at `now_ms` on the person's day
/// number `day`.
fn build(
    logged: &[Session],
    waiting: Result<Vec<Waiting>, String>,
    collected: Option<&Collected>,
    now_ms: i64,
    day: i64,
) -> View {
    let none = Collected::default();
    let snapshot = collected.unwrap_or(&none);
    let local = &snapshot.local.value;
    let mut standing_items = Vec::new();
    for branch in local.branches.iter().filter(|b| b.error.is_none()) {
        if branch.unproven == 0 {
            continue;
        }
        let class = if branch.failed > 0 {
            Class::Failed
        } else if branch.unknown > 0 {
            Class::Unknown
        } else {
            Class::Evidence
        };
        let failed = match branch.failed {
            0 => String::new(),
            n => format!(" ({n} failed)"),
        };
        standing_items.push(standing(
            Kind::Main,
            class,
            format!(
                "main:{}:{}",
                branch.slug,
                branch.tip.as_deref().unwrap_or("")
            ),
            &branch.slug,
            format!(
                "{}: {}{} unproven{failed} since {}. Prove it: ohjaa gate",
                clean(&branch.branch),
                branch.unproven,
                if branch.more { "+" } else { "" },
                short(branch.oldest.as_deref().unwrap_or("?"))
            ),
        ));
    }
    // Open in the log now: what the snapshot found of them, when it did.
    for session in logged.iter().filter(|s| s.left.is_none()) {
        let who = format!(
            "{} \"{}\"",
            clean(&session.agent),
            cut(&clean(&session.intent), 40)
        );
        let id = &session.id;
        if local.gone.contains(id) {
            standing_items.push(standing(
                Kind::Gone,
                Class::Unknown,
                format!("gone:{id}"),
                &session.repo,
                format!("still running, its ohjaa gone: {who} ({id})"),
            ));
        } else if local.orphaned.contains(id) {
            standing_items.push(standing(
                Kind::Orphaned,
                Class::Unknown,
                format!("orphaned:{id}"),
                &session.repo,
                format!("orphaned, nothing after its ohjaa went is observed: {who} ({id})"),
            ));
        } else if now_ms - session.started_ms > OPEN_TOO_LONG_MS {
            standing_items.push(standing(
                Kind::OpenLong,
                Class::Person,
                format!("open:{id}"),
                &session.repo,
                format!("open {}: {who} ({id})", span(now_ms - session.started_ms)),
            ));
        }
    }
    standing_items.sort_by_key(|item| item.class);

    let mut events = Vec::new();
    for session in logged {
        if let Some(left) = &session.left {
            events.push(ended(session, left, snapshot.usage.value.get(&session.id)));
        }
    }
    for pr in &snapshot.github.value {
        pr_events(pr, &mut events);
    }
    events.sort_by_key(|item| std::cmp::Reverse(item.at_ms));

    let mut sources = Vec::new();
    let (waiting, today) = match waiting {
        Ok(mut waiting) => {
            waiting
                .sort_by(|a, b| (&a.since, &a.scope, &a.name).cmp(&(&b.since, &b.scope, &b.name)));
            let today =
                (!waiting.is_empty()).then(|| day.rem_euclid(waiting.len() as i64) as usize);
            (waiting, today)
        }
        Err(why) => {
            sources.push(format!("the proposals could not be read: {}", clean(&why)));
            (Vec::new(), None)
        }
    };
    let mut notes = Vec::new();
    match collected {
        None => sources.push(
            "nothing collected yet: `ohjaa collect` reads the marks, GitHub, vessa and the fleet"
                .to_owned(),
        ),
        Some(snapshot) => {
            let parts = [
                (
                    "marks",
                    &snapshot.local.error,
                    snapshot.local.as_of_ms,
                    &snapshot.local.notes,
                ),
                (
                    "GitHub",
                    &snapshot.github.error,
                    snapshot.github.as_of_ms,
                    &snapshot.github.notes,
                ),
                (
                    "vessa",
                    &snapshot.usage.error,
                    snapshot.usage.as_of_ms,
                    &snapshot.usage.notes,
                ),
                (
                    "fleet",
                    &snapshot.fleet.error,
                    snapshot.fleet.as_of_ms,
                    &snapshot.fleet.notes,
                ),
            ];
            for (part, error, as_of, said) in parts {
                if let Some(why) = error {
                    sources.push(match as_of {
                        Some(at) => format!("{part} as of {}: {}", clock(at), clean(why)),
                        None => format!("{part} unavailable: {}", clean(why)),
                    });
                }
                notes.extend(said.iter().map(|note| format!("{part}: {}", clean(note))));
            }
        }
    }
    View {
        standing: standing_items,
        events,
        waiting,
        today,
        live: snapshot.fleet.value.clone(),
        sources,
        notes,
        collected_ms: collected.map(|c| c.at_ms),
    }
}

/// What is new since `seen`.
struct News<'a> {
    standing: Vec<&'a Item>,
    events: Vec<&'a Item>,
    proposals: usize,
}

fn news<'a>(view: &'a View, seen: &Seen) -> News<'a> {
    News {
        standing: view
            .standing
            .iter()
            .filter(|item| !seen.items.contains(&item.key))
            .collect(),
        events: folded(
            view.events
                .iter()
                .filter(|item| {
                    let from = if item.github {
                        seen.github_ms
                    } else {
                        seen.at_ms
                    };
                    item.at_ms > from - OVERLAP_MS && !seen.events.contains(&item.key)
                })
                .collect(),
        ),
        proposals: view
            .waiting
            .iter()
            .filter(|waiting| !seen.items.contains(&proposal_key(waiting)))
            .count(),
    }
}

/// The cursor once the person has seen `view` at `now_ms`; with `day`, the
/// digest was shown that day.
fn advance(
    view: &View,
    old: &Seen,
    collected: Option<&Collected>,
    now_ms: i64,
    day: Option<&str>,
) -> Seen {
    let github_ms = collected
        .and_then(|c| c.github.as_of_ms)
        .unwrap_or(old.github_ms)
        .max(old.github_ms);
    let events = view
        .events
        .iter()
        .filter(|item| {
            let from = if item.github { github_ms } else { now_ms };
            item.at_ms > from - OVERLAP_MS
        })
        .map(|item| item.key.clone())
        .collect();
    let items = view
        .standing
        .iter()
        .map(|item| item.key.clone())
        .chain(view.waiting.iter().map(proposal_key))
        .collect();
    Seen {
        schema: SEEN_SCHEMA.to_owned(),
        at_ms: now_ms,
        github_ms,
        events,
        items,
        digest_ms: if day.is_some() {
            Some(now_ms)
        } else {
            old.digest_ms
        },
        day: day.map(str::to_owned).or_else(|| old.day.clone()),
    }
}

/// A pull request opened and then merged or closed in the same stretch is
/// said once, as what became of it.
fn folded(events: Vec<&Item>) -> Vec<&Item> {
    let pr = |item: &&Item| item.key.rsplit_once(':').map(|(pr, _)| pr.to_owned());
    let ended: BTreeSet<String> = events
        .iter()
        .filter(|item| matches!(item.kind, Kind::Merged | Kind::Closed))
        .filter_map(pr)
        .collect();
    events
        .into_iter()
        .filter(|item| item.kind != Kind::Opened || !pr(item).is_some_and(|pr| ended.contains(&pr)))
        .collect()
}

/// What happened after `since_ms`: my sessions, then what landed, then the
/// other pull requests, each newest first.
fn after(view: &View, since_ms: i64) -> Vec<&Item> {
    let group = |kind: Kind| match kind {
        Kind::Ended { .. } => 0,
        Kind::Merged => 1,
        _ => 2,
    };
    let mut events = folded(
        view.events
            .iter()
            .filter(|item| item.at_ms > since_ms)
            .collect(),
    );
    events.sort_by_key(|item| group(item.kind));
    events
}

fn count(items: &[&Item], of: impl Fn(Kind) -> bool) -> usize {
    items.iter().filter(|item| of(item.kind)).count()
}

/// `3 sessions ended (1 without read-back/handover)`: with `gaps`, how many
/// of them left no read-back or no handover.
fn sessions_ended(events: &[&Item], gaps: bool) -> Option<String> {
    let ended = count(events, |k| matches!(k, Kind::Ended { .. }));
    if ended == 0 {
        return None;
    }
    let untold = count(events, |k| k == Kind::Ended { told: false });
    let mut text = plural(ended, "session ended", "sessions ended");
    if gaps && untold > 0 {
        text.push_str(&format!(" ({untold} without read-back/handover)"));
    }
    Some(text)
}

/// `PRs: 2 merged, 1 opened`.
fn pull_requests(events: &[&Item]) -> Option<String> {
    let said: Vec<String> = [
        (Kind::Merged, "merged"),
        (Kind::Opened, "opened"),
        (Kind::Closed, "closed"),
    ]
    .into_iter()
    .map(|(kind, what)| (count(events, |k| k == kind), what))
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    (!said.is_empty()).then(|| format!("PRs: {}", said.join(", ")))
}

/// The one line for a terminal's opening: what is new, most urgent first,
/// at most [`WIDTH`] characters; `None` when nothing is.
fn one_line(news: &News) -> Option<String> {
    let mut parts = Vec::new();
    let mains: Vec<&&Item> = news
        .standing
        .iter()
        .filter(|item| item.kind == Kind::Main)
        .collect();
    match mains.as_slice() {
        [] => {}
        [one] => parts.push(format!(
            "{} main {}",
            one.repo,
            if one.class == Some(Class::Failed) {
                "failed"
            } else {
                "unproven"
            }
        )),
        many => parts.push(format!("main unproven in {} repos", many.len())),
    }
    for (kind, one, many) in [
        (
            Kind::Gone,
            "session runs, its ohjaa gone",
            "sessions run, their ohjaa gone",
        ),
        (Kind::Orphaned, "session orphaned", "sessions orphaned"),
    ] {
        match count(&news.standing, |k| k == kind) {
            0 => {}
            n => parts.push(plural(n, one, many)),
        }
    }
    parts.extend(sessions_ended(&news.events, true));
    parts.extend(pull_requests(&news.events));
    if news.proposals > 0 {
        parts.push(plural(news.proposals, "new proposal", "new proposals"));
    }
    match count(&news.standing, |k| k == Kind::OpenLong) {
        0 => {}
        n => parts.push(plural(
            n,
            "session open over a day",
            "sessions open over a day",
        )),
    }
    if parts.is_empty() {
        return None;
    }
    let tail = " · ohjaa digest";
    let mut line = String::from("ohjaa");
    for (i, part) in parts.iter().enumerate() {
        let more = if i + 1 < parts.len() { " · …" } else { "" };
        if line.chars().count() + 3 + part.chars().count() + more.len() + tail.chars().count()
            > WIDTH
            && i > 0
        {
            line.push_str(" · …");
            break;
        }
        line.push_str(" · ");
        line.push_str(part);
    }
    line.push_str(tail);
    Some(format!("{}\n", cut(&line, WIDTH)))
}

/// `  ! sykli    main: 2 unproven since 5fcdeef. Prove it: ohjaa gate`.
fn row(item: &Item, width: usize) -> String {
    let bullet = if item.class.is_some() { "!" } else { "·" };
    cut(
        &format!("  {bullet} {:<width$}  {}", item.repo, item.text),
        WIDTH,
    )
}

fn repo_width<'a>(items: impl Iterator<Item = &'a Item>) -> usize {
    items
        .map(|item| item.repo.chars().count())
        .max()
        .unwrap_or(0)
        .min(12)
}

/// Today's proposal in one line, with its id and the command that decides it.
fn proposal_line(view: &View) -> Option<String> {
    let waiting = &view.waiting[view.today?];
    let head = format!("  ? today's proposal {} ({})", waiting.name, waiting.scope);
    let tail = format!(" · {} wait · ohjaa review", view.waiting.len());
    let room = WIDTH.saturating_sub(head.chars().count() + tail.chars().count() + 2);
    let title = clean(&waiting.title);
    let said = if room >= 12 && !title.is_empty() {
        format!(": {}", cut(&title, room))
    } else {
        String::new()
    };
    Some(cut(&format!("{head}{said}{tail}"), WIDTH))
}

/// The day's digest: what stands, today's proposal and what happened since
/// `since_ms`, at most [`DAILY_LINES`] lines. `None` when there is nothing.
fn daily(view: &View, since_ms: i64) -> Option<String> {
    let events = after(view, since_ms);
    let proposal = proposal_line(view);
    if view.standing.is_empty() && events.is_empty() && proposal.is_none() {
        return None;
    }
    let mut summary = vec![format!("since {}", clock(since_ms))];
    summary.extend(sessions_ended(&events, false));
    summary.extend(pull_requests(&events));
    let mut lines = vec![cut(&format!("ohjaa · {}", summary.join(" · ")), WIDTH)];
    // Only which sources failed: why is in `ohjaa digest`.
    if !view.sources.is_empty() {
        lines.push(cut(
            &format!(
                "  {}; more: ohjaa digest",
                plural(
                    view.sources.len(),
                    "source could not be read",
                    "sources could not be read"
                )
            ),
            WIDTH,
        ));
    }
    let width = repo_width(view.standing.iter().chain(events.iter().copied()));
    let mut rows: Vec<String> = view.standing.iter().map(|item| row(item, width)).collect();
    let after_standing = rows.len();
    rows.extend(events.iter().map(|item| row(item, width)));
    let room = DAILY_LINES - lines.len() - usize::from(proposal.is_some());
    let more = rows.len().saturating_sub(room);
    if more > 0 {
        rows.truncate(room - 1);
    }
    if let Some(proposal) = proposal {
        rows.insert(after_standing.min(rows.len()), proposal);
    }
    lines.extend(rows);
    if more > 0 {
        lines.push(format!("  … {} more: ohjaa digest", more + 1));
    }
    Some(lines.join("\n") + "\n")
}

/// All of it: what stands, what happened since `since_ms`, today's
/// proposal, who is live, and what could not be read.
fn full(view: &View, since_ms: i64, pending: Option<&str>) -> String {
    let events = after(view, since_ms);
    let mut out = format!("ohjaa digest · since {}", clock(since_ms));
    if let Some(at) = view.collected_ms {
        out.push_str(&format!(" · collected {}", clock(at)));
    }
    out.push('\n');
    if let Some(pending) = pending {
        out.push_str(&format!("{pending}\n"));
    }
    let width = repo_width(view.standing.iter().chain(events.iter().copied()));
    let mut section = |title: &str, items: Vec<&Item>| {
        if !items.is_empty() {
            out.push_str(&format!("\n{title}\n"));
            for item in items {
                out.push_str(&row(item, width));
                out.push('\n');
            }
        }
    };
    section("Still open", view.standing.iter().collect());
    let of = |kinds: &[fn(Kind) -> bool]| -> Vec<&Item> {
        events
            .iter()
            .copied()
            .filter(|item| kinds.iter().any(|is| is(item.kind)))
            .collect()
    };
    section("Sessions ended", of(&[|k| matches!(k, Kind::Ended { .. })]));
    section("Landed", of(&[|k| k == Kind::Merged]));
    section(
        "Pull requests",
        of(&[|k| k == Kind::Opened, |k| k == Kind::Closed]),
    );
    if events.is_empty() {
        out.push_str(&format!(
            "\nNothing new happened since {}.\n",
            clock(since_ms)
        ));
    }
    if let Some(today) = view.today {
        let waiting = &view.waiting[today];
        out.push_str("\nProposal of the day\n");
        out.push_str(&cut(
            &format!(
                "  {} ({}, since {})",
                waiting.name, waiting.scope, waiting.since
            ),
            WIDTH,
        ));
        out.push('\n');
        out.push_str(&cut(&format!("  {}", clean(&waiting.title)), WIDTH));
        out.push_str(&format!(
            "\n  {} wait. Decide: ohjaa review\n",
            view.waiting.len()
        ));
    }
    if view.collected_ms.is_some() {
        let live: Vec<String> = view
            .live
            .iter()
            .map(|live| format!("{} ({})", clean(&live.repo), clean(&live.agent)))
            .collect();
        out.push_str("\nNow\n");
        out.push_str(&cut(
            &match live.len() {
                0 => "  nobody is live".to_owned(),
                n => format!("  {} live: {}", n, live.join(", ")),
            },
            WIDTH,
        ));
        out.push_str("\n");
    }
    if !view.sources.is_empty() || !view.notes.is_empty() {
        out.push_str("\nSources\n");
        for said in view.sources.iter().chain(&view.notes) {
            out.push_str(&cut(&format!("  {said}"), WIDTH));
            out.push('\n');
        }
    }
    out
}

// ---- the commands -------------------------------------------------------------

/// Start one `ohjaa collect` in the background, detached from this terminal,
/// unless one is running. Whether it started one.
fn start_collect(dirs: &Dirs, roots: &[PathBuf]) -> bool {
    match process::lock_file(&collect::lock_path(dirs), Duration::ZERO) {
        Ok(free) => drop(free),
        Err(_) => return false,
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut command = Command::new(exe);
    command.args(["collect", "--background"]);
    for root in roots {
        command
            .arg("--root")
            .arg(crate::home::absolute(root.clone()));
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(collect::dir(dirs));
    detach(&mut command);
    command.spawn().is_ok()
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid(2) is async-signal-safe and changes only the child: a
    // session of its own, so no terminal hangs up on it and no shell counts
    // it as a job.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

/// `ohjaa line`: for a terminal's opening. Silent unless a person watches;
/// never an error, whatever happens.
pub fn line(roots: &[PathBuf]) {
    if person_watching() {
        let _ = say_line(roots);
    }
}

fn say_line(roots: &[PathBuf]) -> Result<()> {
    let dirs = Dirs::resolve(None)?;
    let now = now_ms();
    let collected = collect::read(&dirs);
    let fresh = collected
        .as_ref()
        .is_some_and(|c| now - c.at_ms <= LINE_COLLECTS_AFTER_MS);
    if !fresh {
        start_collect(&dirs, roots);
    }
    // A log a writer holds now is read at the next terminal: a terminal
    // never waits for one.
    let log = sessions::Reader::open_now(&sessions::store_path(&dirs.state))?;
    let (logged, _) = log.all()?;
    drop(log);
    let (today, day) = local_day(now);
    let view = build(
        &logged,
        report::waiting(&dirs),
        collected.as_ref(),
        now,
        day,
    );
    let old = Seen::read(&dirs).unwrap_or_else(|| Seen::first(now));
    let mut out = std::io::stdout().lock();
    // The day's digest waits for a fresh snapshot: last night's would miss
    // the night. It is marked shown before it is shown: where it cannot be
    // marked, it is never shown, rather than at every terminal.
    if fresh && old.day.as_deref() != Some(today.as_str()) {
        let since = old.digest_ms.unwrap_or(now - DAY_MS);
        advance(&view, &old, collected.as_ref(), now, Some(&today)).write(&dirs)?;
        if let Some(text) = daily(&view, since) {
            out.write_all(text.as_bytes())?;
        }
        return Ok(());
    }
    if let Some(text) = one_line(&news(&view, &old)) {
        out.write_all(text.as_bytes())?;
        out.flush()?;
        advance(&view, &old, collected.as_ref(), now, None).write(&dirs)?;
    }
    Ok(())
}

/// `ohjaa digest`: everything since the digest was last shown (or since
/// `since`), collecting first when the snapshot is old. Shown to a person,
/// it moves the cursor; with `since` it never does.
pub fn digest(roots: &[PathBuf], since: Option<&str>, no_collect: bool) -> Result<i32> {
    let dirs = Dirs::resolve(None)?;
    let now = now_ms();
    let window = since
        .map(|text| report::parse_since(text, now))
        .transpose()?;
    let mut collected = collect::read(&dirs);
    let mut pending = None;
    let old = collected
        .as_ref()
        .is_none_or(|c| now - c.at_ms > DIGEST_COLLECTS_AFTER_MS);
    if old && !no_collect {
        // Detached, like `line`'s: it finishes whether or not this waits.
        let before = collected.as_ref().map(|c| c.at_ms);
        start_collect(&dirs, roots);
        let terminal = std::io::stderr().is_terminal();
        if terminal {
            eprint!("ohjaa: collecting…\r");
        }
        let deadline = Instant::now() + DIGEST_WAITS;
        loop {
            let read = collect::read(&dirs);
            if read.as_ref().map(|c| c.at_ms) != before {
                collected = read;
                break;
            }
            if Instant::now() >= deadline {
                pending = Some("a fresh collect goes on in the background".to_owned());
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if terminal {
            eprint!("                    \r");
        }
    }
    let mut unread = Vec::new();
    let logged = match sessions::Reader::open(&sessions::store_path(&dirs.state))
        .and_then(|log| log.all())
    {
        Ok((logged, rows)) => {
            if !rows.is_empty() {
                unread.push(format!(
                    "{} session record(s) in the log could not be read",
                    rows.len()
                ));
            }
            logged
        }
        Err(error) => {
            unread.push(format!("the session log could not be read: {error}"));
            Vec::new()
        }
    };
    let seen = Seen::read(&dirs);
    let since_ms = window.unwrap_or_else(|| {
        seen.as_ref()
            .and_then(|seen| seen.digest_ms)
            .unwrap_or(now - DAY_MS)
    });
    let (today, day) = local_day(now);
    let mut view = build(
        &logged,
        report::waiting(&dirs),
        collected.as_ref(),
        now,
        day,
    );
    view.sources.extend(unread);
    let mut out = std::io::stdout().lock();
    out.write_all(full(&view, since_ms, pending.as_deref()).as_bytes())?;
    out.flush()?;
    if window.is_none() && person_watching() {
        let old = seen.unwrap_or_else(|| Seen::first(now));
        advance(&view, &old, collected.as_ref(), now, Some(&today)).write(&dirs)?;
    }
    Ok(if view.sources.is_empty() {
        0
    } else {
        crate::INCOMPLETE
    })
}

/// `ohjaa collect`: one collect at a time; one started by `line` or
/// `digest` (`background`) says nothing.
pub fn collect_now(
    repos: Result<Vec<PathBuf>>,
    fleet: Result<Vec<PathBuf>>,
    background: bool,
) -> Result<i32> {
    let dirs = Dirs::resolve(None)?;
    let _lock = match process::lock_file(&collect::lock_path(&dirs), Duration::ZERO) {
        Ok(lock) => lock,
        Err(why) if why.contains("held by another") => {
            if !background {
                eprintln!("ohjaa: a collect is running already; its snapshot is the next one");
            }
            return Ok(0);
        }
        Err(why) => anyhow::bail!(why),
    };
    let started = Instant::now();
    let now = now_ms();
    let from = collect::from_ms(Seen::read(&dirs).and_then(|seen| seen.digest_ms), now);
    let error = |error: anyhow::Error| format!("{error:#}");
    let collected = collect::collect(&dirs, repos.map_err(error), fleet.map_err(error), from, now);
    collect::write(&dirs, &collected)?;
    let failed = [
        &collected.local.error,
        &collected.github.error,
        &collected.usage.error,
        &collected.fleet.error,
    ]
    .iter()
    .filter(|error| error.is_some())
    .count();
    if !background {
        let say = |part: &str, error: &Option<String>, answer: String| match error {
            Some(why) => println!("  {part:<7} unavailable: {}", clean(why)),
            None => println!("  {part:<7} {answer}"),
        };
        println!(
            "ohjaa: collected in {:.1}s, since {}",
            started.elapsed().as_secs_f64(),
            clock(from)
        );
        let local = &collected.local.value;
        say(
            "marks",
            &collected.local.error,
            format!(
                "{}, {} not proven at the tip",
                plural(local.branches.len(), "entry", "entries"),
                local.branches.iter().filter(|b| b.unproven > 0).count()
            ),
        );
        say(
            "github",
            &collected.github.error,
            plural(
                collected.github.value.len(),
                "pull request",
                "pull requests",
            ),
        );
        say(
            "vessa",
            &collected.usage.error,
            format!(
                "{} costed",
                plural(collected.usage.value.len(), "session", "sessions")
            ),
        );
        say(
            "fleet",
            &collected.fleet.error,
            format!("{} live", collected.fleet.value.len()),
        );
    }
    Ok(if failed == 0 { 0 } else { crate::INCOMPLETE })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{Branch, Local, Part};
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;

    /// A session that started `ago_ms` before NOW and ended `ended_ago`
    /// before it (`None`: still open), with or without its testimony.
    fn session(id: &str, ago_ms: i64, ended_ago: Option<i64>, told: bool) -> Session {
        let mut value = json!({
            "id": id, "agent": "claude", "repo": "/r/ohjaa", "intent": "fix the parser",
            "started_ms": NOW - ago_ms, "delivered": [],
        });
        if let Some(ended) = ended_ago {
            value["left"] = json!({
                "ended_ms": NOW - ended, "exit": {"how": "code", "code": 0},
                "finished": {"kind": "no_pull_request"},
            });
        }
        if told {
            value["readback"] = json!({"at_ms": NOW - ago_ms, "text": "goal"});
            value["handover"] = json!({"at_ms": NOW - ago_ms, "next": "more"});
        }
        serde_json::from_value(value).unwrap()
    }

    fn waiting(name: &str, since: &str) -> Waiting {
        Waiting {
            scope: "ours".into(),
            name: name.into(),
            title: format!("What {name} says, at some length so that it has to be cut short"),
            since: since.into(),
            repo: None,
        }
    }

    fn snapshot(at_ms: i64) -> Collected {
        Collected {
            schema: collect::SCHEMA.into(),
            at_ms,
            local: Part {
                as_of_ms: Some(at_ms),
                ..Part::default()
            },
            github: Part {
                as_of_ms: Some(at_ms),
                ..Part::default()
            },
            usage: Part {
                as_of_ms: Some(at_ms),
                ..Part::default()
            },
            fleet: Part {
                as_of_ms: Some(at_ms),
                ..Part::default()
            },
        }
    }

    fn pr(number: u64, created_ago: i64, merged_ago: Option<i64>) -> Pr {
        Pr {
            repo: "false-systems/sykli".into(),
            number,
            title: "Ask git once".into(),
            url: format!("https://github.com/false-systems/sykli/pull/{number}"),
            author: Some("yairfalse".into()),
            created_ms: NOW - created_ago,
            merged_ms: merged_ago.map(|ago| NOW - ago),
            merged_by: merged_ago.map(|_| "yairfalse".into()),
            closed_ms: merged_ago.map(|ago| NOW - ago),
            base: Some("main".into()),
            default_branch: Some("main".into()),
        }
    }

    fn view(logged: &[Session], collected: Option<&Collected>) -> View {
        build(logged, Ok(Vec::new()), collected, NOW, 0)
    }

    #[test]
    fn every_agent_marker_silences_the_line_and_a_persons_own_variables_do_not() {
        for marker in AGENT_MARKERS {
            let env = |name: &str| (name == marker).then(|| OsString::from("1"));
            assert_eq!(agent_marker(env), Some(marker));
        }
        // Set but empty is not set; what a person exports is no agent's.
        let env = |name: &str| match name {
            "CLAUDECODE" => Some(OsString::new()),
            "CODEX_HOME" | "CLAUDE_CONFIG_DIR" | "TERM_PROGRAM" => Some("x".into()),
            _ => None,
        };
        assert_eq!(agent_marker(env), None);
    }

    #[test]
    fn a_line_says_what_is_new_once_and_seeing_it_moves_the_cursor() {
        let logged = [
            session("a", 3 * HOUR_MS, Some(HOUR_MS), true),
            session("b", 3 * HOUR_MS, Some(HOUR_MS), false),
            session("old", 9 * DAY_MS, Some(8 * DAY_MS), false),
        ];
        let view = view(&logged, None);
        let first = Seen::first(NOW);
        let line = one_line(&news(&view, &first)).expect("something is new");
        assert!(
            line.contains("2 sessions ended (1 without read-back/handover)"),
            "{line}"
        );
        let seen = advance(&view, &first, None, NOW, None);
        assert_eq!(one_line(&news(&view, &seen)), None, "seen is not new");
        // The record of a session that ended before the person looked, read
        // after: new once.
        let mut later = logged.to_vec();
        later.push(session("late", 2 * HOUR_MS, Some(MINUTE_MS), false));
        let view = build(&later, Ok(Vec::new()), None, NOW + MINUTE_MS, 0);
        let line = one_line(&news(&view, &seen)).expect("the late record is new");
        assert!(line.contains("1 session ended (1 without"), "{line}");
        let seen = advance(&view, &seen, None, NOW + MINUTE_MS, None);
        assert_eq!(one_line(&news(&view, &seen)), None);
    }

    #[test]
    fn what_github_said_before_an_old_snapshot_was_read_is_never_lost() {
        // The person saw a line at NOW from a snapshot collected 5 h before:
        // GitHub is seen only as far as it was read.
        let old = snapshot(NOW - 5 * HOUR_MS);
        let seen = advance(
            &view(&[], Some(&old)),
            &Seen::first(NOW),
            Some(&old),
            NOW,
            None,
        );
        assert_eq!(seen.github_ms, NOW - 5 * HOUR_MS);
        let mut fresh = snapshot(NOW + MINUTE_MS);
        fresh.github.value = vec![pr(7, 4 * HOUR_MS, Some(4 * HOUR_MS))];
        let view = build(&[], Ok(Vec::new()), Some(&fresh), NOW + MINUTE_MS, 0);
        let line = one_line(&news(&view, &seen)).expect("merged after the snapshot");
        assert!(line.contains("PRs: 1 merged"), "{line}");
        // Opened and merged in the same stretch: said once, as merged.
        assert!(!line.contains("opened"), "{line}");
    }

    #[test]
    fn a_line_is_short_and_plain_however_much_is_new() {
        let logged: Vec<Session> = (0..40)
            .map(|i| session(&format!("s{i}"), 2 * HOUR_MS, Some(HOUR_MS), i % 2 == 0))
            .collect();
        let mut collected = snapshot(NOW);
        collected.local.value = Local {
            branches: ["sykli", "ohjaa", "habitat"]
                .iter()
                .map(|name| Branch {
                    slug: format!("false-systems/{name}"),
                    branch: "main".into(),
                    tip: Some("5fcdeef0".into()),
                    unproven: 2,
                    failed: 1,
                    oldest: Some("093dfc5a".into()),
                    ..Branch::default()
                })
                .collect(),
            gone: vec!["s1".into()],
            orphaned: Vec::new(),
        };
        collected.github.value = (1..60)
            .map(|n| pr(n, HOUR_MS, (n % 3 == 0).then_some(HOUR_MS / 2)))
            .collect();
        let view = build(
            &logged,
            Ok((0..115)
                .map(|i| waiting(&format!("p{i}"), "2026-10-20"))
                .collect()),
            Some(&collected),
            NOW,
            0,
        );
        let line = one_line(&news(&view, &Seen::first(NOW))).unwrap();
        assert!(line.chars().count() <= WIDTH + 1, "{line}");
        assert!(
            line.starts_with("ohjaa · main unproven in 3 repos"),
            "{line}"
        );
        assert!(line.trim_end().ends_with("· ohjaa digest"), "{line}");
        // No ids in the line, and nothing raw from outside.
        assert!(!line.contains("oh-") && !line.contains("s1"), "{line}");
        let daily = daily(&view, NOW - DAY_MS).unwrap();
        assert!(daily.lines().count() <= DAILY_LINES, "{daily}");
        for row in daily.lines() {
            assert!(row.chars().count() <= WIDTH, "{row}");
        }
        assert!(daily.contains("more: ohjaa digest"), "{daily}");
        for row in full(&view, NOW - DAY_MS, None).lines() {
            assert!(row.chars().count() <= WIDTH, "{row}");
        }
    }

    #[test]
    fn what_stands_comes_in_the_order_a_person_must_see_it() {
        let logged = [
            session("gone", HOUR_MS, None, false),
            session("orphan", HOUR_MS, None, false),
            session("long", 2 * DAY_MS, None, true),
            // Closed since the snapshot found its ohjaa gone: no longer open.
            session("closed", 3 * HOUR_MS, Some(HOUR_MS), true),
        ];
        let mut collected = snapshot(NOW);
        let main = |name: &str, failed, unknown| Branch {
            slug: format!("false-systems/{name}"),
            branch: "main".into(),
            tip: Some(format!("{name}-tip")),
            unproven: 1,
            failed,
            unknown,
            oldest: Some("093dfc5a".into()),
            ..Branch::default()
        };
        collected.local.value = Local {
            branches: vec![
                main("evidence", 0, 0),
                main("unknown", 0, 1),
                main("failed", 1, 0),
                Branch {
                    slug: "false-systems/proven".into(),
                    ..Branch::default()
                },
            ],
            gone: vec!["gone".into(), "closed".into()],
            orphaned: vec!["orphan".into()],
        };
        let view = view(&logged, Some(&collected));
        let said: Vec<(&str, Option<Class>)> = view
            .standing
            .iter()
            .map(|item| (item.key.split(':').nth(1).unwrap(), item.class))
            .collect();
        assert_eq!(
            said,
            vec![
                ("false-systems/failed", Some(Class::Failed)),
                ("false-systems/unknown", Some(Class::Unknown)),
                ("gone", Some(Class::Unknown)),
                ("orphan", Some(Class::Unknown)),
                ("false-systems/evidence", Some(Class::Evidence)),
                ("long", Some(Class::Person)),
            ]
        );
        let full = full(&view, NOW - DAY_MS, None);
        assert!(
            full.contains("main: 1 unproven (1 failed) since 093dfc5. Prove it: ohjaa gate"),
            "{full}"
        );
        assert!(full.contains("still running, its ohjaa gone"), "{full}");
        // A new tip is a new item: said again.
        let seen = advance(&view, &Seen::first(NOW), Some(&collected), NOW, None);
        collected.local.value.branches[2].tip = Some("newer".into());
        let view = build(&logged, Ok(Vec::new()), Some(&collected), NOW, 0);
        let line = one_line(&news(&view, &seen)).unwrap();
        assert!(line.contains("failed main failed"), "{line}");
    }

    #[test]
    fn one_proposal_a_day_with_its_id_and_the_command_that_decides_it() {
        let proposals: Vec<Waiting> = ["b", "a", "c"]
            .iter()
            .map(|name| waiting(name, "2026-10-20"))
            .collect();
        let today = |day: i64| {
            let view = build(&[], Ok(proposals.clone()), Some(&snapshot(NOW)), NOW, day);
            let daily = daily(&view, NOW - DAY_MS).unwrap();
            let line = daily
                .lines()
                .find(|line| line.contains("today's proposal"))
                .unwrap()
                .to_owned();
            assert!(line.ends_with("· 3 wait · ohjaa review"), "{line}");
            assert!(line.chars().count() <= WIDTH, "{line}");
            line
        };
        // Oldest first, then by name; one a day, in turn.
        assert!(today(0).contains("today's proposal a (ours): What a says"));
        assert!(today(1).contains("today's proposal b (ours)"));
        assert!(today(2).contains("today's proposal c (ours)"));
        assert!(today(3).contains("today's proposal a (ours)"));
        let view = build(&[], Ok(proposals), Some(&snapshot(NOW)), NOW, 4);
        let full = full(&view, NOW - DAY_MS, None);
        assert!(full.contains("Proposal of the day\n  b (ours, since 2026-10-20)"));
        assert!(full.contains("3 wait. Decide: ohjaa review"), "{full}");
    }

    #[test]
    fn new_proposals_are_news_once() {
        let mut proposals = vec![waiting("a", "2026-10-20")];
        let view = build(&[], Ok(proposals.clone()), None, NOW, 0);
        let line = one_line(&news(&view, &Seen::first(NOW))).unwrap();
        assert!(line.contains("1 new proposal"), "{line}");
        let seen = advance(&view, &Seen::first(NOW), None, NOW, None);
        assert_eq!(one_line(&news(&view, &seen)), None);
        proposals.push(waiting("b", "2026-10-21"));
        proposals.push(waiting("c", "2026-10-21"));
        let view = build(&[], Ok(proposals), None, NOW, 0);
        assert!(one_line(&news(&view, &seen))
            .unwrap()
            .contains("2 new proposals"));
    }

    #[test]
    fn a_session_says_how_it_ended_what_it_left_out_and_what_it_cost() {
        let mut gone = session("killed", 2 * HOUR_MS, Some(HOUR_MS), false);
        gone.left.as_mut().unwrap().exit = Exit::Signal { signal: 9 };
        let mut orphan = session("orphan", 2 * HOUR_MS, Some(HOUR_MS), false);
        orphan.left.as_mut().unwrap().orphaned = true;
        let mut costed = session("costed", 2 * HOUR_MS, Some(HOUR_MS), true);
        costed.readback = None;
        let mut collected = snapshot(NOW);
        collected.usage.value.insert(
            "costed".into(),
            Cost {
                tokens: Some(3_040_000),
                usd_micros: None,
            },
        );
        let view = view(&[gone, orphan, costed], Some(&collected));
        let said = |id: &str| {
            view.events
                .iter()
                .find(|item| item.key == format!("session:{id}"))
                .unwrap()
                .text
                .clone()
        };
        assert_eq!(
            said("killed"),
            "claude \"fix the parser\" · killed (signal 9) · 60m · no read-back or handover"
        );
        assert_eq!(
            said("orphan"),
            "claude \"fix the parser\" · orphaned · no read-back or handover"
        );
        assert_eq!(
            said("costed"),
            "claude \"fix the parser\" · exit 0 · 60m · no read-back · 3.0M tokens"
        );
    }

    #[test]
    fn text_from_outside_never_reaches_the_terminal_raw() {
        let dirty = "a\u{1b}]52;c;x\u{7}b\u{9b}31m\u{7f}c\u{202e}d\u{2066}e\u{200f}f\tg\nh";
        let cleaned = clean(dirty);
        assert!(cleaned.chars().all(|c| !c.is_control()), "{cleaned:?}");
        assert!(!cleaned.contains('\u{202e}') && !cleaned.contains('\u{2066}'));
        let mut collected = snapshot(NOW);
        let mut hostile = pr(9, HOUR_MS, None);
        hostile.title = dirty.into();
        hostile.author = Some("\u{1b}[2Jx".into());
        collected.github.value = vec![hostile];
        collected.github.error = Some("gh: \u{1b}[31mno".into());
        let mut intent = session("s", 2 * HOUR_MS, Some(HOUR_MS), true);
        intent.intent = "\u{1b}]0;title\u{7}".into();
        let view = view(&[intent], Some(&collected));
        let said = full(&view, NOW - DAY_MS, None)
            + &daily(&view, NOW - DAY_MS).unwrap()
            + &one_line(&news(&view, &Seen::first(NOW))).unwrap();
        assert!(
            said.chars().all(|c| c == '\n' || !c.is_control()),
            "{said:?}"
        );
    }

    #[test]
    fn the_cursor_reads_back_as_written_and_an_unknown_one_is_none() {
        let dir = std::env::temp_dir().join(format!("ohjaa-seen-{}", std::process::id()));
        let dirs = Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: dir.join("mine"),
            machine: dir.join("machine"),
            proposals: dir.join("proposals"),
            state: dir.clone(),
        };
        let view = view(&[session("a", 2 * HOUR_MS, Some(HOUR_MS), true)], None);
        let seen = advance(&view, &Seen::first(NOW), None, NOW, Some("2026-10-27"));
        seen.write(&dirs).unwrap();
        assert_eq!(Seen::read(&dirs), Some(seen.clone()));
        assert_eq!(seen.digest_ms, Some(NOW));
        std::fs::write(seen_path(&dirs), json!({"schema": "other"}).to_string()).unwrap();
        assert_eq!(Seen::read(&dirs), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
