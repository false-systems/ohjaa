//! The steering graph: what the people an agent works for know and have
//! decided (faija-ontology.v2, in the knowledge-node.v1 format).
//!
//! Toimija reports facts and refuses to decide. This is the other half: the
//! decisions, taste and lessons that steer an agent *before* it acts. Every
//! node carries its time, its why and whose it is; the scope is the directory
//! it lives in.
//!
//! | scope   | lives in                          | changes by        |
//! |---------|-----------------------------------|-------------------|
//! | ours    | `<ours checkout>/knowledge/`      | review (PR)       |
//! | repo    | `<repo>/.ohjaa/knowledge/`        | review (PR)       |
//! | mine    | `$XDG_CONFIG_HOME/ohjaa/mine/`    | its owner         |
//! | machine | `$XDG_CONFIG_HOME/ohjaa/machine/` | its owner         |
//!
//! Proposals wait in `$XDG_STATE_HOME/ohjaa/proposals/` until a person accepts
//! them, so an agent's draft never lands in a shared repository by itself.

use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use faija_knowledge::{self as fk, Date, Kind, Located, Node, Predicate, Scope, Status};

/// A fact not re-confirmed for this long is flagged: facts expire (law 6).
pub const FACT_TTL_DAYS: i64 = 90;

/// The briefing is read before every action; past this size it stops being
/// read. Going over is a signal to merge and prune, never to truncate.
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

