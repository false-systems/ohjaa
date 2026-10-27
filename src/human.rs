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
//! snapshot is old. What the snapshot says is said with how old it is, and
//! what it could not read is said as that, never as an all-clear.
//!
//! What stays until it is resolved comes first, in this order: a branch
//! with a failed commit; what is unknown (a source that could not be read,
//! a session whose ohjaa is gone, an orphan, a commit sykli could say
//! nothing about); a branch whose commits lack evidence; what waits for a
//! person (a session open a day).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::collect::{self, Collected, Cost, Part, Pr};
use crate::knowledge::{self, Dirs};
use crate::report::{self, Waiting};
use crate::sessions::{self, clock, Exit, Finished, Left, Session};
use crate::{process, scan};

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// A record may reach its source a while after it happened: a session's
/// record is written as it leaves, GitHub's search lags, and a snapshot is
/// as old as its collect. What happened this close to what was seen is told
/// apart by its key, never missed for its time.
const OVERLAP_MS: i64 = 2 * HOUR_MS;

/// `line` starts a collect in the background past this age, and what the
/// snapshot says is said with its age past it.
const LINE_COLLECTS_AFTER_MS: i64 = 15 * MINUTE_MS;

/// `digest` collects first past this age, and waits for it this long.
const DIGEST_COLLECTS_AFTER_MS: i64 = 5 * MINUTE_MS;
const DIGEST_WAITS: Duration = Duration::from_secs(10);

/// How long `digest` waits for the cursor's claim: a terminal's line holds
/// it a fraction of a second.
const CLAIM_WAITS: Duration = Duration::from_secs(2);

/// An open session older than this waits for a person to look at it.
const OPEN_TOO_LONG_MS: i64 = DAY_MS;

/// The widest line the view prints, in characters.
pub const WIDTH: usize = 100;

/// The day's digest is at most this many lines.
const DAILY_LINES: usize = 15;

/// What the shells an agent runs its tools in carry, each set by the agent
/// (or by `ohjaa run` and `toimija run` around it), never by a person's own
/// startup files. A variable a person may set (`CODEX_HOME`) is not one.
pub const AGENT_MARKERS: [&str; 17] = [
    "OHJAA_SESSION",
    "TOIMIJA_SESSION",
    "TOIMIJA_RUN",
    // Claude Code.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    // Codex: its thread and session, its CI mode and sandbox, and the
    // launcher npm installs.
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_CI",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_MANAGED_BY_NPM",
    // pi.
    "PI_SESSION_ID",
    "PI_CODING_AGENT",
    // opencode.
    "OPENCODE",
    "OPENCODE_SESSION_ID",
    // What Claude Code, pi and opencode set for any tool to read.
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

/// `08:12 UTC` within the day, else `10-26 08:12 UTC`: ohjaa's times are
/// UTC, and say so.
fn short_clock(at_ms: i64, now_ms: i64) -> String {
    let full = clock(at_ms);
    if (now_ms - at_ms).abs() < 20 * HOUR_MS {
        full[11..].to_owned()
    } else {
        full[5..].to_owned()
    }
}

/// `as of 08:12 UTC`, or `never read`.
fn as_of(at_ms: Option<i64>, now_ms: i64) -> String {
    match at_ms {
        Some(at) => format!("as of {}", short_clock(at, now_ms)),
        None => "never read".to_owned(),
    }
}

/// Read longer ago than a fresh snapshot is: said with its age.
fn old(at_ms: Option<i64>, now_ms: i64) -> bool {
    at_ms.is_none_or(|at| now_ms - at > LINE_COLLECTS_AFTER_MS)
}

// ---- what the person saw ----------------------------------------------------

const SEEN_SCHEMA: &str = "ohjaa-seen.v1";

/// Where a stretch of what happened begins, by each source's own clock:
/// the session log is read now, GitHub as far as the snapshot read it.
/// GitHub's begins [`OVERLAP_MS`] earlier, for what its search indexes
/// late: what the digest that ended here said of those two hours (`said`)
/// is not said again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Window {
    sessions_ms: i64,
    github_ms: i64,
    /// The keys of what GitHub said from the overlap before `github_ms` on,
    /// its very end included, as the digest that ended here had it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    said: BTreeSet<String>,
}

impl Window {
    fn at(ms: i64) -> Window {
        Window {
            sessions_ms: ms,
            github_ms: ms,
            said: BTreeSet::new(),
        }
    }

    /// `item` happened in the stretch from here, by its source's own clock
    /// and from the second (a session's record and GitHub keep whole
    /// seconds); GitHub's from the overlap on, but what the digest that
    /// ended here said. That holds what it said of the very end too: what
    /// merged in the second GitHub was read, or while its search ran.
    fn holds(&self, item: &Item) -> bool {
        let second = |ms: i64| ms - ms.rem_euclid(1000);
        if !item.github {
            return item.at_ms >= second(self.sessions_ms);
        }
        item.at_ms >= second(self.github_ms - OVERLAP_MS) && !self.said.contains(&item.key)
    }
}

/// The cursor: what the person last saw, and when.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    schema: String,
    /// When they last saw a line or a digest.
    at_ms: i64,
    /// How far GitHub had been read by then (the snapshot's own time).
    github_ms: i64,
    /// What happened that they saw, still within the overlap.
    events: BTreeSet<String>,
    /// What stood, and the proposals that waited, when they saw it.
    items: BTreeSet<String>,
    /// Where the next day's digest begins: where the last digest ended.
    digest: Option<Window>,
    /// Where what a day's digest left out begins, the earliest one's when
    /// several were cut: `ohjaa digest` shows it, and it stays until a
    /// person reads that in full. `None` when nothing was left out.
    unseen: Option<Window>,
    /// The person's day the last digest was shown on.
    day: Option<String>,
}

fn seen_path(dirs: &Dirs) -> PathBuf {
    collect::dir(dirs).join("seen.json")
}

/// Held while a terminal works out and says its line, so terminals opened
/// together say it once, and while `ohjaa digest` reads and moves the
/// cursor.
fn claim_path(dirs: &Dirs) -> PathBuf {
    collect::dir(dirs).join("seen.lock")
}

/// What the person was shown.
#[derive(Debug, Clone)]
enum Shown {
    Line,
    /// The day's digest, from `from`; `cut`: it left some out.
    Daily {
        from: Window,
        cut: bool,
    },
    Full,
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

    /// Where the day's digest begins.
    fn daily_from(&self, now_ms: i64) -> Window {
        self.digest
            .clone()
            .unwrap_or_else(|| Window::at(now_ms - DAY_MS))
    }

