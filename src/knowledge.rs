//! The steering graph: what the people an agent works for know and have
//! decided (faija-ontology.v2, in the knowledge-node.v1 format).
//!
//! Toimija reports facts and refuses to decide. This is the other half: the
//! decisions, taste and lessons that steer an agent *before* it acts. Every
//! node carries its time, its why and whose it is; the scope is the directory
//! it lives in.
//!
//! | scope   | lives in                                         | changes by        |
//! |---------|--------------------------------------------------|-------------------|
//! | ours    | `<ours checkout>/knowledge/`                     | review (PR)       |
//! | repo    | `<repo>/AGENTS.md`, `## Lessons` ([`crate::lessons`]) | a PR ohjaa opens |
//! | mine    | `$XDG_CONFIG_HOME/ohjaa/mine/`                   | its owner         |
//! | machine | `$XDG_CONFIG_HOME/ohjaa/machine/`                | its owner         |
//!
//! Proposals wait in `$XDG_STATE_HOME/ohjaa/proposals/` until a person accepts
//! them, so an agent's draft never lands in a shared repository by itself. An
//! accepted repo node is kept in `$XDG_STATE_HOME/ohjaa/lessons/<clone>/`
//! ([`accepted_dir`]), with its why, while its line goes to the repository's
//! AGENTS.md; one accepted before that sits in `<repo>/.ohjaa/knowledge/`
//! until `ohjaa lessons publish` moves it.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use faija_knowledge::{self as fk, Date, Kind, Located, Node, Predicate, Scope, Status};

/// A fact not re-confirmed for this long is flagged: facts expire (law 6).
pub const FACT_TTL_DAYS: i64 = 90;

/// The briefing is read before every action; past this size it stops being
/// read. Going over is a signal to merge and prune. Past it, the briefing and
/// the agents' block keep every decision in full and the rest by
/// priority (`keep_rank`) for as long as it fits, and name what is left by count: a
/// degrade, never a node cut short.
pub const DEFAULT_BUDGET: usize = 8_000;

fn nonempty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home() -> Result<PathBuf> {
    nonempty_env("HOME").context("HOME is not set")
}

/// `$XDG_CONFIG_HOME`, else `~/.config`. An empty variable counts as unset.
pub fn config_home() -> Result<PathBuf> {
    Ok(match nonempty_env("XDG_CONFIG_HOME") {
        Some(dir) => dir,
        None => home()?.join(".config"),
    })
}

/// `$XDG_STATE_HOME`, else `~/.local/state`.
pub fn state_home() -> Result<PathBuf> {
    Ok(match nonempty_env("XDG_STATE_HOME") {
        Some(dir) => dir,
        None => home()?.join(".local").join("state"),
    })
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), nonempty_env("HOME")) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Where every scope and the waiting proposals live, resolved once.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// The shared knowledge checkout, when one is configured.
    pub ours_checkout: Option<PathBuf>,
    /// The repository the command runs in, when it runs in one.
    pub repo_root: Option<PathBuf>,
    pub mine: PathBuf,
    pub machine: PathBuf,
    pub proposals: PathBuf,
    pub state: PathBuf,
}

impl Dirs {
    pub fn resolve(repo_root: Option<&Path>) -> Result<Dirs> {
        let config = config_home()?.join("ohjaa");
        let state = state_home()?.join("ohjaa");
        let ours_checkout = match nonempty_env("OHJAA_OURS") {
            Some(path) => Some(expand_home(&path.to_string_lossy())),
            None => std::fs::read_to_string(config.join("ours"))
                .ok()
                .and_then(|text| {
                    text.lines()
                        .map(|line| line.split('#').next().unwrap_or("").trim())
                        .find(|line| !line.is_empty())
                        .map(expand_home)
                }),
        };
        Ok(Dirs {
            ours_checkout,
            repo_root: repo_root.map(Path::to_path_buf),
            mine: config.join("mine"),
            machine: config.join("machine"),
            proposals: state.join("proposals"),
            state,
        })
    }

    /// The knowledge directory of a scope, or `None` when the scope has no
    /// home here (no ours checkout configured, or not inside a repository).
    pub fn scope_dir(&self, scope: Scope) -> Option<PathBuf> {
        match scope {
            Scope::Ours => self.ours_checkout.as_ref().map(|dir| dir.join("knowledge")),
            Scope::Repo => self.repo_root.as_ref().map(|root| repo_knowledge(root)),
            Scope::Mine => Some(self.mine.clone()),
            Scope::Machine => Some(self.machine.clone()),
        }
    }

    /// Where a proposal for `scope` waits. Repo proposals wait per repository.
    pub fn proposal_dir(&self, scope: Scope, repo: Option<&Path>) -> Result<PathBuf> {
        let dir = self.proposals.join(scope.name());
        Ok(match scope {
            Scope::Repo => dir.join(encode_repo(
                repo.context("a repo proposal needs the repository it belongs to")?,
            )),
            _ => dir,
        })
    }
}

/// Where repo knowledge accepted before ohjaa wrote it into AGENTS.md sits:
/// untracked in the checkout, until `ohjaa lessons publish` moves it.
pub fn repo_knowledge(root: &Path) -> PathBuf {
    root.join(".ohjaa").join("knowledge")
}

/// `.ohjaa/knowledge/`, then `.ohjaa/`, taken out of a checkout once
/// nothing is left in them: what ohjaa left untracked there, gone whole.
pub fn tidy_untracked(root: &Path) {
    let untracked = repo_knowledge(root);
    if std::fs::remove_dir(&untracked).is_ok() {
        if let Some(dot) = untracked.parent() {
            let _ = std::fs::remove_dir(dot);
        }
    }
}

/// The clone `repo` is a checkout of, as its common git directory read from
/// git's layout (no git process), else `repo` itself: every worktree of a
/// clone shares the repo knowledge accepted for it, and what intake saw of it
/// ([`crate::evidence`]).
pub fn clone_key(repo: &Path) -> PathBuf {
    let common = repo
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .and_then(|dir| crate::hook::common_dir_in(dir, None, None));
    let key = common.unwrap_or_else(|| repo.to_path_buf());
    std::fs::canonicalize(&key).unwrap_or(key)
}

/// Where ohjaa keeps the repo knowledge accepted for `repo`'s clone, each
/// node with where its line went (`delivery.json`): in ohjaa's state
/// (`state` is `$XDG_STATE_HOME/ohjaa`), never in the repository.
pub fn accepted_dir(state: &Path, repo: &Path) -> PathBuf {
    state.join("lessons").join(encode_repo(&clone_key(repo)))
}

/// A repository path as one directory name: `/a/b` → `a%2Fb` (and `%` → `%25`).
fn encode_repo(repo: &Path) -> String {
    repo.to_string_lossy()
        .trim_start_matches('/')
        .replace('%', "%25")
        .replace('/', "%2F")
}

fn decode_repo(name: &str) -> PathBuf {
    PathBuf::from(format!("/{}", name.replace("%2F", "/").replace("%25", "%")))
}

/// A file that could not be read as a node, and why.
#[derive(Debug, Clone)]
pub struct Broken {
    pub path: PathBuf,
    pub reason: String,
}

/// A change ohjaa refuses to make: a person-only decision attempted without a
/// person, or a result that would not hold together. Exit code 3, distinct
/// from a failure to act.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse<T>(reason: String) -> Result<T> {
    Err(Refused(reason).into())
}

/// Every node loaded from the scopes that have a home here.
#[derive(Debug, Default)]
pub struct Loaded {
    pub nodes: Vec<Located>,
    /// The file each node was read from, index for index with `nodes`.
    pub files: Vec<PathBuf>,
    pub broken: Vec<Broken>,
    /// Files untracked in a checkout's `.ohjaa/knowledge/` of a node ohjaa's
    /// state holds too (a move cut short): left, the state's copy read.
    pub shadowed: Vec<PathBuf>,
}

impl Loaded {
    /// The first node with this id, for questions an ambiguous id cannot hurt.
    pub fn get(&self, id: &str) -> Option<&Located> {
        self.nodes.iter().find(|located| located.node.id == id)
    }

    /// The one node with this id and its file. An id defined in more than one
    /// scope is refused: changing "it" could change the wrong one.
    pub fn unique(&self, id: &str) -> Result<Option<(&Located, &Path)>> {
        let mut found = self
            .nodes
            .iter()
            .zip(&self.files)
            .filter(|(located, _)| located.node.id == id);
        let first = found.next();
        if found.next().is_some() {
            return refuse(format!(
                "`{id}` is defined in more than one scope; resolve that by hand first (`ohjaa check`)"
            ));
        }
        Ok(first.map(|(located, file)| (located, file.as_path())))
    }
}

fn markdown_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    Ok(files)
}

fn read_node(path: &Path) -> std::result::Result<Node, String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "file name is not UTF-8".to_owned())?;
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    fk::parse_node(stem, &text).map_err(|error| error.to_string())
}

/// Every node of the scopes that have a home here. Repo knowledge is read
/// from ohjaa's state for the repository's clone and from its
/// `.ohjaa/knowledge/`, what was accepted before ohjaa wrote it into AGENTS.md.
pub fn load(dirs: &Dirs) -> Result<Loaded> {
    let mut loaded = Loaded::default();
    for scope in Scope::ALL {
        let mut homes: Vec<PathBuf> = Vec::new();
        // ohjaa's state for a repository first: a node it holds is the one,
        // and a copy of it left untracked in the checkout is shadowed.
        if let (Scope::Repo, Some(root)) = (scope, &dirs.repo_root) {
            homes.push(accepted_dir(&dirs.state, root));
        }
        homes.extend(dirs.scope_dir(scope));
        let accepted = homes.len() > 1;
        for (at, dir) in homes.into_iter().enumerate() {
            let held: Vec<String> = loaded
                .nodes
                .iter()
                .filter(|located| located.scope == scope)
                .map(|located| located.node.id.clone())
                .collect();
            for path in markdown_files(&dir)? {
                match read_node(&path) {
                    Ok(node) if accepted && at > 0 && held.contains(&node.id) => {
                        loaded.shadowed.push(path);
                    }
                    Ok(node) => {
                        loaded.nodes.push(Located { scope, node });
                        loaded.files.push(path);
                    }
                    Err(reason) => loaded.broken.push(Broken { path, reason }),
                }
            }
        }
    }
    Ok(loaded)
}