pub fn repo_knowledge(root: &Path) -> PathBuf {
    root.join(".ohjaa").join("knowledge")
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

pub fn load(dirs: &Dirs) -> Result<Loaded> {
    let mut loaded = Loaded::default();
    for scope in Scope::ALL {
        let Some(dir) = dirs.scope_dir(scope) else {
            continue;
        };
        for path in markdown_files(&dir)? {
            match read_node(&path) {
                Ok(node) => {
                    loaded.nodes.push(Located { scope, node });
                    loaded.files.push(path);
                }
                Err(reason) => loaded.broken.push(Broken { path, reason }),
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

fn on_path(name: &str) -> bool {
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

/// The briefing limited to some scopes: a file other people read gets only
/// shared Knowledge, never one person's `mine` or `machine`.
pub fn render_scopes(loaded: &Loaded, flags: &[Flag], scopes: &[Scope]) -> String {
    let current: Vec<&Located> = fk::briefing(&loaded.nodes)
        .into_iter()
        .filter(|located| scopes.contains(&located.scope))
        .collect();
    if current.is_empty() {
        return String::new();
    }
    let mut out = String::from(PREAMBLE);
    let mut scope = None;
    for located in current {
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
        let _ = writeln!(out, "  ({} · as of {})", node.id, node.as_of);
        for flag in flags.iter().filter(|flag| flag.id() == node.id) {
            let _ = writeln!(out, "  ! {}", flag.describe());
        }
    }
    out
}

pub const BLOCK_BEGIN: &str = "<!-- ohjaa:knowledge begin: generated by `ohjaa brief --write`; edit the nodes, not this block -->";
pub const BLOCK_END: &str = "<!-- ohjaa:knowledge end -->";

/// Replace (or append) the generated block in an instruction file, keeping
/// everything a person wrote around it. A block with a missing or misplaced
/// end marker is refused: guessing where it ends could delete what they wrote.
pub fn write_block(existing: &str, briefing: &str) -> Result<String> {
    let block = format!("{BLOCK_BEGIN}\n{}\n{BLOCK_END}\n", briefing.trim_end());
    Ok(
        match (existing.find(BLOCK_BEGIN), existing.find(BLOCK_END)) {
            (Some(start), Some(end)) if end > start => {
                let after = &existing[end + BLOCK_END.len()..];
                let after = after.strip_prefix('\n').unwrap_or(after);
                format!("{}{block}{after}", &existing[..start])
            }
            (None, None) if existing.trim().is_empty() => block,
            (None, None) => format!("{}\n\n{block}", existing.trim_end()),
            _ => {
                return refuse(
                    "the file has an ohjaa knowledge marker without its partner; fix it by hand"
                        .to_owned(),
                )
            }
        },
    )
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
    let problems: Vec<String> = fk::validate_set(&set)
        .into_iter()
        .filter(|error| touched.contains(&error.id.as_str()))
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
    /// Shared checkouts that now hold uncommitted Knowledge to review.
    pub review_in: Vec<PathBuf>,
}

fn shared_checkout(dirs: &Dirs, scope: Scope, repo: Option<&Path>) -> Option<PathBuf> {
    match scope {
        Scope::Ours => dirs.ours_checkout.clone(),
        Scope::Repo => repo
            .map(Path::to_path_buf)
            .or_else(|| dirs.repo_root.clone()),
        _ => None,
    }
}

fn target_dir(dirs: &Dirs, scope: Scope, repo: Option<&Path>) -> Result<PathBuf> {
    match scope {
        Scope::Repo => Ok(repo_knowledge(
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
    let mut replaced = Vec::new();
    let mut replaced_files = Vec::new();
    for target in &node.supersedes {
        let (old, file) = loaded
            .unique(target)?
            .with_context(|| format!("supersedes `{target}`, which does not exist"))?;
        let mut old = old.clone();
        old.node.status = Status::Superseded;
        replaced.push(old);
        replaced_files.push(file.to_path_buf());
    }
    check_fits(&loaded, pending.scope, &node, &replaced, &[])?;

    let dir = target_dir(dirs, pending.scope, pending.repo.as_deref())?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let mut outcome = Outcome::default();
    let path = dir.join(format!("{}.md", node.id));
    write_atomic(&path, &fk::to_text(&node))?;
    outcome.written.push(path.clone());
    for (old, old_path) in replaced.iter().zip(replaced_files) {
        write_atomic(&old_path, &fk::to_text(&old.node))?;
        outcome.written.push(old_path);
    }
    if pending.staged && pending.path != path {
        std::fs::remove_file(&pending.path)?;
        outcome.removed.push(pending.path.clone());
    }
    if let Some(checkout) = shared_checkout(dirs, pending.scope, pending.repo.as_deref()) {
        outcome.review_in.push(checkout);
    }
    Ok(outcome)
}

/// Drop a proposal that never became Knowledge.
pub fn reject(dirs: &Dirs, id: &str) -> Result<Outcome> {
    let loaded = load(dirs)?;
    let pending = one_pending(dirs, &loaded, id)?;
    std::fs::remove_file(&pending.path)?;
    let mut outcome = Outcome {
        removed: vec![pending.path.clone()],
        ..Outcome::default()
    };
    if !pending.staged {
        if let Some(checkout) = shared_checkout(dirs, pending.scope, pending.repo.as_deref()) {
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
    write_atomic(&path, &fk::to_text(&node))?;
    let mut outcome = Outcome {
        written: vec![path],
        ..Outcome::default()
    };
    if let Some(checkout) = shared_checkout(dirs, located.scope, None) {
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
    fn repo_proposals_wait_per_repository_and_land_in_it() {
        let sandbox = Sandbox::new("repo");
        let elsewhere = sandbox.root.join("other repo%x");
        let mut proposal = sandbox.proposal(Scope::Repo, "uploader-owner");
        proposal.repo = Some(elsewhere.clone());
        sandbox.propose(proposal).unwrap();
        let (waiting, _) = pending(&sandbox.dirs, &load(&sandbox.dirs).unwrap()).unwrap();
        assert_eq!(waiting[0].repo.as_deref(), Some(elsewhere.as_path()));

        let outcome = accept(&sandbox.dirs, "uploader-owner", date("2026-09-24")).unwrap();
        assert_eq!(
            outcome.written,
            vec![repo_knowledge(&elsewhere).join("uploader-owner.md")]
        );
        assert_eq!(outcome.review_in, vec![elsewhere]);
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