    /// Where `ohjaa digest` begins: what the person has not seen in full.
    fn full_from(&self, now_ms: i64) -> Window {
        self.unseen
            .clone()
            .unwrap_or_else(|| self.daily_from(now_ms))
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
    /// A source that could not be read, or not all of it.
    Source,
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
    /// What a line says of it, when that is not just counted.
    brief: Option<String>,
}

struct View {
    /// In the order a person must see them.
    standing: Vec<Item>,
    /// Newest first.
    events: Vec<Item>,
    /// The proposals waiting for a person, oldest first, and today's.
    waiting: Vec<Waiting>,
    today: Option<usize>,
    /// Each part of the snapshot: its name, when it was read, and from when
    /// it covers what happened.
    parts: Vec<(&'static str, Option<i64>, Option<i64>)>,
    /// Everything that could not be read, each in full.
    unread: Vec<String>,
    collected_ms: Option<i64>,
}

impl View {
    /// Something could not be read: never shown as all clear.
    fn incomplete(&self) -> bool {
        !self.unread.is_empty()
    }

    /// When GitHub was read from, as far as the snapshot covers it.
    fn github_from(&self) -> Option<i64> {
        self.parts
            .iter()
            .find(|(name, _, _)| *name == "GitHub")
            .and_then(|(_, _, from)| *from)
    }
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

fn amount(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

/// `est. $1.20`, or `115k tokens + 2.9M cache reads`: most of a session's
/// tokens are reads of its cache, said apart.
fn cost(cost: &Cost) -> Option<String> {
    if let Some(usd) = cost.usd_micros {
        return Some(format!("est. ${:.2}", usd as f64 / 1e6));
    }
    let tokens = cost.tokens?;
    Some(match cost.cache_reads {
        Some(cached) if cached > 0 => {
            format!("{} tokens + {} cache reads", amount(tokens), amount(cached))
        }
        _ => format!("{} tokens", amount(tokens)),
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
        repo: clean(&name(repo)),
        text,
        brief: None,
    }
}

fn event(kind: Kind, key: String, at_ms: i64, github: bool, repo: &str, text: String) -> Item {
    Item {
        kind,
        class: None,
        key,
        at_ms,
        github,
        repo: clean(&name(repo)),
        text,
        brief: None,
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

/// A pull request's opening, and its merge or close. A merge into another
/// branch (a stacked pull request) is no landing: it is said with the
/// pull requests.
fn pr_events(pr: &Pr, out: &mut Vec<Item>) {
    let (number, title) = (pr.number, clean(&pr.title));
    let who = |login: &Option<String>| clean(login.as_deref().unwrap_or("?"));
    let mut say = |kind, what: &str, at_ms, text: String| {
        let key = format!("pr:{}#{number}:{what}", pr.repo);
        out.push(event(kind, key, at_ms, true, &pr.repo, text));
    };
    say(
        Kind::Opened,
        "opened",
        pr.created_ms,
        format!("#{number} opened by {}: {title}", who(&pr.author)),
    );
    match (pr.merged_ms, pr.closed_ms) {
        (Some(at), _) if pr.landed() => say(
            Kind::Merged,
            "merged",
            at,
            format!("#{number} merged by {}: {title}", who(&pr.merged_by)),
        ),
        (Some(at), _) => say(
            Kind::Closed,
            "merged",
            at,
            format!("#{number} merged into {}: {title}", who(&pr.base)),
        ),
        (None, Some(at)) => say(
            Kind::Closed,
            "closed",
            at,
            format!("#{number} closed: {title}"),
        ),
        (None, None) => {}
    }
}

/// A part of the snapshot that could not be read, or not all of it: said
/// as that, with how old what it holds is, and never as an all-clear.
fn source<T>(name: &str, part: &Part<T>, now_ms: i64, unread: &mut Vec<String>) -> Option<Item> {
    let age = as_of(part.as_of_ms, now_ms);
    unread.extend(
        part.error
            .iter()
            .map(|why| format!("{name} unavailable, {age}: {}", clean(why))),
    );
    unread.extend(
        part.gaps
            .iter()
            .map(|gap| format!("{name}: {}", clean(gap))),
    );
    let (what, why) = match (&part.error, part.gaps.as_slice()) {
        (Some(why), _) => ("unavailable".to_owned(), why),
        (None, [first, ..]) => (format!("{} could not be read", part.gaps.len()), first),
        (None, []) => return None,
    };
    let failures: Vec<&String> = part.error.iter().chain(&part.gaps).collect();
    let mut item = standing(
        Kind::Source,
        Class::Unknown,
        source_key(name, &failures),
        name,
        format!("{what}, {age}: {}", clean(why)),
    );
    item.brief = Some(format!("{name}: {what}"));
    Some(item)
}

/// A source's failure is said in the line once, and another failure of it
/// again: its key is the failure's own words, but for their numbers, so a
/// count or a time that moved is the same failure.
fn source_key(name: &str, failures: &[&String]) -> String {
    let words: Vec<String> = failures
        .iter()
        .map(|failure| {
            failure
                .chars()
                .map(|c| if c.is_ascii_digit() { '#' } else { c })
                .collect()
        })
        .collect();
    format!("source:{name}:{}", words.join(" | "))
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
    let mut unread = Vec::new();
    let mut standing_items = Vec::new();
    // A branch is said as it was last read, with its age when that is not
    // now: one that could not be read stays until a read resolves it.
    for branch in local.branches.iter().filter(|b| b.unproven > 0) {
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
        let said = format!(
            "{}: {}{} unproven{failed}",
            clean(&branch.branch),
            branch.unproven,
            if branch.more { "+" } else { "" },
        );
        let age = match (&branch.error, old(branch.as_of_ms, now_ms)) {
            (Some(_), _) => format!(", {} (unread since)", as_of(branch.as_of_ms, now_ms)),
            (None, true) => format!(", {}", as_of(branch.as_of_ms, now_ms)),
            (None, false) => String::new(),
        };
        let mut item = standing(
            Kind::Main,
            class,
            format!(
                "main:{}:{}",
                branch.slug,
                branch.tip.as_deref().unwrap_or("")
            ),
            &branch.slug,
            format!(
                "{said} since {}{age}. Prove it: ohjaa gate",
                short(branch.oldest.as_deref().unwrap_or("?"))
            ),
        );
        item.brief = Some(format!("{} {said}", item.repo));
        standing_items.push(item);
    }
    match collected {
        None => {
            let mut item = standing(
                Kind::Source,
                Class::Unknown,
                "source:snapshot".to_owned(),
                "snapshot",
                "nothing collected yet: `ohjaa collect` reads it".to_owned(),
            );
            item.brief = Some("nothing collected yet".to_owned());
            standing_items.push(item);
            unread.push(
                "nothing collected yet: `ohjaa collect` reads the marks, GitHub and vessa"
                    .to_owned(),
            );
        }
        Some(snapshot) => {
            standing_items.extend(source("marks", &snapshot.local, now_ms, &mut unread));
            standing_items.extend(source("GitHub", &snapshot.github, now_ms, &mut unread));
            standing_items.extend(source("vessa", &snapshot.usage, now_ms, &mut unread));
        }
    }
    // Open in the log now: what the snapshot found of them, as of when.
    let found = if old(snapshot.local.as_of_ms, now_ms) {
        format!(", {}", as_of(snapshot.local.as_of_ms, now_ms))
    } else {
        String::new()
    };
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
                format!("still running, its ohjaa gone{found}: {who} ({id})"),
            ));
        } else if local.orphaned.contains(id) {
            standing_items.push(standing(
                Kind::Orphaned,
                Class::Unknown,
                format!("orphaned:{id}"),
                &session.repo,
                format!("orphaned{found}, nothing after its ohjaa went is observed: {who} ({id})"),
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

    let (waiting, today) = match waiting {
        Ok(mut waiting) => {
            waiting
                .sort_by(|a, b| (&a.since, &a.scope, &a.name).cmp(&(&b.since, &b.scope, &b.name)));
            let today =
                (!waiting.is_empty()).then(|| day.rem_euclid(waiting.len() as i64) as usize);
            (waiting, today)
        }
        Err(why) => {
            let mut item = standing(
                Kind::Source,
                Class::Unknown,
                source_key("proposals", &[&why]),
                "proposals",
                format!("could not be read: {}", clean(&why)),
            );
            item.brief = Some("proposals: could not be read".to_owned());
            standing_items.push(item);
            unread.push(format!("the proposals could not be read: {}", clean(&why)));
            (Vec::new(), None)
        }
    };
    standing_items.sort_by_key(|item| item.class);
    let parts = collected
        .map(|c| {
            vec![
                ("marks", c.local.as_of_ms, c.local.from_ms),
                ("GitHub", c.github.as_of_ms, c.github.from_ms),
                ("vessa", c.usage.as_of_ms, c.usage.from_ms),
            ]
        })
        .unwrap_or_default();
    View {
        standing: standing_items,
        events,
        waiting,
        today,
        parts,
        unread,
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

/// The cursor once the person was shown `shown` of `view` at `now_ms`, on
/// their day `today`.
fn advance(
    view: &View,
    old: &Seen,
    collected: Option<&Collected>,
    now_ms: i64,
    shown: Shown,
    today: &str,
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
    // A digest ends where GitHub was read to, by GitHub's clock: what the
    // next collect reads from before then is still the next digest's. Each
    // pull request event in the overlap before that end is in this digest's
    // stretch or was said by the one before it: the next leaves them out of
    // its overlap, and says only what GitHub indexed late.
    let github_end = collected
        .and_then(|c| c.github.as_of_ms)
        .unwrap_or(old.daily_from(now_ms).github_ms);
    let overlap = github_end - OVERLAP_MS;
    let here = Window {
        sessions_ms: now_ms,
        github_ms: github_end,
        said: view
            .events
            .iter()
            .filter(|item| item.github && item.at_ms >= overlap - overlap.rem_euclid(1000))
            .map(|item| item.key.clone())
            .collect(),
    };
    let (digest, unseen, day) = match shown {
        Shown::Line => (old.digest.clone(), old.unseen.clone(), old.day.clone()),
        // What an earlier digest left out begins earlier still: kept, so
        // nothing the person has not seen is dropped.
        Shown::Daily { from, cut } => (
            Some(here),
            old.unseen.clone().or(cut.then_some(from)),
            Some(today.to_owned()),
        ),
        Shown::Full => (Some(here), None, Some(today.to_owned())),
    };
    Seen {
        schema: SEEN_SCHEMA.to_owned(),
        at_ms: now_ms,
        github_ms,
        events,
        items,
        digest,
        unseen,
        day,
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

/// What happened from `from` on ([`Window::holds`]): my sessions, then what
/// landed, then the other pull requests, each newest first.
fn after<'a>(view: &'a View, from: &Window) -> Vec<&'a Item> {
    let group = |kind: Kind| match kind {
        Kind::Ended { .. } => 0,
        Kind::Merged => 1,
        _ => 2,
    };
    let mut events = folded(view.events.iter().filter(|item| from.holds(item)).collect());
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
    // `sykli main: 2 unproven (1 failed)`, as the digest says it.
    let mains: Vec<&&Item> = news
        .standing
        .iter()
        .filter(|item| item.kind == Kind::Main)
        .collect();
    match mains.as_slice() {
        [] => {}
        [one] => parts.extend(one.brief.clone()),
        many => parts.push(format!("{} branches not proven", many.len())),
    }
    parts.extend(
        news.standing
            .iter()
            .filter(|item| item.kind == Kind::Source)
            .filter_map(|item| item.brief.clone()),
    );
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
    // Whole parts only: what does not fit is left to the digest, said with `…`.
    let (more, tail) = (" · …", " · ohjaa digest");
    let mut line = String::from("ohjaa");
    for (i, part) in parts.iter().enumerate() {
        let after = if i + 1 < parts.len() { more } else { "" };
        let width = [line.as_str(), " · ", part, after, tail]
            .iter()
            .map(|s| s.chars().count())
            .sum::<usize>();
        if width > WIDTH && i > 0 {
            line.push_str(more);
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
    let head = format!(
        "  ? today's proposal {} ({})",
        clean(&waiting.name),
        clean(&waiting.scope)
    );
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

/// The day's digest: what stands, today's proposal and what happened from
/// `from`, at most [`DAILY_LINES`] lines, and whether it left any out.
/// `left_out`: where what an earlier one left out begins, still unread in
/// full; the last line points to it. `None` when there is nothing.
fn daily(
    view: &View,
    from: &Window,
    left_out: Option<&Window>,
    now_ms: i64,
) -> Option<(String, bool)> {
    let events = after(view, from);
    let proposal = proposal_line(view);
    if view.standing.is_empty() && events.is_empty() && proposal.is_none() && left_out.is_none() {
        return None;
    }
    let mut summary = vec![format!("since {}", clock(from.sessions_ms))];
    summary.extend(sessions_ended(&events, false));
    summary.extend(pull_requests(&events));
    let mut lines = vec![cut(&format!("ohjaa · {}", summary.join(" · ")), WIDTH)];
    // An old snapshot is said, never waited for: a collect runs beside it.
    if let Some(at) = view.collected_ms.filter(|at| old(Some(*at), now_ms)) {
        lines.push(format!(
            "  snapshot as of {}; a fresh collect runs in the background",
            short_clock(at, now_ms)
        ));
    }
    let width = repo_width(view.standing.iter().chain(events.iter().copied()));
    let mut rows: Vec<String> = view.standing.iter().map(|item| row(item, width)).collect();
    let after_standing = rows.len();
    rows.extend(events.iter().map(|item| row(item, width)));
    // The last line points to `ohjaa digest`, for what this one leaves out
    // and for what an earlier one did.
    let mut room = DAILY_LINES - lines.len() - usize::from(proposal.is_some());
    if rows.len() > room || left_out.is_some() {
        room -= 1;
    }
    let more = rows.len().saturating_sub(room);
    rows.truncate(room);
    if let Some(proposal) = proposal {
        rows.insert(after_standing.min(rows.len()), proposal);
    }
    lines.extend(rows);
    let earlier = left_out.map(|at| {
        format!(
            "what an earlier digest left out, since {}",
            short_clock(at.sessions_ms, now_ms)
        )
    });
    match (more, earlier) {
        (0, None) => {}
        (0, Some(earlier)) => lines.push(format!("  … and {earlier}: ohjaa digest")),
        (more, None) => lines.push(format!("  … {more} more: ohjaa digest")),
        (more, Some(earlier)) => {
            lines.push(format!("  … {more} more, and {earlier}: ohjaa digest"))
        }
    }
    Some((lines.join("\n") + "\n", more > 0))
}

/// Who is live, as `ohjaa fleet` finds them now, and how many scans could
/// not look.
pub struct Fleet {
    live: Vec<String>,
    scanned: usize,
    failed: usize,
}

fn fleet(repos: &[PathBuf]) -> Fleet {
    let scans = scan::scan_fleet(repos);
    Fleet {
        live: scans
            .iter()
            .flat_map(|scan| {
                scan.occupying()
                    .map(|s| format!("{} ({})", clean(&scan.name()), clean(&s.agent)))
            })
            .collect(),
        scanned: scans.len(),
        failed: scans.iter().filter(|scan| scan.error.is_some()).count(),
    }
}

/// All of it: what stands, what happened from `from`, today's proposal, who
/// is live, and how old each source is, with what could not be read.
fn full(
    view: &View,
    from: &Window,
    fleet: Option<&Result<Fleet, String>>,
    pending: Option<&str>,
    now_ms: i64,
) -> String {
    let events = after(view, from);
    let mut out = format!("ohjaa digest · since {}", clock(from.sessions_ms));
    if let Some(at) = view.collected_ms {
        out.push_str(&format!(" · collected {}", short_clock(at, now_ms)));
    }
    out.push('\n');
    // GitHub was read from where the snapshot asked, not from `since`.
    let pull_requests_from = view
        .github_from()
        .map_or(from.github_ms, |read| read.max(from.github_ms));
    if pull_requests_from > from.sessions_ms + MINUTE_MS {
        out.push_str(&format!(
            "pull requests only since {}: GitHub was read from there\n",
            clock(pull_requests_from)
        ));
    }
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
            clock(from.sessions_ms)
        ));
    }
    if let Some(today) = view.today {
        let waiting = &view.waiting[today];
        out.push_str("\nProposal of the day\n");
        out.push_str(&cut(
            &format!(
                "  {} ({}, since {})",
                clean(&waiting.name),
                clean(&waiting.scope),
                clean(&waiting.since)
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
    if let Some(fleet) = fleet {
        out.push_str("\nNow\n");
        let said = match fleet {
            Err(why) => format!("  the fleet could not be read: {}", clean(why)),
            Ok(fleet) if fleet.failed > 0 => format!(
                "  {} live; {} of {} repositories could not be scanned",
                fleet.live.len(),
                fleet.failed,
                fleet.scanned
            ),
            Ok(fleet) if fleet.live.is_empty() => "  nobody is live".to_owned(),
            Ok(fleet) => format!("  {} live: {}", fleet.live.len(), fleet.live.join(", ")),
        };
        out.push_str(&cut(&said, WIDTH));
        out.push('\n');
    }
    if !view.parts.is_empty() || !view.unread.is_empty() {
        out.push_str("\nSources\n");
        let ages: Vec<String> = view
            .parts
            .iter()
            .map(|(name, at, _)| format!("{name} {}", as_of(*at, now_ms)))
            .collect();
        if !ages.is_empty() {
            out.push_str(&cut(&format!("  {}", ages.join(" · ")), WIDTH));
            out.push('\n');
        }
        for said in &view.unread {
            out.push_str(&cut(&format!("  {said}"), WIDTH));
            out.push('\n');
        }
    }
    out
}

// ---- the commands -------------------------------------------------------------

/// What became of asking for a collect.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Collecting {
    Started,
    /// One runs already: it is the one to wait for.
    Running,
    Failed(String),
}

/// Start one `ohjaa collect` in the background, detached from this terminal,
/// unless one is running.
fn start_collect(dirs: &Dirs, roots: &[PathBuf]) -> Collecting {
    match process::lock_file(&collect::lock_path(dirs), Duration::ZERO) {
        Ok(free) => drop(free),
        Err(why) if why.contains("held by another") => return Collecting::Running,
        Err(why) => return Collecting::Failed(why),
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return Collecting::Failed(error.to_string()),
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
    match command.spawn() {
        Ok(_) => Collecting::Started,
        Err(error) => Collecting::Failed(error.to_string()),
    }
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
    // Terminals opened together: the one that claims the line says it, and
    // the others say nothing rather than the same again.
    let Ok(_claim) = process::lock_file(&claim_path(&dirs), Duration::ZERO) else {
        return Ok(());
    };
    let now = now_ms();
    let collected = collect::read(&dirs);
    if old(collected.as_ref().map(|c| c.at_ms), now) {
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
    let old_seen = Seen::read(&dirs).unwrap_or_else(|| Seen::first(now));
    let mut out = std::io::stdout().lock();
    // The day's digest, from the snapshot there is (said as of when it was
    // collected; a fresh one is on its way). It is marked shown before it is
    // shown: where it cannot be marked, it is never shown, rather than at
    // every terminal.
    if old_seen.day.as_deref() != Some(today.as_str()) {
        let from = old_seen.daily_from(now);
        let said = daily(&view, &from, old_seen.unseen.as_ref(), now);
        let cut = said.as_ref().is_some_and(|(_, cut)| *cut);
        let shown = Shown::Daily { from, cut };
        advance(&view, &old_seen, collected.as_ref(), now, shown, &today).write(&dirs)?;
        if let Some((text, _)) = said {
            out.write_all(text.as_bytes())?;
        }
        return Ok(());
    }
    if let Some(text) = one_line(&news(&view, &old_seen)) {
        out.write_all(text.as_bytes())?;
        out.flush()?;
        advance(
            &view,
            &old_seen,
            collected.as_ref(),
            now,
            Shown::Line,
            &today,
        )
        .write(&dirs)?;
    }
    Ok(())
}

/// `ohjaa digest`: everything the person has not seen in full (or since
/// `since`), collecting first when the snapshot is old, and who is live,
/// asked now. Shown to a person, it moves the cursor, under the claim a
/// terminal's line takes; with `since` it never does. Exits 4 when anything
/// could not be read.
pub fn digest(
    roots: &[PathBuf],
    fleet_repos: Result<Vec<PathBuf>>,
    since: Option<&str>,
    no_collect: bool,
) -> Result<i32> {
    let dirs = Dirs::resolve(None)?;
    let now = now_ms();
    let window = since
        .map(|text| report::parse_since(text, now))
        .transpose()?;
    let mut collected = collect::read(&dirs);
    let mut pending = None;
    if !no_collect
        && collected
            .as_ref()
            .is_none_or(|c| now - c.at_ms > DIGEST_COLLECTS_AFTER_MS)
    {
        // Detached, like `line`'s: it finishes whether or not this waits.
        let before = collected.as_ref().map(|c| c.at_ms);
        let asked = start_collect(&dirs, roots);
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
            let snapshot = as_of(before, now);
            if let Collecting::Failed(why) = &asked {
                pending = Some(format!(
                    "no collect could be started ({}); the snapshot is {snapshot}",
                    clean(why)
                ));
                break;
            }
            if Instant::now() >= deadline {
                pending = Some(format!(
                    "a collect goes on in the background; the snapshot is {snapshot}"
                ));
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
    let live = fleet_repos
        .map(|repos| fleet(&repos))
        .map_err(|error| format!("{error:#}"));
    match &live {
        Err(why) => unread.push(format!("the fleet could not be read: {}", clean(why))),
        Ok(live) if live.failed > 0 => unread.push(format!(
            "fleet: {} of {} repositories could not be scanned",
            live.failed, live.scanned
        )),
        Ok(_) => {}
    }
    // The cursor is read and moved under the claim a terminal's line takes:
    // a line said between the two would be said again, or undo this. One
    // that cannot be claimed in a moment leaves the cursor where it was:
    // the next digest shows this again, and loses none of it.
    let claim = (window.is_none() && person_watching())
        .then(|| process::lock_file(&claim_path(&dirs), CLAIM_WAITS));
    let unmoved = match &claim {
        Some(Err(why)) if why.contains("held by another") => {
            eprintln!("ohjaa: the cursor stays where it was: {}", clean(why));
            None
        }
        // It cannot be claimed at all: the cursor cannot be moved, as before.
        Some(Err(why)) => Some(why.clone()),
        _ => None,
    };
    let seen = Seen::read(&dirs);
    let from = window.map(Window::at).unwrap_or_else(|| {
        seen.as_ref()
            .map_or(Window::at(now - DAY_MS), |seen| seen.full_from(now))
    });
    let (today, day) = local_day(now);
    let mut view = build(
        &logged,
        report::waiting(&dirs),
        collected.as_ref(),
        now,
        day,
    );
    view.unread.extend(unread);
    let mut out = std::io::stdout().lock();
    out.write_all(full(&view, &from, Some(&live), pending.as_deref(), now).as_bytes())?;
    out.flush()?;
    if let Some(why) = unmoved {
        anyhow::bail!("the cursor cannot be moved: {why}");
    }
    if matches!(claim, Some(Ok(_))) {
        let old_seen = seen.unwrap_or_else(|| Seen::first(now));
        advance(
            &view,
            &old_seen,
            collected.as_ref(),
            now,
            Shown::Full,
            &today,
        )
        .write(&dirs)?;
    }
    Ok(if view.incomplete() {
        crate::INCOMPLETE
    } else {
        0
    })
}

/// `ohjaa collect`: one collect at a time; one started by `line` or
/// `digest` (`background`) says nothing.
pub fn collect_now(repos: Result<Vec<PathBuf>>, background: bool) -> Result<i32> {
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
    let next = Seen::read(&dirs).map(|seen| seen.full_from(now).github_ms);
    let from = collect::from_ms(next, now);
    let collected = collect::collect(&dirs, repos.map_err(|e| format!("{e:#}")), from, now);
    collect::write(&dirs, &collected)?;
    if !background {
        println!(
            "ohjaa: collected in {:.1}s, from {}",
            started.elapsed().as_secs_f64(),
            clock(from)
        );
        let local = &collected.local.value;
        say_part(
            "marks",
            &collected.local,
            format!(
                "{}, {} not proven at the tip",
                plural(local.branches.len(), "entry", "entries"),
                local.branches.iter().filter(|b| b.unproven > 0).count()
            ),
        );
        say_part(
            "github",
            &collected.github,
            plural(
                collected.github.value.len(),
                "pull request",
                "pull requests",
            ),
        );
        say_part(
            "vessa",
            &collected.usage,
            format!(
                "{} costed",
                plural(collected.usage.value.len(), "session", "sessions")
            ),
        );
    }
    let failed = collected.local.failed() || collected.github.failed() || collected.usage.failed();
    Ok(if failed { crate::INCOMPLETE } else { 0 })
}

/// `  marks   10 entries, 1 not proven at the tip; 1 could not be read: …`.
fn say_part<T>(name: &str, part: &Part<T>, answer: String) {
    let said = match (&part.error, part.gaps.first()) {
        (Some(why), _) => format!("unavailable: {}", clean(why)),
        (None, Some(first)) => format!(
            "{answer}; {} could not be read: {}",
            part.gaps.len(),
            clean(first)
        ),
        (None, None) => answer,
    };
    println!("{}", cut(&format!("  {name:<7} {said}"), WIDTH));
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
        let seen = advance(&view, &first, None, NOW, Shown::Line, "d");
        assert_eq!(one_line(&news(&view, &seen)), None, "seen is not new");
        // The record of a session that ended before the person looked, read
        // after: new once.
        let mut later = logged.to_vec();
        later.push(session("late", 2 * HOUR_MS, Some(MINUTE_MS), false));
        let view = build(&later, Ok(Vec::new()), None, NOW + MINUTE_MS, 0);
        let line = one_line(&news(&view, &seen)).expect("the late record is new");
        assert!(line.contains("1 session ended (1 without"), "{line}");
        let seen = advance(&view, &seen, None, NOW + MINUTE_MS, Shown::Line, "d");
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
            Shown::Line,
            "d",
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
                    as_of_ms: Some(NOW),
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
        assert!(line.starts_with("ohjaa · 3 branches not proven"), "{line}");
        assert!(line.trim_end().ends_with("· ohjaa digest"), "{line}");
        // No ids in the line, and nothing raw from outside.
        assert!(!line.contains("oh-") && !line.contains("s1"), "{line}");
        let daily = daily(&view, &Window::at(NOW - DAY_MS), None, NOW)
            .unwrap()
            .0;
        assert!(daily.lines().count() <= DAILY_LINES, "{daily}");
        for row in daily.lines() {
            assert!(row.chars().count() <= WIDTH, "{row}");
        }
        assert!(daily.contains("more: ohjaa digest"), "{daily}");
        for row in full(&view, &Window::at(NOW - DAY_MS), None, None, NOW).lines() {
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
            as_of_ms: Some(NOW),
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
        let full = full(&view, &Window::at(NOW - DAY_MS), None, None, NOW);
        assert!(
            full.contains("main: 1 unproven (1 failed) since 093dfc5. Prove it: ohjaa gate"),
            "{full}"
        );
        assert!(full.contains("still running, its ohjaa gone"), "{full}");
        // A new tip is a new item: said again.
        let seen = advance(
            &view,
            &Seen::first(NOW),
            Some(&collected),
            NOW,
            Shown::Line,
            "d",
        );
        collected.local.value.branches[2].tip = Some("newer".into());
        let view = build(&logged, Ok(Vec::new()), Some(&collected), NOW, 0);
        let line = one_line(&news(&view, &seen)).unwrap();
        assert!(
            line.contains("ohjaa · failed main: 1 unproven (1 failed) · ohjaa digest"),
            "{line}"
        );
    }

    #[test]
    fn one_proposal_a_day_with_its_id_and_the_command_that_decides_it() {
        let proposals: Vec<Waiting> = ["b", "a", "c"]
            .iter()
            .map(|name| waiting(name, "2026-10-20"))
            .collect();
        let today = |day: i64| {
            let view = build(&[], Ok(proposals.clone()), Some(&snapshot(NOW)), NOW, day);
            let daily = daily(&view, &Window::at(NOW - DAY_MS), None, NOW)
                .unwrap()
                .0;
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
        let full = full(&view, &Window::at(NOW - DAY_MS), None, None, NOW);
        assert!(full.contains("Proposal of the day\n  b (ours, since 2026-10-20)"));
        assert!(full.contains("3 wait. Decide: ohjaa review"), "{full}");
    }

    #[test]
    fn new_proposals_are_news_once() {
        let mut proposals = vec![waiting("a", "2026-10-20")];
        let view = build(&[], Ok(proposals.clone()), None, NOW, 0);
        let line = one_line(&news(&view, &Seen::first(NOW))).unwrap();
        assert!(line.contains("1 new proposal"), "{line}");
        let seen = advance(&view, &Seen::first(NOW), None, NOW, Shown::Line, "d");
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
                tokens: Some(115_248),
                cache_reads: Some(2_923_935),
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
            "claude \"fix the parser\" · exit 0 · 60m · no read-back · 115k tokens + 2.9M cache \
             reads"
        );
    }

    /// Review of #63, finding 1: what could not be read is said where the
    /// view is read, and makes the digest incomplete; never an all-clear.
    #[test]
    fn what_could_not_be_read_is_said_and_never_an_all_clear() {
        let mut collected = snapshot(NOW);
        collected.local.gaps = vec!["false-systems/valvo: not marked: no clone".into()];
        collected.github.error = Some("gh api graphql: not logged in".into());
        let view = view(&[], Some(&collected));
        assert!(view.incomplete());
        let rows: Vec<&str> = view
            .standing
            .iter()
            .map(|item| item.text.as_str())
            .collect();
        assert_eq!(
            rows,
            vec![
                "1 could not be read, as of 08:00 UTC: false-systems/valvo: not marked: no clone",
                "unavailable, as of 08:00 UTC: gh api graphql: not logged in",
            ]
        );
        let line = one_line(&news(&view, &Seen::first(NOW))).unwrap();
        assert!(
            line.contains("marks: 1 could not be read · GitHub: unavailable"),
            "{line}"
        );
        let full = full(&view, &Window::at(NOW - DAY_MS), None, None, NOW);
        assert!(full.contains("! marks"), "{full}");
        assert!(
            full.contains("Sources\n  marks as of 08:00 UTC · GitHub as of 08:00 UTC"),
            "{full}"
        );
        // Nothing collected yet is unknown too, not nothing.
        let none = view_of_nothing();
        assert!(none.incomplete());
        assert_eq!(
            none.standing[0].brief.as_deref(),
            Some("nothing collected yet")
        );
        // And all clear only when everything was read.
        assert!(!view_ok().incomplete());
    }

    fn view_of_nothing() -> View {
        build(&[], Ok(Vec::new()), None, NOW, 0)
    }

    fn view_ok() -> View {
        build(&[], Ok(Vec::new()), Some(&snapshot(NOW)), NOW, 0)
    }

    /// Finding 2: a branch whose marks could not be read stays open as it
    /// was last read, said with that time.
    #[test]
    fn a_branch_that_could_not_be_read_stays_open_as_of_its_last_read() {
        let mut collected = snapshot(NOW);
        collected.local.value.branches = vec![Branch {
            slug: "false-systems/habitat".into(),
            branch: "main".into(),
            tip: Some("86bd44c0".into()),
            unproven: 3,
            failed: 1,
            oldest: Some("1a2b3c4d".into()),
            as_of_ms: Some(NOW - 3 * HOUR_MS),
            error: Some("git: cannot run".into()),
            ..Branch::default()
        }];
        collected.local.gaps = vec!["false-systems/habitat: not marked: git: cannot run".into()];
        let failed = view(&[], Some(&collected));
        assert_eq!(failed.standing[0].class, Some(Class::Failed));
        assert_eq!(
            failed.standing[0].text,
            "main: 3 unproven (1 failed) since 1a2b3c4, as of 05:00 UTC (unread since). Prove \
             it: ohjaa gate"
        );
        assert!(failed.incomplete());
        // An old read, not a failed one, is said with its age too.
        collected.local.value.branches[0].error = None;
        collected.local.gaps.clear();
        let view = view(&[], Some(&collected));
        assert!(
            view.standing[0].text.contains(", as of 05:00 UTC."),
            "{}",
            view.standing[0].text
        );
    }

    /// Finding 3: what the day's digest left out is where `ohjaa digest`
    /// starts; once read in full, the next starts after it. GitHub's part of
    /// a window is by GitHub's clock: the snapshot's time.
    #[test]
    fn what_the_days_digest_left_out_is_where_ohjaa_digest_starts() {
        let old = snapshot(NOW - 3 * HOUR_MS);
        let view = view(&[], Some(&old));
        let from = Window::at(NOW - DAY_MS);
        let first = Seen::first(NOW);
        let cut = advance(
            &view,
            &first,
            Some(&old),
            NOW,
            Shown::Daily {
                from: from.clone(),
                cut: true,
            },
            "d",
        );
        assert_eq!(
            cut.full_from(NOW),
            from,
            "the full digest shows what was left out"
        );
        assert_eq!(
            cut.daily_from(NOW),
            Window {
                sessions_ms: NOW,
                github_ms: NOW - 3 * HOUR_MS,
                said: BTreeSet::new(),
            },
            "the next day's starts where GitHub was read to"
        );
        let whole = advance(
            &view,
            &first,
            Some(&old),
            NOW,
            Shown::Daily { from, cut: false },
            "d",
        );
        assert_eq!(whole.full_from(NOW), whole.daily_from(NOW));
        let read = advance(&view, &cut, Some(&old), NOW + HOUR_MS, Shown::Full, "d");
        assert_eq!(read.unseen, None);
        assert_eq!(read.full_from(NOW).sessions_ms, NOW + HOUR_MS);
    }

    /// Second review of #63, finding 2: what a day's digest left out stays
    /// where `ohjaa digest` begins until it is read in full, whatever the
    /// days' digests after it leave out or not.
    #[test]
    fn what_a_days_digest_left_out_stays_until_it_is_read_in_full() {
        let snap = snapshot(NOW);
        let view = view(&[], Some(&snap));
        let day = |n: i64| NOW + n * DAY_MS;
        let first = advance(
            &view,
            &Seen::first(day(0)),
            Some(&snap),
            day(0),
            Shown::Daily {
                from: Window::at(day(-1)),
                cut: true,
            },
            "d0",
        );
        assert_eq!(first.full_from(day(0)), Window::at(day(-1)));
        let whole = advance(
            &view,
            &first,
            Some(&snap),
            day(1),
            Shown::Daily {
                from: first.daily_from(day(1)),
                cut: false,
            },
            "d1",
        );
        assert_eq!(
            whole.full_from(day(1)),
            Window::at(day(-1)),
            "a whole digest the next day drops nothing left out before"
        );
        let cut_again = advance(
            &view,
            &whole,
            Some(&snap),
            day(2),
            Shown::Daily {
                from: whole.daily_from(day(2)),
                cut: true,
            },
            "d2",
        );
        assert_eq!(
            cut_again.full_from(day(2)),
            Window::at(day(-1)),
            "the earliest is kept"
        );
        let seen = advance(&view, &cut_again, Some(&snap), day(2), Shown::Line, "d2");
        assert_eq!(
            seen.full_from(day(2)),
            Window::at(day(-1)),
            "a line moves none"
        );
        let read = advance(&view, &seen, Some(&snap), day(3), Shown::Full, "d3");
        assert_eq!(read.unseen, None, "read in full");
    }

    /// Second review of #63, finding 2: the day's digest points to what an
    /// earlier one left out while it is unread, within its lines.
    #[test]
    fn the_days_digest_points_to_what_an_earlier_one_left_out() {
        let earlier = Window::at(NOW - 2 * DAY_MS);
        let quiet = build(&[], Ok(Vec::new()), Some(&snapshot(NOW)), NOW, 0);
        let (said, cut) = daily(&quiet, &Window::at(NOW - DAY_MS), Some(&earlier), NOW)
            .expect("said, though nothing else is");
        assert!(!cut);
        assert!(
            said.trim_end().ends_with(&format!(
                "  … and what an earlier digest left out, since {}: ohjaa digest",
                short_clock(NOW - 2 * DAY_MS, NOW)
            )),
            "{said}"
        );
        let mut busy = snapshot(NOW);
        busy.github.value = (1..40).map(|n| pr(n, HOUR_MS, Some(HOUR_MS / 2))).collect();
        let view = build(&[], Ok(Vec::new()), Some(&busy), NOW, 0);
        let (said, cut) = daily(&view, &Window::at(NOW - DAY_MS), Some(&earlier), NOW).unwrap();
        assert!(cut);
        assert_eq!(said.lines().count(), DAILY_LINES, "{said}");
        let last = said.lines().last().unwrap();
        assert!(
            last.starts_with("  … 26 more, and what an earlier digest left out, since "),
            "{said}"
        );
        assert!(last.chars().count() <= WIDTH, "{last}");
    }

    /// Second review of #63, finding 3: a pull request GitHub indexes late,
    /// merged before the snapshot the last digest was shown from, is in the
    /// next digest, and what that digest said is not said again.
    #[test]
    fn a_pull_request_indexed_late_is_in_the_next_digest_and_nothing_twice() {
        let mut before = snapshot(NOW);
        before.github.value = vec![pr(79, 3 * HOUR_MS, Some(30 * MINUTE_MS))];
        let shown = view(&[], Some(&before));
        let first = Window::at(NOW - DAY_MS);
        assert_eq!(after(&shown, &first).len(), 1, "#79 merged");
        let seen = advance(
            &shown,
            &Seen::first(NOW),
            Some(&before),
            NOW,
            Shown::Full,
            "d",
        );
        // #80 merged 20 minutes before that snapshot, and is found only now.
        let mut later = snapshot(NOW + HOUR_MS);
        later.github.value = vec![
            pr(79, 3 * HOUR_MS, Some(30 * MINUTE_MS)),
            pr(80, 2 * HOUR_MS, Some(20 * MINUTE_MS)),
        ];
        let view = build(&[], Ok(Vec::new()), Some(&later), NOW + HOUR_MS, 0);
        let next = seen.full_from(NOW + HOUR_MS);
        let keys: Vec<&str> = after(&view, &next)
            .iter()
            .map(|item| item.key.as_str())
            .collect();
        assert_eq!(keys, vec!["pr:false-systems/sykli#80:merged"]);
        // Once said, it is not said again; the day's digest is the same.
        let seen = advance(&view, &seen, Some(&later), NOW + HOUR_MS, Shown::Full, "d");
        assert!(after(&view, &seen.full_from(NOW + HOUR_MS)).is_empty());
        assert!(after(&view, &seen.daily_from(NOW + HOUR_MS)).is_empty());
        // What lies before the overlap is never in it.
        let mut long_ago = pr(81, 5 * HOUR_MS, Some(150 * MINUTE_MS));
        long_ago.title = "merged before the overlap".into();
        let mut stale = later.clone();
        stale.github.value.push(long_ago);
        let view = build(&[], Ok(Vec::new()), Some(&stale), NOW + HOUR_MS, 0);
        assert!(after(&view, &seen.full_from(NOW + HOUR_MS)).is_empty());
    }

    /// Review of #67, finding 1: a pull request at the very end of a
    /// digest's GitHub stretch (merged in the second GitHub was read, or
    /// while its search ran) is said by that digest and not by the next.
    #[test]
    fn a_pull_request_at_the_end_of_a_digests_stretch_is_said_once() {
        // Read at 700 ms into the second #79 merged in; #78 merged while
        // the search ran, two seconds after the read began.
        let mut read = snapshot(NOW + 700);
        read.github.value = vec![pr(79, HOUR_MS, Some(0)), pr(78, HOUR_MS, Some(-2_000))];
        let shown = build(&[], Ok(Vec::new()), Some(&read), NOW + 5_000, 0);
        let keys = |view: &View, from: &Window| -> Vec<String> {
            after(view, from)
                .iter()
                .map(|item| item.key.clone())
                .collect()
        };
        assert_eq!(
            keys(&shown, &Window::at(NOW - DAY_MS)),
            vec![
                "pr:false-systems/sykli#78:merged",
                "pr:false-systems/sykli#79:merged"
            ]
        );
        let seen = advance(
            &shown,
            &Seen::first(NOW + 5_000),
            Some(&read),
            NOW + 5_000,
            Shown::Full,
            "d",
        );
        let mut later = read.clone();
        later.at_ms = NOW + HOUR_MS;
        later.github.as_of_ms = Some(NOW + HOUR_MS);
        let view = build(&[], Ok(Vec::new()), Some(&later), NOW + HOUR_MS, 0);
        assert!(keys(&view, &seen.full_from(NOW + HOUR_MS)).is_empty());
        assert!(keys(&view, &seen.daily_from(NOW + HOUR_MS)).is_empty());
    }

    /// Second review of #63, a nit: a source is said in the line for each
    /// failure, not once while the first stands; the same failure with its
    /// numbers moved (a count, a time) is the same.
    #[test]
    fn another_failure_of_a_source_is_said_again_and_the_same_one_is_not() {
        let mut collected = snapshot(NOW);
        collected.github.error = Some("gh api graphql: not logged in".into());
        collected.local.gaps = vec!["false-systems/valvo: not marked: no clone".into()];
        let first = view(&[], Some(&collected));
        let seen = advance(
            &first,
            &Seen::first(NOW),
            Some(&collected),
            NOW,
            Shown::Line,
            "d",
        );
        assert_eq!(one_line(&news(&first, &seen)), None);
        collected.github.error = Some("gh api graphql: connection refused".into());
        let other = view(&[], Some(&collected));
        let line = one_line(&news(&other, &seen)).expect("another failure");
        assert!(line.contains("GitHub: unavailable"), "{line}");
        assert!(!line.contains("marks"), "the marks failed the same: {line}");
        let seen = advance(&other, &seen, Some(&collected), NOW, Shown::Line, "d");
        collected
            .local
            .gaps
            .push("false-systems/habitat: not marked: no clone".into());
        let more = view(&[], Some(&collected));
        let line = one_line(&news(&more, &seen)).expect("one more could not be read");
        assert!(line.contains("marks: 2 could not be read"), "{line}");
        let seen = advance(&more, &seen, Some(&collected), NOW, Shown::Line, "d");
        // The same cap, read later: its count and time moved, nothing new.
        collected.github.error = None;
        collected.github.gaps =
            vec!["250 pull requests were updated since 2026-10-26 08:00 UTC; only 200 read".into()];
        let capped = view(&[], Some(&collected));
        let seen = advance(&capped, &seen, Some(&collected), NOW, Shown::Line, "d");
        collected.github.gaps =
            vec!["261 pull requests were updated since 2026-10-26 08:15 UTC; only 200 read".into()];
        let later = view(&[], Some(&collected));
        assert_eq!(one_line(&news(&later, &seen)), None);
    }

    /// Finding 5: a window longer than GitHub was read says so.
    #[test]
    fn a_window_longer_than_github_was_read_says_so() {
        let mut collected = snapshot(NOW);
        collected.github.from_ms = Some(NOW - DAY_MS);
        let view = view(&[], Some(&collected));
        let week = full(&view, &Window::at(NOW - 7 * DAY_MS), None, None, NOW);
        assert!(
            week.contains(&format!(
                "pull requests only since {}: GitHub was read from there",
                clock(NOW - DAY_MS)
            )),
            "{week}"
        );
        let day = full(&view, &Window::at(NOW - DAY_MS), None, None, NOW);
        assert!(!day.contains("only since"), "{day}");
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
        let said = full(&view, &Window::at(NOW - DAY_MS), None, None, NOW)
            + &daily(&view, &Window::at(NOW - DAY_MS), None, NOW)
                .unwrap()
                .0
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
        let seen = advance(
            &view,
            &Seen::first(NOW),
            None,
            NOW,
            Shown::Full,
            "2026-10-27",
        );
        seen.write(&dirs).unwrap();
        assert_eq!(Seen::read(&dirs), Some(seen.clone()));
        assert_eq!(seen.digest.map(|w| w.sessions_ms), Some(NOW));
        std::fs::write(seen_path(&dirs), json!({"schema": "other"}).to_string()).unwrap();
        assert_eq!(Seen::read(&dirs), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