// ---- time -------------------------------------------------------------------

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn day_number(date: Date) -> i64 {
    let (y, m, d) = (
        i64::from(date.year()),
        i64::from(date.month()),
        i64::from(date.day()),
    );
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn date_from_day_number(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Date::new(year as u16, month as u8, day as u8).expect("civil arithmetic yields real dates")
}

/// Today, in UTC. `OHJAA_TODAY=YYYY-MM-DD` pins it for reproducible runs.
pub fn today() -> Date {
    if let Some(pinned) = std::env::var("OHJAA_TODAY")
        .ok()
        .and_then(|s| parse_date(&s))
    {
        return pinned;
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    date_from_day_number(seconds.div_euclid(86_400))
}

pub fn parse_date(text: &str) -> Option<Date> {
    let mut parts = text.trim().splitn(3, '-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.get(..2)?.parse().ok()?;
    Date::new(year, month, day)
}

// ---- flags ------------------------------------------------------------------

/// Why a current node may no longer hold. A flag never hides a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flag {
    /// A `holds_while` predicate is false right now.
    Failing { id: String, predicate: String },
    /// A fact not re-confirmed for longer than [`FACT_TTL_DAYS`].
    Stale { id: String, days: i64 },
}

impl Flag {
    pub fn id(&self) -> &str {
        match self {
            Flag::Failing { id, .. } | Flag::Stale { id, .. } => id,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Flag::Failing { predicate, .. } => format!("may no longer hold: {predicate} is false"),
            Flag::Stale { days, .. } => format!("fact last confirmed {days} days ago"),
        }
    }
}

pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            candidate.is_file()
        }
    })
}

fn holds(predicate: &Predicate, repo_root: Option<&Path>) -> bool {
    match predicate {
        Predicate::Command(name) => on_path(name),
        Predicate::Path(path) => {
            let resolved = if path.starts_with('/') || path.starts_with("~/") {
                expand_home(path)
            } else {
                match repo_root {
                    Some(root) => root.join(path),
                    None => return false,
                }
            };
            resolved.exists()
        }
    }
}

pub fn flags(loaded: &Loaded, repo_root: Option<&Path>, today: Date) -> Vec<Flag> {
    let mut out = Vec::new();
    for located in &loaded.nodes {
        let node = &located.node;
        if node.status != Status::Current {
            continue;
        }
        for predicate in &node.holds_while {
            if !holds(predicate, repo_root) {
                out.push(Flag::Failing {
                    id: node.id.clone(),
                    predicate: predicate.to_string(),
                });
            }
        }
        let days = day_number(today) - day_number(node.as_of);
        if node.kind == Kind::Fact && days > FACT_TTL_DAYS {
            out.push(Flag::Stale {
                id: node.id.clone(),
                days,
            });
        }
    }
    out
}

// ---- the briefing -----------------------------------------------------------

fn scope_heading(scope: Scope) -> &'static str {
    match scope {
        Scope::Ours => "Ours: decided by the team, binding everywhere",
        Scope::Repo => "This repository",
        Scope::Mine => "Mine: how the person you work for likes to work",
        Scope::Machine => "This machine",
    }
}

/// The Knowledge section an Agent reads before acting, in contract order.
/// Empty when there is no current Knowledge.
pub fn render(loaded: &Loaded, flags: &[Flag]) -> String {
    render_scopes(loaded, flags, &Scope::ALL)
}

/// What opens the knowledge section of a briefing.
pub const PREAMBLE: &str =
    "Knowledge (ohjaa, knowledge-node.v1): what the people you work for know and\n\
     have decided, as of the date on each entry. Follow it. If an entry looks\n\
     wrong or stale, say so instead of quietly working around it. Propose new\n\
     knowledge with `ohjaa propose`; only a person makes it current.\n";

/// The briefing limited to some scopes, in full: a file other people read
/// gets only shared Knowledge, never one person's `mine` or `machine`.
pub fn render_scopes(loaded: &Loaded, flags: &[Flag], scopes: &[Scope]) -> String {
    render_nodes(&in_scopes(loaded, scopes), flags, &[], 0)
}

/// The current nodes of `scopes`, in briefing order.
pub fn in_scopes<'a>(loaded: &'a Loaded, scopes: &[Scope]) -> Vec<&'a Located> {
    fk::briefing(&loaded.nodes)
        .into_iter()
        .filter(|located| scopes.contains(&located.scope))
        .collect()
}

/// What a rendering within a budget holds.
#[derive(Debug)]
pub struct Within<'a> {
    pub text: String,
    /// The nodes it shows, in briefing order.
    pub kept: Vec<&'a Located>,
    /// The nodes it names only by count.
    pub more: usize,
    /// How long the rendering of every node would be.
    pub full: usize,
    /// The budget it was rendered within.
    pub budget: usize,
}

impl Within<'_> {
    /// What is said on stderr when the budget cut it, or its decisions
    /// alone are over it, naming `said` as the budget: the one a reader is
    /// told of (the section's, of which this rendering may be a part).
    pub fn over(&self, said: usize) -> Option<String> {
        let full = self.full;
        let (more, len) = (self.more, self.text.len());
        let advice = "merge or retire some (`ohjaa check`)";
        let rest = self
            .kept
            .iter()
            .filter(|located| located.node.kind != Kind::Decision)
            .count();
        let holds = if rest == 0 {
            "its decisions alone".to_owned()
        } else {
            format!("every decision and {rest} more")
        };
        match (len > self.budget, more) {
            (true, 0) => Some(format!(
                "the knowledge is {full} characters, all of it decisions, over the {said} an \
                 agent will reliably read: the briefing holds every one; {advice}"
            )),
            (true, more) => Some(format!(
                "the knowledge is {full} characters, and its decisions alone are over the {said} \
                 an agent will reliably read: the briefing holds every decision, in {len} \
                 characters, and names the other {more} by count; {advice}"
            )),
            (false, 0) => None,
            (false, more) => Some(format!(
                "the knowledge is {full} characters, over the {said} an agent will reliably \
                 read: the briefing holds {holds}, and names the other {more} by count; {advice}"
            )),
        }
    }
}

/// What the briefing says beside a node, after its id and date: (id, note).
pub type Notes = [(String, String)];

/// The briefing of `nodes` (in briefing order) within `budget` characters,
/// each with its `notes`.
pub fn render_within<'a>(
    nodes: Vec<&'a Located>,
    flags: &[Flag],
    notes: &Notes,
    budget: usize,
) -> Within<'a> {
    within(nodes, budget, |kept, more| {
        render_nodes(kept, flags, notes, more)
    })
}

/// Past the decisions, which node is kept first when not all fit: taste
/// (how the person works, in every session), then lesson (what a correction
/// taught), then fact (the state of the world: the nearest to what can be
/// found out again, and the first to go stale); within a kind, the one last
/// confirmed (`as_of`) first. Ties keep briefing order.
fn keep_rank(located: &Located) -> (u8, std::cmp::Reverse<Date>) {
    let kind = match located.node.kind {
        Kind::Decision => 0,
        Kind::Taste => 1,
        Kind::Lesson => 2,
        Kind::Fact => 3,
    };
    (kind, std::cmp::Reverse(located.node.as_of))
}

/// `nodes` (in briefing order), rendered by `render` (what it shows, and
/// how many it names only by count) within `budget`: all of them when they
/// fit; else every decision, then the rest by [`keep_rank`] for as long as
/// the rendering, with the count of what is left, still fits. The filling
/// stops at the first that does not fit: a later, shorter one never jumps it.
fn within<'a>(
    nodes: Vec<&'a Located>,
    budget: usize,
    render: impl Fn(&[&'a Located], usize) -> String,
) -> Within<'a> {
    let text = render(&nodes, 0);
    let full = text.len();
    if full <= budget {
        return Within {
            text,
            kept: nodes,
            more: 0,
            full,
            budget,
        };
    }
    let mut keep: Vec<bool> = nodes
        .iter()
        .map(|located| located.node.kind == Kind::Decision)
        .collect();
    let mut rest: Vec<usize> = (0..nodes.len()).filter(|&at| !keep[at]).collect();
    rest.sort_by_key(|&at| keep_rank(nodes[at]));
    let chosen = |keep: &[bool]| -> Vec<&'a Located> {
        nodes
            .iter()
            .zip(keep)
            .filter(|(_, kept)| **kept)
            .map(|(located, _)| *located)
            .collect()
    };
    let mut more = rest.len();
    let mut text = render(&chosen(&keep), more);
    for at in rest {
        keep[at] = true;
        let candidate = render(&chosen(&keep), more - 1);
        if candidate.len() > budget {
            keep[at] = false;
            break;
        }
        more -= 1;
        text = candidate;
    }
    Within {
        text,
        kept: chosen(&keep),
        more,
        full,
        budget,
    }
}

/// These nodes as the briefing shows them, then the count of those it does
/// not. Empty when there are none of either.
fn render_nodes(nodes: &[&Located], flags: &[Flag], notes: &Notes, more: usize) -> String {
    if nodes.is_empty() && more == 0 {
        return String::new();
    }
    let mut out = String::from(PREAMBLE);
    let mut scope = None;
    for located in nodes {
        if scope != Some(located.scope) {
            scope = Some(located.scope);
            let _ = write!(out, "\n{}:\n", scope_heading(located.scope));
        }
        let node = &located.node;
        let mut lines = node.body.lines();
        let _ = writeln!(
            out,
            "- [{}] {}",
            node.kind.name(),
            lines.next().unwrap_or("")
        );
        for line in lines {
            if line.trim().is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "  {line}");
            }
        }
        let note: String = notes
            .iter()
            .filter(|(id, _)| *id == node.id)
            .map(|(_, note)| format!(" · {note}"))
            .collect();
        let _ = writeln!(out, "  ({} · as of {}{note})", node.id, node.as_of);
        for flag in flags.iter().filter(|flag| flag.id() == node.id) {
            let _ = writeln!(out, "  ! {}", flag.describe());
        }
    }
    if more > 0 {
        let _ = write!(out, "\n{more} more: `ohjaa list`\n");
    }
    out
}

pub const BLOCK_BEGIN: &str = "<!-- ohjaa:knowledge begin: generated by `ohjaa brief --write`; edit the nodes, not this block -->";
pub const BLOCK_END: &str = "<!-- ohjaa:knowledge end -->";
/// What every generated block's begin line starts with: `brief --write`'s
/// and [`INSTRUCTIONS_BEGIN`] are one block, so a file holds at most one.
const BLOCK_PREFIX: &str = "<!-- ohjaa:knowledge begin";
/// The begin line of the block in each agent's own global instruction file.
pub const INSTRUCTIONS_BEGIN: &str = "<!-- ohjaa:knowledge begin: mine and machine, written by \
     `ohjaa apply` and after each decision, removed by `ohjaa apply --undo`; edit the nodes \
     (`ohjaa list`), not this block -->";
/// What every begin line apply writes starts with, whatever its words after:
/// a block opened otherwise (`brief --write`'s, a person's) is not apply's.
const INSTRUCTIONS_PREFIX: &str = "<!-- ohjaa:knowledge begin: mine and machine";

/// Whose the generated block in a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// No block.
    None,
    /// One apply wrote ([`INSTRUCTIONS_BEGIN`]).
    Applied,
    /// Another: `brief --write`'s, or one a person wrote. It is theirs.
    Other,
}

/// Whose the generated block in `text` is (a broken one is refused).
pub fn held(text: &str) -> Result<Held> {
    Ok(match find_block(text)? {
        None => Held::None,
        Some(range)
            if text[range.clone()]
                .trim_start()
                .starts_with(INSTRUCTIONS_PREFIX) =>
        {
            Held::Applied
        }
        Some(_) => Held::Other,
    })
}

/// The generated block's lines in `text`: from the start of its begin line
/// to the end of its end line (its newline too, when it has one). A marker
/// without its partner, one inside a line, or a second block is refused:
/// guessing where a block ends could delete what a person wrote.
fn find_block(text: &str) -> Result<Option<std::ops::Range<usize>>> {
    let begins = text.matches(BLOCK_PREFIX).count();
    let ends = text.matches(BLOCK_END).count();
    if begins == 0 && ends == 0 {
        return Ok(None);
    }
    let mut start = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let bare = line.trim();
        match start {
            None if bare.starts_with(BLOCK_PREFIX) => start = Some(at),
            Some(start) if bare == BLOCK_END && begins == 1 && ends == 1 => {
                return Ok(Some(start..at + line.len()))
            }
            _ => {}
        }
        at += line.len();
    }
    refuse(
        "the file has an ohjaa knowledge marker without its partner, or more than one block; \
         fix it by hand"
            .to_owned(),
    )
}

/// What went in around a block that was not there before, so that taking it
/// out puts back exactly what was there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    /// A blank line went in before it.
    pub blank: bool,
    /// The text before it ended in a newline (or was empty).
    pub newline: bool,
}

impl Placed {
    /// Nothing added around it: it replaced a block, or filled an empty file.
    pub const IN_PLACE: Placed = Placed {
        blank: false,
        newline: true,
    };
}

/// `existing` with the generated block, opened by `begin` and holding
/// `body`: replaced where it is, else appended after a blank line. Nothing a
/// person wrote, before, after or around it, changes.
pub fn with_block(existing: &str, begin: &str, body: &str) -> Result<(String, Placed)> {
    let block = format!("{begin}\n{}\n{BLOCK_END}\n", body.trim_end());
    Ok(match find_block(existing)? {
        Some(range) => {
            // A block that ends the file without a newline keeps it so.
            let block = if existing[range.clone()].ends_with('\n') {
                block.as_str()
            } else {
                block.trim_end_matches('\n')
            };
            (
                format!(
                    "{}{block}{}",
                    &existing[..range.start],
                    &existing[range.end..]
                ),
                Placed::IN_PLACE,
            )
        }
        None if existing.is_empty() => (block, Placed::IN_PLACE),
        None => {
            let newline = existing.ends_with('\n');
            let mut out = existing.to_owned();
            if !newline {
                out.push('\n');
            }
            let blank = !out.ends_with("\n\n");
            if blank {
                out.push('\n');
            }
            out.push_str(&block);
            (out, Placed { blank, newline })
        }
    })
}

/// `existing` without the generated block; at the end of the file, without
/// what `placed` says went in before it too. `None` when there is none.
pub fn without_block(existing: &str, placed: Placed) -> Result<Option<String>> {
    let Some(range) = find_block(existing)? else {
        return Ok(None);
    };
    let (mut before, after) = (&existing[..range.start], &existing[range.end..]);
    if after.is_empty() {
        if placed.blank && before.ends_with("\n\n") {
            before = &before[..before.len() - 1];
        }
        if !placed.newline && before.ends_with('\n') {
            before = &before[..before.len() - 1];
        }
    }
    Ok(Some(format!("{before}{after}")))
}

/// Replace (or append) the generated block in an instruction file, keeping
/// everything a person wrote around it. A block with a missing or misplaced
/// end marker is refused: guessing where it ends could delete what they wrote.
pub fn write_block(existing: &str, briefing: &str) -> Result<String> {
    with_block(existing, BLOCK_BEGIN, briefing).map(|(text, _)| text)
}

/// What opens the block in an agent's own instruction file.
pub const INSTRUCTIONS_PREAMBLE: &str =
    "Knowledge (ohjaa, mine and machine): what the person you work for has decided and knows. \
     Follow it; if an entry looks wrong or stale, say so instead of quietly working around it. \
     Propose changes with `ohjaa propose`; only a person makes them current.";

/// The knowledge an agent's own instruction file holds, and how much of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compact {
    pub text: String,
    /// The nodes it holds, one line each.
    pub shown: usize,
    /// The nodes it names only by count, being over the budget.
    pub more: usize,
    /// The ids of the nodes it holds, in briefing order.
    pub held: Vec<String>,
}

impl Compact {
    pub fn summary(&self) -> String {
        match self.more {
            0 => format!(
                "{} node{}",
                self.shown,
                if self.shown == 1 { "" } else { "s" }
            ),
            more => format!(
                "{} of {} nodes, the other {more} by count: over the {DEFAULT_BUDGET}-character \
                 budget, merge or retire some",
                self.shown,
                self.shown + more
            ),
        }
    }
}

/// The current knowledge of `scopes`, compactly, in briefing order: one
/// line per node, `kind: text`, no origins. Over `budget` characters, every
/// decision in full and the rest by priority while it fits ([`within`]),
/// then the count of what is left: never a node cut short.
pub fn render_compact(loaded: &Loaded, scopes: &[Scope], budget: usize) -> Compact {
    let current = in_scopes(loaded, scopes);
    if current.is_empty() {
        return Compact {
            text: format!(
                "{INSTRUCTIONS_PREAMBLE}\nNone recorded yet (`ohjaa propose`, then `ohjaa review`).\n"
            ),
            shown: 0,
            more: 0,
            held: Vec::new(),
        };
    }
    let within = within(current, budget, |kept, more| {
        let mut text = format!("{INSTRUCTIONS_PREAMBLE}\n");
        for located in kept {
            let words: Vec<&str> = located.node.body.split_whitespace().collect();
            let _ = writeln!(text, "- {}: {}", located.node.kind.name(), words.join(" "));
        }
        if more > 0 {
            let _ = writeln!(text, "{more} more: `ohjaa list`");
        }
        text
    });
    Compact {
        text: within.text,
        shown: within.kept.len(),
        more: within.more,
        held: within
            .kept
            .iter()
            .map(|located| located.node.id.clone())
            .collect(),
    }
}

// ---- proposing and deciding ------------------------------------------------

/// Only a person may make Knowledge current, retire it or reject it (law 2).
/// Agents run without a terminal; this is the line they do not cross.
pub fn require_person(action: &str) -> Result<()> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Ok(());
    }
    refuse(format!(
        "`ohjaa {action}` changes what every agent will read, so only a person may run it, \
         from an interactive terminal. Agents propose with `ohjaa propose`."
    ))
}

/// A node waiting for a person's decision.
#[derive(Debug, Clone)]
pub struct Pending {
    pub scope: Scope,
    /// For repo proposals: the repository it belongs to.
    pub repo: Option<PathBuf>,
    pub path: PathBuf,
    pub node: Node,
    /// `true` when it waits among the proposals, `false` when a proposed node was
    /// committed into a scope directory (for review in a pull request).
    pub staged: bool,
}

/// Everything awaiting a decision: the proposals, plus proposed nodes already in
/// a scope directory.
pub fn pending(dirs: &Dirs, loaded: &Loaded) -> Result<(Vec<Pending>, Vec<Broken>)> {
    let mut found = Vec::new();
    let mut broken = Vec::new();
    let mut take =
        |scope: Scope, repo: Option<PathBuf>, dir: &Path, found: &mut Vec<Pending>| -> Result<()> {
            for path in markdown_files(dir)? {
                match read_node(&path) {
                    Ok(node) => found.push(Pending {
                        scope,
                        repo: repo.clone(),
                        path,
                        node,
                        staged: true,
                    }),
                    Err(reason) => broken.push(Broken { path, reason }),
                }
            }
            Ok(())
        };
    for scope in [Scope::Ours, Scope::Mine, Scope::Machine] {
        take(scope, None, &dirs.proposals.join(scope.name()), &mut found)?;
    }
    let repo_proposals = dirs.proposals.join(Scope::Repo.name());
    if let Ok(entries) = std::fs::read_dir(&repo_proposals) {
        let mut repos: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .collect();
        repos.sort();
        for dir in repos {
            let name = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            take(Scope::Repo, Some(decode_repo(&name)), &dir, &mut found)?;
        }
    }
    for (located, file) in loaded.nodes.iter().zip(&loaded.files) {
        if located.node.status == Status::Proposed {
            found.push(Pending {
                scope: located.scope,
                repo: (located.scope == Scope::Repo)
                    .then(|| dirs.repo_root.clone())
                    .flatten(),
                path: file.clone(),
                node: located.node.clone(),
                staged: false,
            });
        }
    }
    Ok((found, broken))
}

/// What an agent (or intake) wants to become Knowledge.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub scope: Scope,
    pub repo: Option<PathBuf>,
    pub id: String,
    pub kind: Kind,
    pub body: String,
    pub origin: Vec<String>,
    pub supersedes: Vec<String>,
    pub refines: Option<String>,
    pub holds_while: Vec<String>,
}

/// Write a proposal where it waits for a person. It must be a valid node and must hold
/// together with the Knowledge it names; nothing is written otherwise.
pub fn propose(dirs: &Dirs, loaded: &Loaded, proposal: Proposal, today: Date) -> Result<PathBuf> {
    let mut holds_while = Vec::new();
    for text in &proposal.holds_while {
        holds_while
            .push(Predicate::parse(text).with_context(|| format!("`{text}` is not a predicate"))?);
    }
    let node = Node {
        id: proposal.id.clone(),
        kind: proposal.kind,
        status: Status::Proposed,
        as_of: today,
        origin: proposal.origin,
        supersedes: proposal.supersedes,
        refines: proposal.refines,
        holds_while,
        body: proposal.body.trim().to_owned(),
    };
    // Validate exactly as a reader would: through the canonical text.
    let text = fk::to_text(&node);
    let node = fk::parse_node(&node.id, &text).map_err(|error| anyhow::anyhow!("{error}"))?;

    let repo = match proposal.scope {
        Scope::Repo => Some(
            proposal
                .repo
                .or_else(|| dirs.repo_root.clone())
                .context("a repo proposal must be made inside the repository, or name it")?,
        ),
        _ => None,
    };
    // A proposal for another repository must hold together with *that*
    // repository's Knowledge, not with the one this command runs in.
    let elsewhere;
    let loaded = match &repo {
        Some(repo) if Some(repo) != dirs.repo_root.as_ref() => {
            let mut there = dirs.clone();
            there.repo_root = Some(repo.clone());
            elsewhere = load(&there)?;
            &elsewhere
        }
        _ => loaded,
    };
    let (waiting, _) = pending(dirs, loaded)?;
    if loaded.get(&node.id).is_some() || waiting.iter().any(|p| p.node.id == node.id) {
        return refuse(format!("`{}` already exists; choose another id", node.id));
    }
    // It may build on a proposal still waiting (of its own repository, for a
    // repo one): it is checked with that one beside the Knowledge, and can
    // be accepted once that one is.
    let beside: Vec<Located> = node
        .refines
        .iter()
        .filter(|base| loaded.get(base).is_none())
        .filter_map(|base| {
            waiting
                .iter()
                .find(|p| p.node.id == **base && (p.scope != Scope::Repo || p.repo == repo))
        })
        .map(|p| Located {
            scope: p.scope,
            node: p.node.clone(),
        })
        .collect();
    check_fits(loaded, proposal.scope, &node, &[], &beside)?;

    let dir = dirs.proposal_dir(proposal.scope, repo.as_deref())?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(format!("{}.md", node.id));
    write_atomic(&path, &text)?;
    Ok(path)
}

/// Refuse a change whose result would break a set rule involving `node`.
/// `beside` joins the set unjudged: proposals `node` builds on.
fn check_fits(
    loaded: &Loaded,
    scope: Scope,
    node: &Node,
    replaced: &[Located],
    beside: &[Located],
) -> Result<()> {
    let problems: Vec<String> = misfits(loaded, scope, node, replaced, beside)
        .into_iter()
        .map(|error| error.to_string())
        .collect();
    if !problems.is_empty() {
        return refuse(format!(
            "refused, it would not hold together:\n  {}",
            problems.join("\n  ")
        ));
    }
    Ok(())
}

/// The set rules a change would break that involve `node` or what it
/// replaces ([`check_fits`]).
fn misfits(
    loaded: &Loaded,
    scope: Scope,
    node: &Node,
    replaced: &[Located],
    beside: &[Located],
) -> Vec<fk::SetError> {
    let mut set: Vec<Located> = loaded
        .nodes
        .iter()
        .filter(|located| {
            located.node.id != node.id && !replaced.iter().any(|r| r.node.id == located.node.id)
        })
        .cloned()
        .collect();
    set.extend(replaced.iter().cloned());
    set.extend(beside.iter().cloned());
    set.push(Located {
        scope,
        node: node.clone(),
    });
    let touched: Vec<&str> = std::iter::once(node.id.as_str())
        .chain(replaced.iter().map(|r| r.node.id.as_str()))
        .collect();
    fk::validate_set(&set)
        .into_iter()
        .filter(|error| touched.contains(&error.id.as_str()))
        .collect()
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// What a decision changed, for the person to see and commit.
#[derive(Debug, Default)]
pub struct Outcome {
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Shared checkouts that now hold uncommitted Knowledge to review: the
    /// ours checkout. Repo knowledge goes through a pull request ohjaa opens.
    pub review_in: Vec<PathBuf>,
    /// The node decided on, as it now is (a rejected proposal as it was).
    pub decided: Option<Located>,
    /// For a decision on repo knowledge: the repository it is of.
    pub repo: Option<PathBuf>,
}

impl Outcome {
    /// Whether the decision was on the home's own knowledge (mine, machine),
    /// which the home's history records.
    pub fn in_the_home(&self) -> bool {
        self.decided
            .as_ref()
            .is_some_and(|located| matches!(located.scope, Scope::Mine | Scope::Machine))
    }
}

/// The commit that records a person's decision in the home: what was decided
/// and why. `why` is retire's `--why`.
pub fn decision_message(action: &str, id: &str, outcome: &Outcome, why: Option<&str>) -> String {
    let Some(decided) = &outcome.decided else {
        return format!("{action} {id}");
    };
    let node = &decided.node;
    let title = node.body.lines().next().unwrap_or("").trim();
    let mut message = format!(
        "{action} {id}: {title}\n\n{}, {}",
        node.kind.name(),
        decided.scope.name()
    );
    match action {
        "accept" => {
            let _ = writeln!(message, ": current as of {}", node.as_of);
            if !node.supersedes.is_empty() {
                let _ = writeln!(message, "supersedes: {}", node.supersedes.join(", "));
            }
            for origin in &node.origin {
                let _ = writeln!(message, "why: {origin}");
            }
        }
        "reject" => {
            let _ = writeln!(message, ": the proposal is dropped, never current");
            for origin in &node.origin {
                let _ = writeln!(message, "why it was proposed: {origin}");
            }
        }
        "retire" => {
            let _ = writeln!(message, ": retired, kept with its why");
            let _ = writeln!(message, "why: {}", why.unwrap_or("").trim());
        }
        _ => {
            let _ = writeln!(message, ": still holds, as of {}", node.as_of);
        }
    }
    message.trim_end().to_owned()
}

/// The checkout a decision leaves for the person to commit and review: the
/// ours checkout. Repo knowledge reaches its repository through the pull
/// request ohjaa opens ([`crate::lessons`]), and nothing is left in it.
fn shared_checkout(dirs: &Dirs, scope: Scope) -> Option<PathBuf> {
    match scope {
        Scope::Ours => dirs.ours_checkout.clone(),
        _ => None,
    }
}

fn target_dir(dirs: &Dirs, scope: Scope, repo: Option<&Path>) -> Result<PathBuf> {
    match scope {
        Scope::Repo => Ok(accepted_dir(
            &dirs.state,
            repo.or(dirs.repo_root.as_deref())
                .context("which repository? run inside it")?,
        )),
        Scope::Ours => dirs
            .scope_dir(Scope::Ours)
            .context("no shared knowledge checkout: set OHJAA_OURS or ~/.config/ohjaa/ours"),
        other => Ok(dirs
            .scope_dir(other)
            .expect("personal scopes always have a home")),
    }
}

/// The one pending node with this id. Two proposals sharing an id are refused
/// rather than guessed between.
fn one_pending(dirs: &Dirs, loaded: &Loaded, id: &str) -> Result<Pending> {
    let (waiting, _) = pending(dirs, loaded)?;
    let mut matching = waiting.into_iter().filter(|p| p.node.id == id);
    let first = matching
        .next()
        .with_context(|| format!("nothing named `{id}` is waiting for a decision"))?;
    if matching.next().is_some() {
        return refuse(format!(
            "more than one proposal is named `{id}`; remove all but one by hand first"
        ));
    }
    Ok(first)
}

/// Make a pending node current, as of `today`. Nodes it supersedes become
/// `superseded` in the same step; if the result would not hold, nothing changes.
pub fn accept(dirs: &Dirs, id: &str, today: Date) -> Result<Outcome> {
    let loaded = load(dirs)?;
    let pending = one_pending(dirs, &loaded, id)?;

    // A repo proposal for another repository is decided against that one.
    let loaded = match (&pending.repo, pending.scope) {
        (Some(repo), Scope::Repo) if Some(repo) != dirs.repo_root.as_ref() => {
            let mut there = dirs.clone();
            there.repo_root = Some(repo.clone());
            load(&there)?
        }
        _ => loaded,
    };

    let mut node = pending.node.clone();
    node.status = Status::Current;
    node.as_of = today;
    let (replaced, replaced_files) = replacing(&loaded, &node)?;
    check_fits(&loaded, pending.scope, &node, &replaced, &[])?;

    let dir = target_dir(dirs, pending.scope, pending.repo.as_deref())?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut outcome = Outcome::default();
    let path = dir.join(format!("{}.md", node.id));
    write_atomic(&path, &fk::to_text(&node))?;
    outcome.written.push(path.clone());
    for (old, old_path) in replaced.iter().zip(replaced_files) {
        // A repo node still untracked in the checkout goes into ohjaa's
        // state with its successor: nothing is written into the checkout.
        let moved = (pending.scope == Scope::Repo && old_path.parent() != Some(dir.as_path()))
            .then(|| dir.join(format!("{}.md", old.node.id)));
        let to = moved.clone().unwrap_or_else(|| old_path.clone());
        write_atomic(&to, &fk::to_text(&old.node))?;
        outcome.written.push(to);
        if moved.is_some() {
            std::fs::remove_file(&old_path)?;
            outcome.removed.push(old_path);
        }
    }
    // A proposal waiting among the proposals, or a proposed node in a scope
    // directory that is not where the current one now is.
    if pending.path != path {
        std::fs::remove_file(&pending.path)?;
        outcome.removed.push(pending.path.clone());
    }
    if let Some(checkout) = shared_checkout(dirs, pending.scope) {
        outcome.review_in.push(checkout);
    }
    outcome.repo = (pending.scope == Scope::Repo)
        .then(|| pending.repo.clone().or_else(|| dirs.repo_root.clone()))
        .flatten();
    if let Some(root) = outcome.repo.as_deref() {
        tidy_untracked(root);
    }
    outcome.decided = Some(Located {
        scope: pending.scope,
        node,
    });
    Ok(outcome)
}

/// The nodes accepting `node` supersedes, marked superseded, and their files.
fn replacing(loaded: &Loaded, node: &Node) -> Result<(Vec<Located>, Vec<PathBuf>)> {
    let mut replaced = Vec::new();
    let mut files = Vec::new();
    for target in &node.supersedes {
        let (old, file) = loaded
            .unique(target)?
            .with_context(|| format!("supersedes `{target}`, which does not exist"))?;
        let mut old = old.clone();
        old.node.status = Status::Superseded;
        replaced.push(old);
        files.push(file.to_path_buf());
    }
    Ok((replaced, files))
}

/// Why `pending` cannot be accepted as it stands, judged against the
/// knowledge as it is now exactly as [`accept`] judges it, writing nothing:
/// `None` when it can. A proposal can stop holding together while it waits,
/// when what it names is replaced, retired or moved to another scope.
pub fn cannot_accept(dirs: &Dirs, loaded: &Loaded, pending: &Pending) -> Option<String> {
    let there;
    let loaded = match (&pending.repo, pending.scope) {
        (Some(repo), Scope::Repo) if Some(repo) != dirs.repo_root.as_ref() => {
            let mut elsewhere = dirs.clone();
            elsewhere.repo_root = Some(repo.clone());
            match load(&elsewhere) {
                Ok(loaded) => {
                    there = loaded;
                    &there
                }
                Err(error) => return Some(format!("{error:#}")),
            }
        }
        _ => loaded,
    };
    let mut node = pending.node.clone();
    node.status = Status::Current;
    let replaced = match replacing(loaded, &node) {
        Ok((replaced, _)) => replaced,
        Err(error) => return Some(format!("{error:#}")),
    };
    let problems: Vec<String> = misfits(loaded, pending.scope, &node, &replaced, &[])
        .into_iter()
        .map(|error| error.detail)
        .collect();
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// How many characters `located` takes in a briefing ([`render_nodes`]),
/// its scope's heading not counted.
pub fn briefing_size(located: &Located) -> usize {
    let heading = format!("\n{}:\n", scope_heading(located.scope)).len();
    render_nodes(&[located], &[], &[], 0).len() - PREAMBLE.len() - heading
}

/// How many characters `node` takes in an agent's own instructions
/// ([`render_compact`]): one line.
pub fn compact_size(node: &Node) -> usize {
    let words: Vec<&str> = node.body.split_whitespace().collect();
    format!("- {}: {}\n", node.kind.name(), words.join(" ")).len()
}

/// Drop a proposal that never became Knowledge.
pub fn reject(dirs: &Dirs, id: &str) -> Result<Outcome> {
    let loaded = load(dirs)?;
    let pending = one_pending(dirs, &loaded, id)?;
    std::fs::remove_file(&pending.path)?;
    let mut outcome = Outcome {
        removed: vec![pending.path.clone()],
        decided: Some(Located {
            scope: pending.scope,
            node: pending.node.clone(),
        }),
        ..Outcome::default()
    };
    if !pending.staged {
        if let Some(checkout) = shared_checkout(dirs, pending.scope) {
            outcome.review_in.push(checkout);
        }
    }
    Ok(outcome)
}

/// Change one current node in place: retire it, or re-confirm it as of today.
fn amend(dirs: &Dirs, id: &str, change: impl FnOnce(&mut Node) -> Result<()>) -> Result<Outcome> {
    let loaded = load(dirs)?;
    let (located, path) = loaded
        .unique(id)?
        .with_context(|| format!("no knowledge named `{id}`"))?;
    let (located, path) = (located.clone(), path.to_path_buf());
    if located.node.status != Status::Current {
        bail!("`{id}` is {}, not current", located.node.status.name());
    }
    let mut node = located.node.clone();
    change(&mut node)?;
    let repo = (located.scope == Scope::Repo)
        .then(|| dirs.repo_root.clone())
        .flatten();
    // A repo node still untracked in the checkout goes into ohjaa's state:
    // nothing is written into the checkout.
    let into_state = repo
        .as_deref()
        .map(|root| accepted_dir(&dirs.state, root))
        .filter(|dir| path.parent() != Some(dir.as_path()));
    let (written, removed) = match into_state {
        Some(dir) => {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            let to = dir.join(format!("{id}.md"));
            write_atomic(&to, &fk::to_text(&node))?;
            std::fs::remove_file(&path)?;
            if let Some(root) = repo.as_deref() {
                tidy_untracked(root);
            }
            (vec![to], vec![path])
        }
        None => {
            write_atomic(&path, &fk::to_text(&node))?;
            (vec![path], Vec::new())
        }
    };
    let mut outcome = Outcome {
        written,
        removed,
        repo,
        decided: Some(Located {
            scope: located.scope,
            node,
        }),
        ..Outcome::default()
    };
    if let Some(checkout) = shared_checkout(dirs, located.scope) {
        outcome.review_in.push(checkout);
    }
    Ok(outcome)
}

/// Knowledge that no longer applies and has no successor. Kept, never deleted.
pub fn retire(dirs: &Dirs, id: &str, why: &str, today: Date) -> Result<Outcome> {
    if why.trim().is_empty() {
        bail!("say why it no longer applies: retired knowledge keeps its why");
    }
    amend(dirs, id, |node| {
        node.status = Status::Retired;
        node.origin
            .push(format!("{today}: retired: {}", why.trim()));
        Ok(())
    })
}

/// A person re-confirms that a node still holds.
pub fn confirm(dirs: &Dirs, id: &str, today: Date) -> Result<Outcome> {
    amend(dirs, id, |node| {
        node.as_of = today;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox {
        root: PathBuf,
        dirs: Dirs,
    }

    impl Sandbox {
        fn new(name: &str) -> Sandbox {
            let root = std::env::temp_dir().join(format!("ohjaa-k-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let dirs = Dirs {
                ours_checkout: Some(root.join("ours")),
                repo_root: Some(root.join("repo")),
                mine: root.join("config/mine"),
                machine: root.join("config/machine"),
                proposals: root.join("state/proposals"),
                state: root.join("state"),
            };
            Sandbox { root, dirs }
        }

        fn proposal(&self, scope: Scope, id: &str) -> Proposal {
            Proposal {
                scope,
                repo: None,
                id: id.to_owned(),
                kind: Kind::Decision,
                body: format!("Knowledge {id}."),
                origin: vec!["a test".to_owned()],
                supersedes: Vec::new(),
                refines: None,
                holds_while: Vec::new(),
            }
        }

        fn propose(&self, proposal: Proposal) -> Result<PathBuf> {
            propose(
                &self.dirs,
                &load(&self.dirs).unwrap(),
                proposal,
                date("2026-09-24"),
            )
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn date(text: &str) -> Date {
        parse_date(text).unwrap()
    }

    #[test]
    fn a_proposal_may_refine_one_still_waiting_and_is_accepted_after_it() {
        let sandbox = Sandbox::new("refinewaiting");
        sandbox
            .propose(sandbox.proposal(Scope::Mine, "base"))
            .unwrap();
        let refinement = Proposal {
            refines: Some("base".to_owned()),
            ..sandbox.proposal(Scope::Mine, "base-in-codex")
        };
        let path = sandbox.propose(refinement).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("refines = \"base\""),
            "kept, marked as refining"
        );
        let today = date("2026-09-25");
        let refused = accept(&sandbox.dirs, "base-in-codex", today).unwrap_err();
        assert!(format!("{refused:#}").contains("`base`"), "{refused:#}");
        accept(&sandbox.dirs, "base", today).unwrap();
        accept(&sandbox.dirs, "base-in-codex", today).unwrap();
    }

    #[test]
    fn day_numbers_round_trip_across_eras() {
        assert_eq!(day_number(date("1970-01-01")), 0);
        assert_eq!(
            day_number(date("2026-09-24")) - day_number(date("2026-06-26")),
            90
        );
        for days in [-1, 0, 59, 60, 10_957, 20_720, 30_000] {
            assert_eq!(day_number(date_from_day_number(days)), days);
        }
        assert_eq!(
            date_from_day_number(day_number(date("2024-02-29"))),
            date("2024-02-29")
        );
    }

    #[test]
    fn a_proposal_waits_until_a_person_accepts_it() {
        let sandbox = Sandbox::new("accept");
        let path = sandbox
            .propose(sandbox.proposal(Scope::Ours, "rust-only"))
            .unwrap();
        assert!(path.starts_with(&sandbox.dirs.proposals));
        let loaded = load(&sandbox.dirs).unwrap();
        assert!(loaded.nodes.is_empty(), "a proposal is not knowledge yet");
        assert_eq!(
            render(&loaded, &[]),
            "",
            "and it never reaches the briefing"
        );

        let outcome = accept(&sandbox.dirs, "rust-only", date("2026-09-25")).unwrap();
        assert!(!path.exists(), "accepting removes the waiting proposal");
        assert_eq!(outcome.review_in, vec![sandbox.root.join("ours")]);
        let loaded = load(&sandbox.dirs).unwrap();
        let node = &loaded.get("rust-only").unwrap().node;
        assert_eq!(node.status, Status::Current);
        assert_eq!(
            node.as_of,
            date("2026-09-25"),
            "accepting confirms it as of today"
        );
        assert!(render(&loaded, &[]).contains("Knowledge rust-only."));
    }

    #[test]
    fn accepting_a_successor_supersedes_in_one_step() {
        let sandbox = Sandbox::new("supersede");
        sandbox
            .propose(sandbox.proposal(Scope::Ours, "use-cell"))
            .unwrap();
        accept(&sandbox.dirs, "use-cell", date("2026-09-01")).unwrap();
        let mut successor = sandbox.proposal(Scope::Ours, "use-solu");
        successor.supersedes = vec!["use-cell".to_owned()];
        sandbox.propose(successor).unwrap();
        accept(&sandbox.dirs, "use-solu", date("2026-09-24")).unwrap();

        let loaded = load(&sandbox.dirs).unwrap();
        assert_eq!(
            loaded.get("use-cell").unwrap().node.status,
            Status::Superseded
        );
        assert_eq!(loaded.get("use-solu").unwrap().node.status, Status::Current);
        assert!(fk::validate_set(&loaded.nodes).is_empty());
        let briefing = render(&loaded, &[]);
        assert!(briefing.contains("use-solu") && !briefing.contains("use-cell"));
    }

    #[test]
    fn personal_knowledge_cannot_supersede_shared_knowledge() {
        let sandbox = Sandbox::new("cross");
        sandbox
            .propose(sandbox.proposal(Scope::Ours, "team-rule"))
            .unwrap();
        accept(&sandbox.dirs, "team-rule", date("2026-09-24")).unwrap();
        let mut mine = sandbox.proposal(Scope::Mine, "my-rule");
        mine.supersedes = vec!["team-rule".to_owned()];
        let error = sandbox.propose(mine).unwrap_err().to_string();
        assert!(error.contains("cross-scope-supersedes"), "{error}");
        assert!(!sandbox.dirs.proposals.join("mine/my-rule.md").exists());
    }

    /// A proposal can stop holding together while it waits: here what it
    /// replaces moved from mine to ours (as the team's rules did on
    /// 2026-11-07). That is said before anyone decides, exactly as accepting
    /// it would refuse it.
    #[test]
    fn a_proposal_that_stopped_holding_together_is_said_before_the_decision() {
        let sandbox = Sandbox::new("stuck");
        sandbox
            .propose(sandbox.proposal(Scope::Mine, "old-rule"))
            .unwrap();
        accept(&sandbox.dirs, "old-rule", date("2026-09-01")).unwrap();
        let mut successor = sandbox.proposal(Scope::Mine, "new-rule");
        successor.supersedes = vec!["old-rule".to_owned()];
        sandbox.propose(successor).unwrap();
        let pending_now = || {
            let loaded = load(&sandbox.dirs).unwrap();
            let (waiting, _) = pending(&sandbox.dirs, &loaded).unwrap();
            let one = waiting
                .into_iter()
                .find(|p| p.node.id == "new-rule")
                .unwrap();
            cannot_accept(&sandbox.dirs, &loaded, &one)
        };
        assert_eq!(pending_now(), None, "it holds together as proposed");
        // The node it replaces moves to the team's knowledge.
        let from = target_dir(&sandbox.dirs, Scope::Mine, None)
            .unwrap()
            .join("old-rule.md");
        let to = target_dir(&sandbox.dirs, Scope::Ours, None).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::rename(&from, to.join("old-rule.md")).unwrap();
        assert_eq!(
            pending_now().as_deref(),
            Some("a mine node cannot supersede `old-rule` in ours")
        );
        let refused = accept(&sandbox.dirs, "new-rule", date("2026-09-24"))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("a mine node cannot supersede `old-rule` in ours"),
            "{refused}"
        );
        // What it replaces gone altogether.
        std::fs::remove_file(to.join("old-rule.md")).unwrap();
        assert_eq!(
            pending_now().as_deref(),
            Some("supersedes `old-rule`, which does not exist")
        );
    }

    #[test]
    fn shared_knowledge_cannot_build_on_personal_knowledge() {
        let sandbox = Sandbox::new("refines");
        sandbox
            .propose(sandbox.proposal(Scope::Mine, "my-habit"))
            .unwrap();
        accept(&sandbox.dirs, "my-habit", date("2026-09-24")).unwrap();
        let mut shared = sandbox.proposal(Scope::Repo, "repo-rule");
        shared.refines = Some("my-habit".to_owned());
        let error = sandbox.propose(shared).unwrap_err().to_string();
        assert!(error.contains("refines-narrower-scope"), "{error}");
    }

    #[test]
    fn ids_are_unique_across_proposals_and_every_scope() {
        let sandbox = Sandbox::new("dup");
        sandbox
            .propose(sandbox.proposal(Scope::Mine, "same"))
            .unwrap();
        let error = sandbox
            .propose(sandbox.proposal(Scope::Ours, "same"))
            .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
    }

    #[test]
    fn invalid_proposals_write_nothing() {
        let sandbox = Sandbox::new("invalid");
        let mut bad = sandbox.proposal(Scope::Mine, "Bad Id");
        assert!(sandbox.propose(bad.clone()).is_err());
        bad.id = "fine".to_owned();
        bad.origin = Vec::new();
        assert!(sandbox
            .propose(bad.clone())
            .unwrap_err()
            .to_string()
            .contains("empty-origin"));
        bad.origin = vec!["why".to_owned()];
        bad.holds_while = vec!["path:../escape".to_owned()];
        assert!(sandbox.propose(bad).is_err());
        assert!(
            !sandbox.dirs.proposals.exists()
                || markdown_files(&sandbox.dirs.proposals.join("mine"))
                    .unwrap()
                    .is_empty()
        );
    }

    #[test]
    fn repo_proposals_wait_per_repository_and_land_in_ohjaas_state_not_in_it() {
        let sandbox = Sandbox::new("repo");
        let elsewhere = sandbox.root.join("other repo%x");
        let mut proposal = sandbox.proposal(Scope::Repo, "uploader-owner");
        proposal.repo = Some(elsewhere.clone());
        sandbox.propose(proposal).unwrap();
        let (waiting, _) = pending(&sandbox.dirs, &load(&sandbox.dirs).unwrap()).unwrap();
        assert_eq!(waiting[0].repo.as_deref(), Some(elsewhere.as_path()));

        let outcome = accept(&sandbox.dirs, "uploader-owner", date("2026-09-24")).unwrap();
        assert_eq!(outcome.written.len(), 1, "{:?}", outcome.written);
        let written = &outcome.written[0];
        assert!(
            written.starts_with(sandbox.dirs.state.join("lessons")),
            "kept in ohjaa's state until it reaches the repository's AGENTS.md: {}",
            written.display()
        );
        assert!(written.ends_with("uploader-owner.md"));
        assert!(
            !repo_knowledge(&elsewhere).exists() && !elsewhere.exists(),
            "nothing is written into the repository"
        );
        assert!(
            outcome.review_in.is_empty(),
            "ohjaa opens the pull request itself"
        );
        // That repository's knowledge has it, current.
        let mut there = sandbox.dirs.clone();
        there.repo_root = Some(elsewhere.clone());
        let loaded = load(&there).unwrap();
        assert_eq!(
            loaded.get("uploader-owner").unwrap().node.status,
            Status::Current
        );
    }

    #[test]
    fn retiring_or_confirming_an_untracked_node_takes_it_out_of_the_checkout() {
        let sandbox = Sandbox::new("amend-untracked");
        let root = sandbox.dirs.repo_root.clone().unwrap();
        let untracked = repo_knowledge(&root);
        std::fs::create_dir_all(&untracked).unwrap();
        for id in ["to-retire", "to-confirm"] {
            let node = Node {
                id: id.to_owned(),
                kind: Kind::Fact,
                status: Status::Current,
                as_of: date("2026-09-01"),
                origin: vec!["accepted before".to_owned()],
                supersedes: Vec::new(),
                refines: None,
                holds_while: Vec::new(),
                body: format!("{id}."),
            };
            std::fs::write(untracked.join(format!("{id}.md")), fk::to_text(&node)).unwrap();
        }
        let accepted = accepted_dir(&sandbox.dirs.state, &root);
        let outcome = retire(&sandbox.dirs, "to-retire", "gone", date("2026-09-24")).unwrap();
        assert_eq!(outcome.written, vec![accepted.join("to-retire.md")]);
        assert_eq!(outcome.removed, vec![untracked.join("to-retire.md")]);
        assert!(untracked.is_dir(), "the other is still there");
        let outcome = confirm(&sandbox.dirs, "to-confirm", date("2026-09-24")).unwrap();
        assert_eq!(outcome.written, vec![accepted.join("to-confirm.md")]);
        assert!(
            !root.join(".ohjaa").exists(),
            "nothing of ohjaa's is left in the checkout"
        );
        let loaded = load(&sandbox.dirs).unwrap();
        assert_eq!(
            loaded.get("to-retire").unwrap().node.status,
            Status::Retired
        );
        assert_eq!(
            loaded.get("to-confirm").unwrap().node.as_of,
            date("2026-09-24")
        );
    }

    #[test]
    fn a_successor_takes_the_untracked_node_it_supersedes_out_of_the_checkout() {
        let sandbox = Sandbox::new("supersede-untracked");
        let root = sandbox.dirs.repo_root.clone().unwrap();
        let untracked = repo_knowledge(&root);
        std::fs::create_dir_all(&untracked).unwrap();
        let old = Node {
            id: "old-way".to_owned(),
            kind: Kind::Decision,
            status: Status::Current,
            as_of: date("2026-09-01"),
            origin: vec!["accepted before".to_owned()],
            supersedes: Vec::new(),
            refines: None,
            holds_while: Vec::new(),
            body: "The old way.".to_owned(),
        };
        std::fs::write(untracked.join("old-way.md"), fk::to_text(&old)).unwrap();
        let mut successor = sandbox.proposal(Scope::Repo, "new-way");
        successor.supersedes = vec!["old-way".to_owned()];
        sandbox.propose(successor).unwrap();

        let outcome = accept(&sandbox.dirs, "new-way", date("2026-09-24")).unwrap();
        let accepted = accepted_dir(&sandbox.dirs.state, &root);
        assert!(
            !untracked.join("old-way.md").exists(),
            "out of the checkout"
        );
        assert!(outcome.removed.contains(&untracked.join("old-way.md")));
        assert!(outcome.written.contains(&accepted.join("old-way.md")));
        let loaded = load(&sandbox.dirs).unwrap();
        assert_eq!(
            loaded.get("old-way").unwrap().node.status,
            Status::Superseded
        );
        assert_eq!(loaded.get("new-way").unwrap().node.status, Status::Current);
        assert_eq!(outcome.repo.as_deref(), Some(root.as_path()));
    }

    #[test]
    fn retire_keeps_the_node_and_its_why() {
        let sandbox = Sandbox::new("retire");
        sandbox
            .propose(sandbox.proposal(Scope::Mine, "old-habit"))
            .unwrap();
        accept(&sandbox.dirs, "old-habit", date("2026-09-01")).unwrap();
        assert!(retire(&sandbox.dirs, "old-habit", " ", date("2026-09-24")).is_err());
        retire(
            &sandbox.dirs,
            "old-habit",
            "moved to Lima",
            date("2026-09-24"),
        )
        .unwrap();
        let loaded = load(&sandbox.dirs).unwrap();
        let node = &loaded.get("old-habit").unwrap().node;
        assert_eq!(node.status, Status::Retired);
        assert_eq!(
            node.origin.last().unwrap(),
            "2026-09-24: retired: moved to Lima"
        );
        assert_eq!(render(&loaded, &[]), "");
    }

    #[test]
    fn stale_facts_and_false_predicates_are_flagged_not_hidden() {
        let sandbox = Sandbox::new("flags");
        let mut fact = sandbox.proposal(Scope::Machine, "old-fact");
        fact.kind = Kind::Fact;
        fact.holds_while = vec!["command:definitely-not-a-real-command-xyz".to_owned()];
        sandbox.propose(fact).unwrap();
        accept(&sandbox.dirs, "old-fact", date("2026-01-01")).unwrap();

        let loaded = load(&sandbox.dirs).unwrap();
        let found = flags(&loaded, None, date("2026-09-24"));
        assert_eq!(found.len(), 2, "{found:?}");
        let briefing = render(&loaded, &found);
        assert!(
            briefing.contains("old-fact"),
            "flagged knowledge is still shown"
        );
        assert!(briefing.contains("may no longer hold: command:definitely-not-a-real-command-xyz"));
        assert!(briefing.contains("fact last confirmed 266 days ago"));

        confirm(&sandbox.dirs, "old-fact", date("2026-09-24")).unwrap();
        let loaded = load(&sandbox.dirs).unwrap();
        assert_eq!(flags(&loaded, None, date("2026-09-24")).len(), 1);
    }

    #[test]
    fn broken_files_are_reported_never_skipped_silently() {
        let sandbox = Sandbox::new("broken");
        std::fs::create_dir_all(&sandbox.dirs.mine).unwrap();
        std::fs::write(sandbox.dirs.mine.join("typo.md"), "+++\nid = \"typo\"\n").unwrap();
        let loaded = load(&sandbox.dirs).unwrap();
        assert_eq!(loaded.broken.len(), 1);
        assert!(loaded.broken[0].reason.contains("missing-frontmatter"));
    }

    #[test]
    fn the_generated_block_is_replaced_and_the_rest_is_kept() {
        let first = write_block("# My notes\n\nKeep me.\n", "Knowledge A").unwrap();
        assert!(first.starts_with("# My notes\n\nKeep me.\n\n<!-- ohjaa:knowledge begin"));
        let second = write_block(&format!("{first}\nAfter.\n"), "Knowledge B").unwrap();
        assert!(second.contains("Knowledge B") && !second.contains("Knowledge A"));
        assert!(second.contains("Keep me.") && second.ends_with("After.\n"));
        assert_eq!(second.matches(BLOCK_BEGIN).count(), 1);
    }

    const BODY: &str = "Knowledge (ohjaa): new\n- decision: x\n";

    fn block(body: &str) -> String {
        format!("{INSTRUCTIONS_BEGIN}\n{}\n{BLOCK_END}\n", body.trim_end())
    }

    /// Added, rewritten and taken out: what the person wrote around the
    /// block, whether a final newline was there, comes back exactly.
    #[test]
    fn the_block_goes_in_and_comes_out_leaving_what_a_person_wrote_exactly() {
        for (mine, blank) in [
            ("# Mine\n\nKeep me.\n", true),
            // No final newline.
            ("# Mine\n\nKeep me.", true),
            // Already ending in a blank line: no second one.
            ("# Mine\n\n", false),
            // Only spaces, and one newline.
            ("  \n", true),
            ("", false),
        ] {
            let (added, placed) = with_block(mine, INSTRUCTIONS_BEGIN, "old").unwrap();
            assert_eq!(placed.blank, blank, "{mine:?}");
            assert_eq!(placed.newline, mine.is_empty() || mine.ends_with('\n'));
            assert!(added.starts_with(mine) && added.ends_with(&block("old")));
            let (rewritten, again) = with_block(&added, INSTRUCTIONS_BEGIN, BODY).unwrap();
            assert_eq!(again, Placed::IN_PLACE, "rewritten where it is");
            assert_eq!(rewritten.replace(&block(BODY), &block("old")), added);
            assert_eq!(
                without_block(&rewritten, placed).unwrap().as_deref(),
                Some(mine),
                "{mine:?}"
            );
        }
        assert_eq!(without_block("# Mine\n", Placed::IN_PLACE).unwrap(), None);
    }

    /// Where a person moved it: first, or between their own lines, or at the
    /// very end without a newline. It is rewritten there, and taken out from
    /// there; their lines stay as they are, and what is inside it is ohjaa's.
    #[test]
    fn a_block_a_person_moved_is_rewritten_and_taken_out_where_it_is() {
        let old = block("old\nwhat a person wrote inside the block");
        for (before, after) in [
            ("", "# Mine\nKeep me.\n"),
            ("# Mine\n", "Keep me.\n"),
            ("# Mine\n\n", ""),
        ] {
            let text = format!("{before}{old}{after}");
            let (new, _) = with_block(&text, INSTRUCTIONS_BEGIN, BODY).unwrap();
            assert_eq!(new, format!("{before}{}{after}", block(BODY)));
            assert!(!new.contains("what a person wrote inside"));
            assert_eq!(
                without_block(&new, Placed::IN_PLACE).unwrap(),
                Some(format!("{before}{after}"))
            );
        }
        let last = format!("# Mine\n{}", old.trim_end());
        let (new, _) = with_block(&last, INSTRUCTIONS_BEGIN, BODY).unwrap();
        assert_eq!(new, format!("# Mine\n{}", block(BODY).trim_end()));
        // A block `brief --write` wrote is the same block.
        let briefed = write_block("# Mine\n", "Knowledge A").unwrap();
        let (new, _) = with_block(&briefed, INSTRUCTIONS_BEGIN, BODY).unwrap();
        assert_eq!(new, format!("# Mine\n\n{}", block(BODY)));
    }

    #[test]
    fn a_second_block_or_a_marker_inside_a_line_is_refused() {
        let two = format!("{}{}", block("a"), block("b"));
        let inline = format!("Mine {INSTRUCTIONS_BEGIN}\nx\n{BLOCK_END}\n");
        for text in [two, inline] {
            for error in [
                with_block(&text, INSTRUCTIONS_BEGIN, BODY).unwrap_err(),
                without_block(&text, Placed::IN_PLACE).unwrap_err(),
            ] {
                assert!(error.downcast_ref::<Refused>().is_some(), "{error}");
            }
        }
    }

    #[test]
    fn a_block_is_applys_only_when_apply_opened_it() {
        assert!(INSTRUCTIONS_BEGIN.starts_with(INSTRUCTIONS_PREFIX));
        assert_eq!(held("# Mine\n").unwrap(), Held::None);
        let (applied, _) = with_block("# Mine\n", INSTRUCTIONS_BEGIN, "x").unwrap();
        assert_eq!(held(&applied).unwrap(), Held::Applied);
        let briefed = write_block("# Mine\n", "x").unwrap();
        assert_eq!(held(&briefed).unwrap(), Held::Other);
        assert!(held(&format!("{applied}{applied}")).is_err());
    }

    fn located(scope: Scope, id: &str, kind: Kind, body: &str) -> Located {
        Located {
            scope,
            node: Node {
                id: id.to_owned(),
                kind,
                status: Status::Current,
                as_of: date("2026-10-27"),
                origin: vec!["2026-10-27: a correction".to_owned()],
                supersedes: Vec::new(),
                refines: None,
                holds_while: Vec::new(),
                body: body.to_owned(),
            },
        }
    }

    #[test]
    fn the_compact_block_is_one_line_a_node_and_over_budget_keeps_every_decision() {
        let mut loaded = Loaded::default();
        for located in [
            located(Scope::Machine, "lima", Kind::Fact, "Lima is\nhere."),
            located(
                Scope::Mine,
                "rust",
                Kind::Decision,
                "Rust only.\n\nNo Python.",
            ),
            located(Scope::Mine, "answer", Kind::Taste, "Answer first."),
            located(Scope::Ours, "team", Kind::Decision, "Team rule."),
            located(
                Scope::Machine,
                "box-rule",
                Kind::Decision,
                "Builds in /tmp.",
            ),
        ] {
            loaded.nodes.push(located);
        }
        let mut retired = located(Scope::Mine, "old", Kind::Decision, "Old.");
        retired.node.status = Status::Retired;
        loaded.nodes.push(retired);
        let scopes = [Scope::Mine, Scope::Machine];
        let all = render_compact(&loaded, &scopes, DEFAULT_BUDGET);
        assert_eq!(
            all.text,
            format!(
                "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only. No Python.\n\
                 - taste: Answer first.\n- decision: Builds in /tmp.\n- fact: Lima is here.\n"
            ),
            "briefing order, mine and machine only, no origins"
        );
        assert_eq!((all.shown, all.more), (4, 0));
        // One node over: only the last by priority goes, the fact.
        let tight = render_compact(&loaded, &scopes, all.text.len() - 1);
        assert_eq!(
            tight.text,
            format!(
                "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only. No Python.\n\
                 - taste: Answer first.\n- decision: Builds in /tmp.\n1 more: `ohjaa list`\n"
            )
        );
        assert_eq!((tight.shown, tight.more), (3, 1));
        assert_eq!(
            tight.summary(),
            "3 of 4 nodes, the other 1 by count: over the 8000-character budget, merge or \
             retire some"
        );
        // Decisions alone over it: still every one, in full.
        let none = render_compact(&loaded, &scopes, 10);
        assert!(none.text.contains("Rust only. No Python.") && none.text.contains("/tmp."));
        let empty = render_compact(&Loaded::default(), &scopes, DEFAULT_BUDGET);
        assert!(empty.text.contains("None recorded yet"), "{}", empty.text);
    }

    /// What stderr says of a briefing the budget cut, in each of its cases.
    #[test]
    fn what_the_budget_cut_is_said_as_it_is() {
        let rule = located(
            Scope::Mine,
            "rule",
            Kind::Decision,
            &"Rust only. ".repeat(20),
        );
        let short = located(Scope::Mine, "short", Kind::Decision, "Rust.");
        let taste = located(Scope::Mine, "small", Kind::Taste, "Keep it small.");
        let big = located(Scope::Mine, "big", Kind::Taste, &"Keep it. ".repeat(30));
        let fact = located(Scope::Machine, "lima", Kind::Fact, "Lima is here.");
        let said = |nodes: Vec<&Located>, budget: usize| {
            let within = render_within(nodes, &[], &[], budget);
            let over = within.over(budget);
            (over, within.full, within.text.len())
        };
        // It fits: nothing is said.
        assert_eq!(said(vec![&taste], DEFAULT_BUDGET).0, None);
        // One node over: every decision, the taste, and the fact by count.
        let full = render_scopes(
            &Loaded {
                nodes: vec![rule.clone(), taste.clone(), fact.clone()],
                ..Loaded::default()
            },
            &[],
            &Scope::ALL,
        )
        .len();
        let (over, _, _) = said(vec![&rule, &taste, &fact], full - 1);
        assert_eq!(
            over.unwrap(),
            format!(
                "the knowledge is {full} characters, over the {} an agent will reliably read: \
                 the briefing holds every decision and 1 more, and names the other 1 by count; \
                 merge or retire some (`ohjaa check`)",
                full - 1
            )
        );
        // The decisions fit, the first of the rest does not.
        let (over, full, _) = said(vec![&short, &big, &fact], 450);
        assert_eq!(
            over.unwrap(),
            format!(
                "the knowledge is {full} characters, over the 450 an agent will reliably read: \
                 the briefing holds its decisions alone, and names the other 2 by count; merge \
                 or retire some (`ohjaa check`)"
            )
        );
        // The decisions alone are over it, with more besides.
        let (over, full, len) = said(vec![&rule, &taste, &fact], 100);
        assert_eq!(
            over.unwrap(),
            format!(
                "the knowledge is {full} characters, and its decisions alone are over the 100 an \
                 agent will reliably read: the briefing holds every decision, in {len} \
                 characters, and names the other 2 by count; merge or retire some (`ohjaa check`)"
            )
        );
        // Decisions, and nothing else, over it.
        let (over, full, _) = said(vec![&rule], 100);
        assert_eq!(
            over.unwrap(),
            format!(
                "the knowledge is {full} characters, all of it decisions, over the 100 an agent \
                 will reliably read: the briefing holds every one; merge or retire some \
                 (`ohjaa check`)"
            )
        );
    }

    /// Over the budget, a degrade and not a cliff: every decision in full,
    /// then taste, lesson and fact, each newest first, for as long as they
    /// fit with the count of the rest; then that count.
    #[test]
    fn over_the_budget_the_block_keeps_every_decision_then_taste_lesson_fact_newest_first() {
        let dated = |scope, id, kind, as_of: &str, body| {
            let mut node = located(scope, id, kind, body);
            node.node.as_of = date(as_of);
            node
        };
        let loaded = Loaded {
            nodes: vec![
                dated(
                    Scope::Mine,
                    "rust",
                    Kind::Decision,
                    "2026-10-01",
                    "Rust only.",
                ),
                dated(
                    Scope::Machine,
                    "box-rule",
                    Kind::Decision,
                    "2026-09-01",
                    "Builds in /tmp.",
                ),
                dated(
                    Scope::Mine,
                    "answer",
                    Kind::Taste,
                    "2026-09-25",
                    "Answer first.",
                ),
                dated(
                    Scope::Mine,
                    "small",
                    Kind::Taste,
                    "2026-10-27",
                    "Keep it small.",
                ),
                dated(
                    Scope::Mine,
                    "menu",
                    Kind::Lesson,
                    "2026-10-27",
                    "A menu is stopping.",
                ),
                dated(
                    Scope::Machine,
                    "lima",
                    Kind::Fact,
                    "2026-09-27",
                    "Lima is here.",
                ),
                dated(
                    Scope::Machine,
                    "vm",
                    Kind::Fact,
                    "2026-10-27",
                    "The VM's /tmp empties.",
                ),
            ],
            ..Loaded::default()
        };
        let scopes = [Scope::Mine, Scope::Machine];
        let start = INSTRUCTIONS_PREAMBLE.len() + 1;
        let block = |budget| {
            let compact = render_compact(&loaded, &scopes, budget);
            (compact.text, compact.shown, compact.more)
        };
        // All of it fits: briefing order, as it always was.
        let all = format!(
            "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only.\n- taste: Answer first.\n\
             - taste: Keep it small.\n- lesson: A menu is stopping.\n- decision: Builds in /tmp.\n\
             - fact: Lima is here.\n- fact: The VM's /tmp empties.\n"
        );
        assert_eq!(all.len(), start + 181);
        assert_eq!(block(all.len()), (all.clone(), 7, 0));
        // One node over the budget: the oldest fact goes, and is counted.
        assert_eq!(
            block(all.len() - 1),
            (
                format!(
                    "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only.\n- taste: Answer first.\n\
                     - taste: Keep it small.\n- lesson: A menu is stopping.\n\
                     - decision: Builds in /tmp.\n- fact: The VM's /tmp empties.\n\
                     1 more: `ohjaa list`\n"
                ),
                6,
                1
            )
        );
        // The lesson does not fit: the filling stops there, though the
        // oldest fact, shorter, would have fitted after it.
        assert_eq!(
            block(start + 148),
            (
                format!(
                    "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only.\n- taste: Answer first.\n\
                     - taste: Keep it small.\n- decision: Builds in /tmp.\n3 more: `ohjaa list`\n"
                ),
                4,
                3
            )
        );
        // The newest taste first.
        assert_eq!(
            block(start + 96),
            (
                format!(
                    "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only.\n- taste: Keep it small.\n\
                     - decision: Builds in /tmp.\n4 more: `ohjaa list`\n"
                ),
                3,
                4
            )
        );
        // The decisions alone over it: every one, in full, and the rest counted.
        assert_eq!(
            block(10),
            (
                format!(
                    "{INSTRUCTIONS_PREAMBLE}\n- decision: Rust only.\n\
                     - decision: Builds in /tmp.\n5 more: `ohjaa list`\n"
                ),
                2,
                5
            )
        );
    }

    #[test]
    fn each_decision_says_what_and_why() {
        let sandbox = Sandbox::new("messages");
        let mut proposal = sandbox.proposal(Scope::Mine, "code-is-rust");
        proposal.body = "Code is Rust or Elixir.\n\nNever Python.".to_owned();
        proposal.origin = vec!["2026-09-16: 'we dont do python'".to_owned()];
        sandbox.propose(proposal).unwrap();
        let accepted = accept(&sandbox.dirs, "code-is-rust", date("2026-10-27")).unwrap();
        assert!(accepted.in_the_home());
        assert_eq!(
            decision_message("accept", "code-is-rust", &accepted, None),
            "accept code-is-rust: Code is Rust or Elixir.\n\n\
             decision, mine: current as of 2026-10-27\n\
             why: 2026-09-16: 'we dont do python'"
        );
        let retired = retire(
            &sandbox.dirs,
            "code-is-rust",
            "moved to ours",
            date("2026-10-28"),
        )
        .unwrap();
        assert_eq!(
            decision_message("retire", "code-is-rust", &retired, Some(" moved to ours ")),
            "retire code-is-rust: Code is Rust or Elixir.\n\n\
             decision, mine: retired, kept with its why\nwhy: moved to ours"
        );
        sandbox
            .propose(sandbox.proposal(Scope::Machine, "lima"))
            .unwrap();
        let rejected = reject(&sandbox.dirs, "lima").unwrap();
        assert_eq!(
            decision_message("reject", "lima", &rejected, None),
            "reject lima: Knowledge lima.\n\n\
             decision, machine: the proposal is dropped, never current\n\
             why it was proposed: a test"
        );
        sandbox
            .propose(sandbox.proposal(Scope::Ours, "team"))
            .unwrap();
        let shared = accept(&sandbox.dirs, "team", date("2026-10-27")).unwrap();
        assert!(
            !shared.in_the_home(),
            "shared knowledge is reviewed elsewhere"
        );
    }

    #[test]
    fn a_half_written_block_is_refused_not_guessed() {
        let broken = format!("# Mine\n{BLOCK_BEGIN}\nold\nMy own words after it.\n");
        let error = write_block(&broken, "new").unwrap_err();
        assert!(error.downcast_ref::<Refused>().is_some(), "{error}");
        let reversed = format!("{BLOCK_END}\n{BLOCK_BEGIN}\n");
        assert!(write_block(&reversed, "new").is_err());
    }

    #[test]
    fn an_id_in_two_scopes_is_never_changed_by_guessing() {
        let sandbox = Sandbox::new("twoscopes");
        sandbox
            .propose(sandbox.proposal(Scope::Ours, "shared-x"))
            .unwrap();
        accept(&sandbox.dirs, "shared-x", date("2026-09-24")).unwrap();
        // A proposed node with the same id, committed into the repo scope.
        let repo_dir = repo_knowledge(sandbox.dirs.repo_root.as_ref().unwrap());
        std::fs::create_dir_all(&repo_dir).unwrap();
        let mut proposed = load(&sandbox.dirs)
            .unwrap()
            .get("shared-x")
            .unwrap()
            .node
            .clone();
        proposed.status = Status::Proposed;
        std::fs::write(repo_dir.join("shared-x.md"), fk::to_text(&proposed)).unwrap();

        let ours_file = sandbox
            .dirs
            .scope_dir(Scope::Ours)
            .unwrap()
            .join("shared-x.md");
        let error = retire(&sandbox.dirs, "shared-x", "why", date("2026-09-24")).unwrap_err();
        assert!(error.downcast_ref::<Refused>().is_some(), "{error}");
        // The committed proposal is the one pending, by its own file...
        let loaded = load(&sandbox.dirs).unwrap();
        let (waiting, _) = pending(&sandbox.dirs, &loaded).unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].path, repo_dir.join("shared-x.md"));
        // ...so rejecting it removes exactly that file, never the team's node.
        let outcome = reject(&sandbox.dirs, "shared-x").unwrap();
        assert_eq!(outcome.removed, vec![repo_dir.join("shared-x.md")]);
        assert!(
            ours_file.is_file(),
            "the team's current knowledge is untouched"
        );
    }

    #[test]
    fn files_read_by_others_get_only_shared_knowledge() {
        let sandbox = Sandbox::new("shared-only");
        for (scope, id) in [
            (Scope::Ours, "team"),
            (Scope::Mine, "personal"),
            (Scope::Machine, "box"),
        ] {
            sandbox.propose(sandbox.proposal(scope, id)).unwrap();
            accept(&sandbox.dirs, id, date("2026-09-24")).unwrap();
        }
        let loaded = load(&sandbox.dirs).unwrap();
        let shared = render_scopes(&loaded, &[], &[Scope::Ours, Scope::Repo]);
        assert!(shared.contains("Knowledge team."));
        assert!(!shared.contains("Knowledge personal.") && !shared.contains("Knowledge box."));
        let everything = render(&loaded, &[]);
        assert!(
            everything.contains("Knowledge personal.") && everything.contains("Knowledge box.")
        );
    }

    #[test]
    fn repo_paths_encode_to_one_directory_and_back() {
        for path in [
            "/Users/x/projects/selko",
            "/tmp/a%b/c",
            "/with space/x",
            "/a/25b/%2F",
        ] {
            let encoded = encode_repo(Path::new(path));
            assert!(!encoded.contains('/'));
            assert_eq!(decode_repo(&encoded), PathBuf::from(path));
        }
    }
}
