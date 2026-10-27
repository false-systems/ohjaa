//! Repository knowledge's home: the repository's own AGENTS.md.
//!
//! Every agent reads the repository's AGENTS.md, so that is where repo
//! knowledge goes: a `## Lessons` section, one line per node, each marked
//! with its id ([`line`]), written through a pull request ohjaa opens when a
//! person accepts a repo proposal (and `ohjaa lessons publish` opens for the
//! nodes accepted before, still in `<repo>/.ohjaa/knowledge/`). The person
//! merges it like any change. Once a line is on the checked-out commit the
//! briefing leaves that node to AGENTS.md; until then it carries it, marked
//! ([`Standing`]).
//!
//! ohjaa's state keeps each accepted node, with its why, and where it went:
//! `$XDG_STATE_HOME/ohjaa/lessons/<clone>/` holds `<id>.md` and
//! `delivery.json` (`ohjaa-lessons.v1`, [`Record`]); `<clone>` is the
//! clone's common git directory, so every worktree of it shares them.
//!
//! The lines are written in a fresh worktree of the repository off its
//! origin's branch, kept in `lessons/.work/` and removed when done, never in
//! the person's checkout. What ohjaa writes into a repository names no tool:
//! not its marks, its words around the lines, its branch, its commit or its
//! pull request, since a repository may be public and ohjaa is not. A
//! repository that is not private gets nothing that names what is: all it
//! would be sent is first checked against a [`DenyList`] built from the
//! person's private context. A line that names something on it is held back
//! and said, with what it named; its text goes nowhere. Anything else that
//! does stops the delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;
use faija_knowledge::{self as fk, Date, Located, Node, Scope, Status};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::{self, Dirs, Loaded, Refused};
use crate::{gitconfig, home, process};

mod spelling;

/// The heading of the section ohjaa writes in AGENTS.md.
pub const HEADING: &str = "## Lessons";
/// What a line of the section begins with, after its `- `: the node's id
/// follows.
const MARK: &str = "<!-- lesson: ";
const MARK_END: &str = " -->";
/// What opens the section when ohjaa makes it.
pub const INTRO: &str = "<!-- This repository's accepted knowledge: one line per node, each \
     marked with its id, written here through a pull request when a person accepts one. A line \
     changed here by hand is replaced the next time it is written: change a lesson where it is \
     decided. -->";
/// The branches of the lessons pull requests begin with this.
pub const BRANCH_PREFIX: &str = "agents-md-lessons/";
pub const SCHEMA: &str = "ohjaa-lessons.v1";
const TITLE: &str = "AGENTS.md: accepted lessons";
/// This tool's name: private, so never sent to a repository that is not.
const TOOL: &str = "ohjaa";

const GH: Duration = Duration::from_secs(30);
const GH_CREATE: Duration = Duration::from_secs(60);
const FETCH: Duration = Duration::from_secs(120);
/// A commit and a push run the repository's own hooks, never skipped: in a
/// fresh worktree a pre-commit that runs clippy on the workspace (tapio's)
/// builds it first.
const HOOKS: Duration = Duration::from_secs(900);
/// How long a delivery waits for another ohjaa's to the same state.
const LOCK_WAIT: Duration = Duration::from_secs(120);

// ---- the section ------------------------------------------------------------

/// A node as its line in the section: its kind, its body on one line and
/// the date it is as of, marked with its id.
pub fn line(node: &Node) -> String {
    let words: Vec<&str> = node.body.split_whitespace().collect();
    format!(
        "- {MARK}{}{MARK_END} **{}**: {} (as of {})",
        node.id,
        node.kind.name(),
        words.join(" "),
        node.as_of
    )
}

/// The id a line of ohjaa's marks; `None` for any other line.
fn marked(line: &str) -> Option<&str> {
    let rest = line
        .trim()
        .strip_prefix('-')?
        .trim_start()
        .strip_prefix(MARK)?;
    let (id, _) = rest.split_once(MARK_END)?;
    fk::is_valid_id(id).then_some(id)
}

/// A marked line as a file holds it: trimmed, and the byte offset its end
/// is at in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub text: String,
    pub end: usize,
}

/// Every marked line in `text` that a reader reads as text, by the id it
/// marks: wherever it is, but inside a fenced block or an HTML comment
/// (where an agent reads it as code, or not at all).
pub fn lines_in(text: &str) -> BTreeMap<String, Vec<Held>> {
    let mut found: BTreeMap<String, Vec<Held>> = BTreeMap::new();
    let (mut fence, mut comment) = (false, false);
    let mut end = 0;
    for raw in text.split_inclusive('\n') {
        end += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        if comment {
            comment =
                !line.contains("-->") || opens_comment(&line[line.rfind("-->").unwrap_or(0)..]);
            continue;
        }
        let bare = line.trim_start();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if let Some(id) = marked(line) {
            found.entry(id.to_owned()).or_default().push(Held {
                text: line.trim().to_owned(),
                end: end - (raw.len() - line.len()),
            });
        }
        comment = opens_comment(line);
    }
    found
}

/// Whether `line` opens an HTML comment it does not close.
fn opens_comment(line: &str) -> bool {
    line.rfind("<!--")
        .is_some_and(|at| !line[at + 4..].contains("-->"))
}

/// What [`with_lessons`] changed, by id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl Changes {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    /// The ids whose text is new in the file.
    fn sent(&self) -> impl Iterator<Item = &String> {
        self.added.iter().chain(&self.changed)
    }
}

/// Where the section is among `lines`: its heading's index and the index
/// of the first line after it (the next heading of level one or two, outside
/// a fenced block, else the end).
fn section(lines: &[(String, &str)]) -> Option<(usize, usize)> {
    let mut fence = false;
    let mut start = None;
    for (at, (text, _)) in lines.iter().enumerate() {
        let bare = text.trim_start();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let heading =
            bare == "#" || bare == "##" || bare.starts_with("# ") || bare.starts_with("## ");
        match start {
            None if text.trim_end().eq_ignore_ascii_case(HEADING) => start = Some(at),
            Some(start) if heading => return Some((start, at)),
            _ => {}
        }
    }
    start.map(|start| (start, lines.len()))
}

/// `existing` (an AGENTS.md; empty when there is none) whose lines of
/// ohjaa's are `lines` (id, line, in order) and none of `gone` (ids ohjaa
/// knows are no longer current). A line of ohjaa's is replaced where it is,
/// and a second one of the same id dropped; the ones not there yet go into
/// the Lessons section, after the last of ohjaa's there, else after what it
/// holds, and the section is added at the end when there is none. Every
/// other line, the person's and a line of an id ohjaa does not know, stays
/// as it is, with its own line ending. Running it on its own result changes
/// nothing.
pub fn with_lessons(
    existing: &str,
    lines: &[(String, String)],
    gone: &[String],
) -> (String, Changes) {
    let eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let wanted: BTreeMap<&str, &str> = lines
        .iter()
        .map(|(id, line)| (id.as_str(), line.as_str()))
        .collect();
    let mut changes = Changes::default();
    let mut found: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<(String, &str)> = Vec::new();
    for raw in existing.split_inclusive('\n') {
        let (text, end) = match raw.strip_suffix("\r\n") {
            Some(text) => (text, "\r\n"),
            None => match raw.strip_suffix('\n') {
                Some(text) => (text, "\n"),
                None => (raw, ""),
            },
        };
        match marked(text) {
            Some(id) => match wanted.get_key_value(id) {
                Some((&id, &line)) if found.insert(id) => {
                    if line != text {
                        changes.changed.push(id.to_owned());
                    }
                    out.push((line.to_owned(), end));
                }
                // A second line of the same id.
                Some((&id, _)) => {
                    if !changes.removed.iter().any(|r| r == id) {
                        changes.removed.push(id.to_owned());
                    }
                }
                None if gone.iter().any(|g| g == id) => changes.removed.push(id.to_owned()),
                None => out.push((text.to_owned(), end)),
            },
            None => out.push((text.to_owned(), end)),
        }
    }
    let missing: Vec<&(String, String)> = lines
        .iter()
        .filter(|(id, _)| !found.contains(id.as_str()))
        .collect();
    if !missing.is_empty() {
        // A last line without its ending gets one: something follows it now.
        if let Some(last) = out.last_mut() {
            if last.1.is_empty() {
                last.1 = eol;
            }
        }
        let new = |text: &str| (text.to_owned(), eol);
        match section(&out) {
            Some((start, end)) => {
                let after = (start + 1..end)
                    .rev()
                    .find(|&at| marked(&out[at].0).is_some())
                    .or_else(|| (start..end).rev().find(|&at| !out[at].0.trim().is_empty()))
                    .unwrap_or(start);
                let at = after + 1;
                let mut insert = Vec::new();
                if after == start {
                    insert.push(new(""));
                }
                insert.extend(missing.iter().map(|(_, line)| new(line)));
                if out.get(at).is_some_and(|(text, _)| !text.trim().is_empty()) {
                    insert.push(new(""));
                }
                out.splice(at..at, insert);
            }
            None => {
                if out.last().is_some_and(|(text, _)| !text.trim().is_empty()) {
                    out.push(new(""));
                }
                out.push(new(HEADING));
                out.push(new(""));
                out.push(new(INTRO));
                out.push(new(""));
                out.extend(missing.iter().map(|(_, line)| new(line)));
            }
        }
        changes.added = missing.iter().map(|(id, _)| id.clone()).collect();
    }
    let text = out
        .iter()
        .map(|(text, end)| format!("{text}{end}"))
        .collect();
    (text, changes)
}

// ---- the deny list ----------------------------------------------------------

/// What the person keeps private: the names of their private repositories,
/// as GitHub lists them to gh, and of those under their roots that gh does
/// not show as public; and their login and name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Private {
    pub repos: Vec<String>,
    pub people: Vec<String>,
    /// Names of repositories under the roots whose visibility is not known
    /// to be public (no GitHub remote, or gh does not show it).
    pub unlisted: Vec<String>,
}

/// The repositories GitHub lists to gh: the names of those not public, the
/// names of the public ones, and every one's `owner/name`, all lower case
/// but `private`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub private: Vec<String>,
    pub public: BTreeSet<String>,
    pub listed: BTreeSet<String>,
}

/// Every repository the person can see, as GitHub's REST API lists them to
/// gh: their own, their organisations', and those they collaborate on, every
/// page (`--paginate`); [`parse_repos`] keeps those that are not public, so
/// an organisation's internal ones count as private. GraphQL's
/// `viewer.repositories` leaves out most of an organisation's.
const PRIVATE_REPOS: &str =
    "user/repos?affiliation=owner,collaborator,organization_member&per_page=100";

fn add_once(list: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() && !list.iter().any(|known| known == text) {
        list.push(text.to_owned());
    }
}

/// The person as gh is signed in (`gh api user`): their login, their name,
/// and each word of it of three letters or more.
pub fn parse_user(output: &[u8]) -> Result<Vec<String>, String> {
    let user: Value = serde_json::from_slice(output)
        .map_err(|error| format!("gh api user printed no answer: {error}"))?;
    let login = user["login"]
        .as_str()
        .filter(|login| !login.is_empty())
        .ok_or("gh api user said no login")?;
    let mut people = Vec::new();
    add_once(&mut people, login);
    if let Some(name) = user["name"].as_str() {
        add_once(&mut people, name);
        for part in name
            .split_whitespace()
            .filter(|part| part.chars().count() >= 3)
        {
            add_once(&mut people, part);
        }
    }
    Ok(people)
}

/// The repositories `gh api user/repos?… --paginate` printed (one JSON
/// array, or one per page).
pub fn parse_repos(output: &[u8]) -> Result<Listing, String> {
    let mut listing = Listing::default();
    let mut pages = 0;
    for page in serde_json::Deserializer::from_slice(output).into_iter::<Value>() {
        let page = page.map_err(|error| format!("gh printed no answer GitHub gave: {error}"))?;
        let Some(repos) = page.as_array() else {
            return Err(format!(
                "GitHub answered without the repositories: {}",
                page["message"].as_str().unwrap_or("no list")
            ));
        };
        pages += 1;
        for repo in repos {
            let name = repo["name"]
                .as_str()
                .ok_or("GitHub listed a repository without its name")?;
            // Public only when GitHub says so: private, internal, or not
            // said, it counts as private.
            let public = match repo["visibility"].as_str() {
                Some(visibility) => visibility.eq_ignore_ascii_case("public"),
                None => repo["private"].as_bool() == Some(false),
            };
            if public {
                listing.public.insert(name.to_lowercase());
            } else {
                add_once(&mut listing.private, name);
            }
            if let Some(full) = repo["full_name"].as_str() {
                listing.listed.insert(full.to_lowercase());
            }
        }
    }
    if pages == 0 {
        return Err("gh printed nothing".to_owned());
    }
    Ok(listing)
}

/// Everything a line that goes to a repository that is not private may not
/// name.
#[derive(Debug, Clone, Default)]
pub struct DenyList {
    entries: Vec<Deny>,
    /// Words a name may begin but that are no secret where the lines go:
    /// the repository's own name and its owner, folded to lower case.
    allowed: Vec<String>,
}

#[derive(Debug, Clone)]
struct Deny {
    text: String,
    what: &'static str,
    /// Matched as a word (not inside a longer run of letters and digits);
    /// otherwise anywhere.
    word: bool,
}

impl DenyList {
    /// For the repository `owner/name` the lines go to: the names of the
    /// person's private repositories, their home path, their home folder's
    /// name (their account), and their login and name. What that
    /// repository's own URL says is no secret there: its name, and its owner
    /// (the person's login, when it is theirs).
    pub fn build(private: &Private, home: Option<&Path>, slug: &str) -> DenyList {
        let (owner, own) = slug.split_once('/').unwrap_or(("", slug));
        let mut list = DenyList {
            allowed: [own, owner]
                .iter()
                .filter(|word| !word.is_empty())
                .map(|word| word.to_lowercase())
                .collect(),
            ..DenyList::default()
        };
        // This tool is private, whatever gh lists: never named, even inside a
        // word (`.ohjaa/`, `OhjaaCli`), but in its own repository.
        if !own.eq_ignore_ascii_case(TOOL) {
            list.add(TOOL, "the name of this tool, which is private", false);
        }
        for name in &private.repos {
            if !name.eq_ignore_ascii_case(own) {
                list.add(name, "the name of a private repository of yours", true);
            }
        }
        for name in &private.unlisted {
            if !name.eq_ignore_ascii_case(own) {
                list.add(
                    name,
                    "the name of a repository under your roots that gh does not show as public",
                    true,
                );
            }
        }
        let account = home
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
        if let Some(home) = home {
            list.add(&home.to_string_lossy(), "your home path", false);
        }
        let people = account
            .iter()
            .map(|account| (account, "your account's name"))
            .chain(
                private
                    .people
                    .iter()
                    .map(|person| (person, "you, as GitHub names you")),
            );
        for (person, what) in people {
            if !person.eq_ignore_ascii_case(owner) {
                list.add(person, what, true);
            }
        }
        list
    }

    fn add(&mut self, text: &str, what: &'static str, word: bool) {
        let text = text.trim();
        if !text.is_empty()
            && !self
                .entries
                .iter()
                .any(|entry| entry.text.eq_ignore_ascii_case(text))
        {
            self.entries.push(Deny {
                text: text.to_owned(),
                what,
                word,
            });
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// What of the list `text` names, each as "`entry` (what it is)", read
    /// every way [`spelling::readings`] reads it: letter case, diacritics,
    /// invisible characters, entities, comments and emphasis are seen
    /// through, and a name is matched at the start of a word, whatever
    /// follows. A letter outside ASCII left after that, or an entity not
    /// read, is said too: it could stand for a letter of a name.
    pub fn matches(&self, text: &str) -> Vec<String> {
        let readings = spelling::readings(text);
        let mut found: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| {
                readings.iter().any(|reading| {
                    if entry.word {
                        spelling::names_word(reading, &entry.text, &self.allowed)
                    } else {
                        spelling::names_anywhere(reading, &entry.text)
                    }
                })
            })
            .map(|entry| format!("`{}` ({})", entry.text, entry.what))
            .collect();
        if let Some(odd) = spelling::odd_letter(&readings[0]) {
            found.push(format!(
                "`{odd}` (a letter outside ASCII, which could stand for one inside it)"
            ));
        }
        if let Some(entity) = spelling::unread_entity(&readings[0]) {
            found.push(format!(
                "`{entity}` (an HTML entity ohjaa does not read, which could stand for a letter)"
            ));
        }
        found
    }
}

// ---- the record ---------------------------------------------------------------

/// Where each accepted node of a clone went, in `delivery.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: String,
    /// A checkout of the clone, where git is run to deliver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// The repository on GitHub, `owner/name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Per node id, its delivery as last known.
    #[serde(default)]
    pub nodes: BTreeMap<String, Delivery>,
    /// Per node id, the untracked file it was moved from, out of the
    /// checkout, and the pull request it went into.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub moved: BTreeMap<String, Moved>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// The day it was last delivered or tried.
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Its line is in an open pull request (`url`).
    Pr,
    /// Its line was on the repository's default branch when ohjaa last looked.
    Merged,
    /// Not delivered (`why`): gh, the remote or git could not.
    Undelivered,
    /// Held back from a repository that is not private (`why`: what it named).
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub from: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub at: String,
}

impl Delivery {
    fn new(state: State, at: Date) -> Delivery {
        Delivery {
            state,
            url: None,
            branch: None,
            why: None,
            at: at.to_string(),
        }
    }

    /// Delivered, or on its way: in a pull request, or merged.
    fn delivered(&self) -> bool {
        matches!(self.state, State::Pr | State::Merged)
    }
}

const RECORD: &str = "delivery.json";

impl Record {
    /// The record in `dir`; an empty one when there is none. A record that
    /// cannot be read is an error, never replaced.
    pub fn read(dir: &Path) -> Result<Record, String> {
        let path = dir.join(RECORD);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{} cannot be read: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }

    fn write(&mut self, dir: &Path) -> Result<(), String> {
        self.schema = SCHEMA.to_owned();
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let mut text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        text.push('\n');
        write_atomic(&dir.join(RECORD), text.as_bytes())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

// ---- where each node stands, for the briefing and `list` ---------------------

/// Where each repo node of the repository a command runs in stands: in its
/// AGENTS.md as checked out (read from the file, no git), or not yet, and why.
#[derive(Debug, Default)]
pub struct Standing {
    pub agents_md: PathBuf,
    /// The marked lines the file holds, by id.
    in_file: BTreeMap<String, Vec<Held>>,
    record: Record,
    /// Ids still in the checkout's `.ohjaa/knowledge/`.
    untracked: BTreeSet<String>,
    /// Ids in ohjaa's state.
    accepted: BTreeSet<String>,
}

impl Standing {
    /// `None` outside a repository. A delivery record that cannot be read is
    /// said on stderr, and every node then reads as not delivered.
    pub fn read(dirs: &Dirs, loaded: &Loaded) -> Option<Standing> {
        let root = dirs.repo_root.as_deref()?;
        let agents_md = root.join("AGENTS.md");
        let in_file = std::fs::read_to_string(&agents_md)
            .map(|text| lines_in(&text))
            .unwrap_or_default();
        let dir = knowledge::accepted_dir(&dirs.state, root);
        let record = Record::read(&dir).unwrap_or_else(|why| {
            eprintln!("ohjaa: {why}");
            Record::default()
        });
        let legacy = knowledge::repo_knowledge(root);
        let mut standing = Standing {
            agents_md,
            in_file,
            record,
            ..Standing::default()
        };
        for (located, file) in loaded.nodes.iter().zip(&loaded.files) {
            if located.scope != Scope::Repo {
                continue;
            }
            if file.starts_with(&legacy) {
                standing.untracked.insert(located.node.id.clone());
            } else if file.starts_with(&dir) {
                standing.accepted.insert(located.node.id.clone());
            }
        }
        Some(standing)
    }

    /// Whether AGENTS.md, as checked out, holds `node`'s line as it is now:
    /// a line of its id in other words (older, edited by hand) does not
    /// count.
    pub fn in_agents_md(&self, node: &Node) -> bool {
        self.in_file
            .get(&node.id)
            .is_some_and(|lines| lines.iter().any(|held| held.text == line(node)))
    }

    /// Where `node`'s line, as it is now, ends in AGENTS.md: the byte after
    /// it, when the file holds it.
    pub fn end_of(&self, node: &Node) -> Option<usize> {
        let line = line(node);
        self.in_file
            .get(&node.id)?
            .iter()
            .find(|held| held.text == line)
            .map(|held| held.end)
    }

    /// What the briefing says beside a repo node it carries: why it is not
    /// left to AGENTS.md.
    pub fn note(&self, node: &Node) -> Option<String> {
        if self.in_agents_md(node) {
            return Some("in AGENTS.md too".to_owned());
        }
        let note = self.delivery_note(&node.id);
        if self.in_file.contains_key(&node.id) {
            let other = "AGENTS.md holds another line of it";
            return Some(note.map_or(other.to_owned(), |note| format!("{note}; {other}")));
        }
        note
    }

    fn delivery_note(&self, id: &str) -> Option<String> {
        let delivery = self.record.nodes.get(id);
        let held = delivery.is_some_and(|delivery| delivery.state == State::Held);
        if self.untracked.contains(id) && !held {
            return Some("not yet in AGENTS.md".to_owned());
        }
        if !self.accepted.contains(id) && !self.untracked.contains(id) {
            return None;
        }
        Some(match delivery {
            Some(Delivery {
                state: State::Pr,
                url: Some(url),
                ..
            }) => format!("pending PR {url}"),
            Some(Delivery {
                state: State::Merged,
                ..
            }) => "merged into AGENTS.md, not yet on this checkout's commit".to_owned(),
            Some(Delivery {
                state: State::Held,
                why,
                ..
            }) => format!(
                "held back from AGENTS.md: {}",
                why.as_deref().unwrap_or("it names something private")
            ),
            Some(Delivery { why, .. }) => format!(
                "not delivered: {}",
                why.as_deref().unwrap_or("not yet tried")
            ),
            None => "not delivered yet".to_owned(),
        })
    }

    /// What `ohjaa list` says under a current repo node.
    pub fn status(&self, node: &Node) -> Option<String> {
        if self.in_agents_md(node) {
            return Some(format!("in AGENTS.md ({})", self.agents_md.display()));
        }
        let note = self.note(node)?;
        Some(if self.untracked.contains(&node.id) {
            format!(
                "{note}: untracked in .ohjaa/knowledge/; deliver it with `ohjaa lessons publish`"
            )
        } else if note.starts_with("not delivered") || note.starts_with("held back") {
            format!("{note}; deliver it with `ohjaa lessons publish` once that is fixed")
        } else {
            note
        })
    }
}

// ---- asking gh and git ----------------------------------------------------------

fn gh(args: &[&str], input: Option<&[u8]>, timeout: Duration) -> Result<String, String> {
    let mut command = Command::new("gh");
    command
        .args(args)
        // gh writes state of its own when it checks for updates; asked not to.
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("GH_PROMPT_DISABLED", "1");
    let what = format!(
        "gh {}",
        args.iter().take(2).copied().collect::<Vec<_>>().join(" ")
    );
    let output =
        process::run(&mut command, input, timeout).map_err(|error| format!("{what}: {error:#}"))?;
    if output.status.success() {
        return Ok(output.stdout_text());
    }
    // gh leads with its reason ("To get started with GitHub CLI, …").
    let stderr = String::from_utf8_lossy(&output.stderr);
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("exited {}", output.status));
    Err(format!("{what}: {why}"))
}

fn git_in(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    git_bytes(dir, args, timeout).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// What git printed, byte for byte, or why it failed, named by its command
/// (past any `-c <setting>` before it).
fn git_bytes(dir: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        // Never a prompt for a password: a delivery is never waited on.
        .env("GIT_TERMINAL_PROMPT", "0");
    for var in gitconfig::REPO_ENV {
        command.env_remove(var);
    }
    let mut words = args.iter().copied();
    let mut command_word = "";
    while let Some(word) = words.next() {
        if word == "-c" {
            words.next();
        } else {
            command_word = word;
            break;
        }
    }
    let what = format!("git {command_word}");
    let output =
        process::run(&mut command, None, timeout).map_err(|error| format!("{what}: {error:#}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!("{what}: {}", output.failure_detail()))
    }
}

/// AGENTS.md as `rev` holds it: empty when it holds none. Anything else that
/// keeps it from being read whole (git failing, bytes that are not UTF-8) is
/// an error, so that the file is never written over from a reading that
/// failed or lost something.
fn agents_md_at(checkout: &Path, rev: &str) -> Result<String, String> {
    let listed = git_in(
        checkout,
        &["ls-tree", "--name-only", rev, "--", "AGENTS.md"],
        process::LOCAL,
    )?;
    if listed.trim().is_empty() {
        return Ok(String::new());
    }
    let bytes = git_bytes(
        checkout,
        &["cat-file", "blob", &format!("{rev}:AGENTS.md")],
        process::LOCAL,
    )?;
    String::from_utf8(bytes).map_err(|_| {
        format!("AGENTS.md on {rev} is not UTF-8, and ohjaa rewrites no file it cannot read whole")
    })
}

/// The repository on GitHub, by HTTPS: the one the pull request is opened
/// on, whatever the checkout's origin pushes to.
fn github_url(slug: &str) -> String {
    format!("https://github.com/{slug}.git")
}

/// `args` for a git that reaches GitHub over HTTPS with gh's credentials
/// (`gh auth git-credential`), the gh a delivery has just been answered by:
/// no SSH key's prompt can hold it up, nor any other helper's. git's own
/// `insteadOf` rules still apply to the URL.
fn over_https<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec![
        "-c",
        "credential.helper=",
        "-c",
        "credential.helper=!gh auth git-credential",
    ];
    all.extend_from_slice(args);
    all
}

/// The repository on GitHub that `checkout`'s origin names, `owner/name`,
/// as written or as git's `insteadOf` rewrites it.
fn github_slug(checkout: &Path) -> Result<String, String> {
    let listing = gitconfig::clone_listing(checkout).map_err(|error| format!("{error:#}"))?;
    let Some(url) = listing.last("remote.origin.url") else {
        return Err("it has no remote named origin".to_owned());
    };
    let rewritten = gitconfig::rewrite(&url.value, &listing.rewrites());
    let found = [url.value.as_str(), rewritten.as_str()]
        .into_iter()
        .filter_map(home::RepoId::parse)
        .find(|id| id.host.as_deref() == Some("github.com"))
        .map(|id| id.slug());
    found.ok_or_else(|| {
        format!(
            "its origin ({}) is not on GitHub, where ohjaa opens the pull request",
            url.value
        )
    })
}

/// What delivering to a repository needs, asked of git and gh.
#[derive(Debug, Clone)]
pub struct Ready {
    pub slug: String,
    /// As GitHub says it: PRIVATE, PUBLIC or INTERNAL.
    pub visibility: String,
    pub default_branch: String,
    /// For a repository that is not private: what its lines are checked
    /// against before anything is sent.
    pub deny: Option<DenyList>,
}

/// The facts one run asks once: the person's private context, checked
/// against the repositories under their roots.
#[derive(Debug, Default)]
pub struct Asker {
    roots: Vec<PathBuf>,
    private: Option<Result<Private, String>>,
}

impl Asker {
    /// An asker for a run whose person keeps their repositories under
    /// `roots` (ohjaa's roots file, else the repository's siblings).
    pub fn new(roots: Vec<PathBuf>) -> Asker {
        Asker {
            roots,
            private: None,
        }
    }

    fn private(&mut self) -> Result<Private, String> {
        let roots = self.roots.clone();
        self.private
            .get_or_insert_with(|| private_context(&roots))
            .clone()
    }

    /// What delivering to `checkout`'s repository needs, or why it cannot
    /// be delivered: no GitHub remote, no gh, gh cannot say, or (for a
    /// repository that is not private) the deny list cannot be built.
    pub fn ready(&mut self, checkout: &Path) -> Result<Ready, String> {
        if !checkout.is_dir() {
            return Err(format!("the checkout {} is gone", checkout.display()));
        }
        let slug = github_slug(checkout)?;
        if !knowledge::on_path("gh") {
            return Err("gh is not on PATH".to_owned());
        }
        let view = gh(
            &[
                "repo",
                "view",
                &slug,
                "--json",
                "visibility,defaultBranchRef",
            ],
            None,
            GH,
        )?;
        let view: Value = serde_json::from_str(&view)
            .map_err(|error| format!("gh repo view printed no answer: {error}"))?;
        let visibility = view["visibility"]
            .as_str()
            .ok_or("gh repo view said no visibility")?
            .to_owned();
        let default_branch = view["defaultBranchRef"]["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or("gh repo view said no default branch")?
            .to_owned();
        let deny = if visibility.eq_ignore_ascii_case("PRIVATE") {
            None
        } else {
            let private = self.private().map_err(|why| {
                format!(
                    "{slug} is {}, and your private repositories could not be listed to check \
                     what goes there: {why}",
                    visibility.to_ascii_lowercase()
                )
            })?;
            let home = std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from);
            Some(DenyList::build(&private, home.as_deref(), &slug))
        };
        Ok(Ready {
            slug,
            visibility,
            default_branch,
            deny,
        })
    }
}

/// The person's private context, never trusted from gh's list alone: a list
/// that holds no private repository is refused, and so is one that lacks a
/// repository found under `roots` that gh does not show as public (a token
/// that sees one organisation, an account switched). The name of every
/// repository under the roots gh does not show as public counts as private.
fn private_context(roots: &[PathBuf]) -> Result<Private, String> {
    let people = gh(&["api", "user"], None, GH).and_then(|out| parse_user(out.as_bytes()))?;
    let listing = gh(&["api", PRIVATE_REPOS, "--paginate"], None, GH)
        .and_then(|out| parse_repos(out.as_bytes()))?;
    if listing.private.is_empty() {
        return Err(
            "gh lists none of your repositories as private or internal, so what is sent cannot \
             be checked against them (is gh signed in to the account that owns them?)"
                .to_owned(),
        );
    }
    let under = crate::roots::git_repos(roots).map_err(|error| {
        format!("the repositories under your roots cannot be listed: {error:#}")
    })?;
    let mut unlisted = Vec::new();
    let mut viewed: BTreeMap<String, bool> = BTreeMap::new();
    for repo in under {
        let slug = github_slug(&repo).ok();
        let public = match &slug {
            Some(slug) if listing.listed.contains(&slug.to_lowercase()) => {
                let name = slug.rsplit('/').next().unwrap_or(slug);
                listing.public.contains(&name.to_lowercase())
            }
            Some(slug) => match viewed.get(slug) {
                Some(public) => *public,
                None => {
                    let visibility = gh(&["repo", "view", slug, "--json", "visibility"], None, GH)
                        .and_then(|out| {
                            serde_json::from_str::<Value>(&out)
                                .map_err(|error| error.to_string())?["visibility"]
                                .as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| "no visibility".to_owned())
                        });
                    match visibility {
                        Ok(visibility) if visibility.eq_ignore_ascii_case("PUBLIC") => {
                            viewed.insert(slug.clone(), true);
                            true
                        }
                        Ok(visibility) => {
                            return Err(format!(
                                "gh's list of your repositories is not complete: it lacks {slug} \
                                 (under your roots at {}), which gh says is {}",
                                repo.display(),
                                visibility.to_ascii_lowercase()
                            ))
                        }
                        Err(why) => {
                            return Err(format!(
                                "gh's list of your repositories is not complete: it lacks {slug} \
                                 (under your roots at {}), which gh cannot show: {why}",
                                repo.display()
                            ))
                        }
                    }
                }
            },
            None => false,
        };
        if !public {
            if let Some(dir) = repo.file_name() {
                add_once(&mut unlisted, &dir.to_string_lossy());
            }
            if let Some(name) = slug.as_deref().and_then(|slug| slug.rsplit('/').next()) {
                add_once(&mut unlisted, name);
            }
        }
    }
    Ok(Private {
        repos: listing.private,
        people,
        unlisted,
    })
}

/// Before a person's accept of a repo proposal: what its delivery needs,
/// asked now. Refused, so that nothing is accepted and the proposal stays
/// waiting, when its repository is not private and its line names something
/// on the deny list; its text is sent nowhere.
pub fn screen(asker: &mut Asker, checkout: &Path, node: &Node) -> Result<Result<Ready, String>> {
    let ready = asker.ready(checkout);
    if let Ok(Ready {
        slug,
        visibility,
        deny: Some(deny),
        ..
    }) = &ready
    {
        let found = deny.matches(&line(node));
        if !found.is_empty() {
            return Err(Refused(format!(
                "not accepted: {slug} is {}, and `{}` names {}. Nothing was sent anywhere, and it \
                 stays waiting: reword it (reject it and propose it again), or make it knowledge \
                 of a private repository",
                visibility.to_ascii_lowercase(),
                node.id,
                found.join(", ")
            ))
            .into());
        }
    }
    Ok(ready)
}

/// An open pull request of the lessons.
#[derive(Debug, Clone)]
struct OpenPr {
    url: String,
    branch: String,
}

/// The person's own open pull request of the lessons in `slug`, from a
/// branch of that repository: a lessons branch someone else opened is not
/// added to.
fn open_pr(slug: &str) -> Result<Option<OpenPr>, String> {
    let out = gh(
        &[
            "pr",
            "list",
            "-R",
            slug,
            "--author",
            "@me",
            "--state",
            "open",
            "--json",
            "number,url,headRefName,isCrossRepository",
            "--limit",
            "100",
        ],
        None,
        GH,
    )?;
    let list: Value = serde_json::from_str(&out)
        .map_err(|error| format!("gh pr list printed no answer: {error}"))?;
    let list = list.as_array().ok_or("gh pr list printed no list")?;
    Ok(list
        .iter()
        .filter(|pr| pr["isCrossRepository"].as_bool() != Some(true))
        .find_map(|pr| {
            let branch = pr["headRefName"].as_str()?;
            branch.starts_with(BRANCH_PREFIX).then(|| OpenPr {
                url: pr["url"].as_str().unwrap_or_default().to_owned(),
                branch: branch.to_owned(),
            })
        })
        .filter(|pr| !pr.url.is_empty()))
}

// ---- delivering ------------------------------------------------------------------

/// What a delivery said, and whether everything it looked at is delivered.
#[derive(Debug, Default)]
pub struct Delivered {
    pub said: Vec<String>,
    pub complete: bool,
}

/// What the section should become, from what it is.
struct Plan {
    text: String,
    changes: Changes,
    held: Vec<(String, Vec<String>)>,
}

fn plan(base: &str, lines: &[(String, String)], gone: &[String], deny: Option<&DenyList>) -> Plan {
    let (text, changes) = with_lessons(base, lines, gone);
    let held: Vec<(String, Vec<String>)> = deny
        .map(|deny| {
            changes
                .sent()
                .filter_map(|id| {
                    let (_, line) = lines.iter().find(|(of, _)| of == id)?;
                    let found = deny.matches(line);
                    (!found.is_empty()).then(|| (id.clone(), found))
                })
                .collect()
        })
        .unwrap_or_default();
    let kept: Vec<(String, String)> = lines
        .iter()
        .filter(|(id, _)| !held.iter().any(|(of, _)| of == id))
        .cloned()
        .collect();
    let (text, changes) = if held.is_empty() {
        (text, changes)
    } else {
        with_lessons(base, &kept, gone)
    };
    Plan {
        text,
        changes,
        held,
    }
}

/// A worktree of the person's clone, off the remote's branch, in ohjaa's
/// state: removed when dropped, however the delivery ends.
struct Work {
    checkout: PathBuf,
    path: PathBuf,
}

impl Work {
    /// What a delivery that was killed left in `lessons/.work/`: taken out,
    /// under the lessons lock (so no delivery is writing there), and git's
    /// record of it in `checkout`'s clone pruned. Another clone's record of
    /// one is pruned by git in its own time.
    fn sweep(checkout: &Path, state: &Path, said: &mut Vec<String>) {
        let base = state.join("lessons").join(".work");
        let Ok(entries) = std::fs::read_dir(&base) else {
            return;
        };
        let mut swept = 0;
        for entry in entries.filter_map(|entry| entry.ok()) {
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => swept += 1,
                Err(error) => said.push(format!(
                    "left {}, from a delivery that did not end: {error}",
                    entry.path().display()
                )),
            }
        }
        if swept > 0 {
            said.push(format!(
                "took out {swept} worktree(s) a delivery that did not end left in {}",
                base.display()
            ));
            let _ = git_in(checkout, &["worktree", "prune"], process::LOCAL);
        }
    }

    fn add(checkout: &Path, state: &Path, rev: &str) -> Result<Work, String> {
        let base = state.join("lessons").join(".work");
        std::fs::create_dir_all(&base)
            .map_err(|error| format!("cannot create {}: {error}", base.display()))?;
        let path = base.join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        git_in(
            checkout,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                &path.to_string_lossy(),
                rev,
            ],
            process::LOCAL,
        )?;
        Ok(Work {
            checkout: checkout.to_path_buf(),
            path,
        })
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let path = self.path.to_string_lossy().into_owned();
        if let Err(why) = git_in(
            &self.checkout,
            &["worktree", "remove", "--force", &path],
            process::LOCAL,
        ) {
            eprintln!("ohjaa: {why}; removing {path} by hand");
            let _ = std::fs::remove_dir_all(&self.path);
            let _ = git_in(&self.checkout, &["worktree", "prune"], process::LOCAL);
        }
    }
}

/// A node with the file it was read from.
type Filed = (Node, PathBuf);

/// The nodes in `dir` (ohjaa's state for a clone, or a checkout's
/// `.ohjaa/knowledge/`); a file that is no node is said, and left.
pub fn nodes_in(dir: &Path, said: &mut Vec<String>) -> Vec<Filed> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.to_owned();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| fk::parse_node(&stem, &text).map_err(|error| error.to_string()));
            match parsed {
                Ok(node) => Some((node, path)),
                Err(why) => {
                    said.push(format!("left {}: not a node: {why}", path.display()));
                    None
                }
            }
        })
        .collect()
}

/// The checkout's `.ohjaa/knowledge/` nodes that `lessons publish` takes:
/// accepted ones, current or not. A proposed node waits for a decision.
pub fn untracked(checkout: &Path, said: &mut Vec<String>) -> Vec<Filed> {
    nodes_in(&knowledge::repo_knowledge(checkout), said)
        .into_iter()
        .filter(|(node, _)| node.status != Status::Proposed)
        .collect()
}

/// What a clone's lessons should be: from the nodes in ohjaa's state and
/// those still in the checkout.
struct Wanted {
    located: Vec<Located>,
    /// The current nodes' lines, in briefing order.
    lines: Vec<(String, String)>,
    /// The ids no longer current: their lines are taken out.
    gone: Vec<String>,
}

impl Wanted {
    fn of(nodes: &[&Filed]) -> Wanted {
        let located: Vec<Located> = nodes
            .iter()
            .map(|(node, _)| Located {
                scope: Scope::Repo,
                node: node.clone(),
            })
            .collect();
        let lines = fk::briefing(&located)
            .into_iter()
            .map(|located| (located.node.id.clone(), line(&located.node)))
            .collect();
        let gone = located
            .iter()
            .filter(|located| located.node.status != Status::Current)
            .map(|located| located.node.id.clone())
            .collect();
        Wanted {
            located,
            lines,
            gone,
        }
    }
}

/// What a delivery sends besides the node lines: its commit message and,
/// for a new pull request, its branch and body (the title is [`TITLE`]).
struct Outgoing {
    branch: String,
    message: String,
    body: String,
}

fn outgoing(
    ready: &Ready,
    plan: &Plan,
    located: &[Located],
    open: Option<&OpenPr>,
    today: Date,
) -> Outgoing {
    let branch = open.map_or_else(
        || {
            format!(
                "{BRANCH_PREFIX}{today}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            )
        },
        |pr| pr.branch.clone(),
    );
    Outgoing {
        branch,
        message: commit_message(&plan.changes),
        body: pr_body(ready, plan, located),
    }
}

/// For a repository that is not private: what of the deny list the rest of
/// what would be sent names, besides the node lines [`plan`] checks: the
/// section's other new lines (its heading and intro, when it is new), the
/// commit message and, for a new pull request, its branch, title and body.
fn names_private(
    deny: &DenyList,
    base: &str,
    plan: &Plan,
    out: &Outgoing,
    new_pr: bool,
) -> Vec<String> {
    let before: BTreeSet<&str> = base.lines().collect();
    let mut sent: Vec<&str> = plan
        .text
        .lines()
        .filter(|line| marked(line).is_none() && !before.contains(line))
        .collect();
    sent.push(&out.message);
    if new_pr {
        sent.extend([out.branch.as_str(), TITLE, out.body.as_str()]);
    }
    let mut found: Vec<String> = Vec::new();
    for text in sent {
        for named in deny.matches(text) {
            if !found.contains(&named) {
                found.push(named);
            }
        }
    }
    found
}

/// A delivery made: what it was made against and where it went.
struct Sent {
    ready: Ready,
    plan: Plan,
    open: Option<OpenPr>,
    url: Option<String>,
    branch: Option<String>,
}

/// Bring the Lessons section of `checkout`'s repository in line with the
/// repo knowledge ohjaa's state holds for its clone, and `extra` (nodes still
/// in the checkout's `.ohjaa/knowledge/`, moved into ohjaa's state once
/// delivered): one pull request, the open one of the lessons, else a new
/// one. `ready` is what the delivery needs, or why it cannot be made. A dry
/// run reads the branch as last fetched, and fetches, writes and sends
/// nothing.
pub fn deliver(
    state: &Path,
    checkout: &Path,
    ready: Result<Ready, String>,
    extra: Vec<Filed>,
    dry_run: bool,
    today: Date,
) -> Delivered {
    let dir = knowledge::accepted_dir(state, checkout);
    let mut said = Vec::new();
    let stays = |extra: usize, said: &mut Vec<String>| {
        if extra > 0 {
            said.push(format!(
                "{extra} file(s) stay in {}",
                knowledge::repo_knowledge(checkout).display()
            ));
        }
    };
    // One delivery at a time changes this state, from its first read to its
    // last write.
    let _lock = if dry_run {
        None
    } else {
        match process::lock_file(&state.join("lessons").join("lessons.lock"), LOCK_WAIT) {
            Ok(lock) => {
                Work::sweep(checkout, state, &mut said);
                Some(lock)
            }
            Err(why) => {
                said.push(format!("not delivered: {why}"));
                stays(extra.len(), &mut said);
                return Delivered {
                    said,
                    complete: false,
                };
            }
        }
    };
    let mut record = match Record::read(&dir) {
        Ok(record) => record,
        Err(why) => {
            said.push(format!("not delivered: {why}; fix or remove it"));
            stays(extra.len(), &mut said);
            return Delivered {
                said,
                complete: false,
            };
        }
    };
    let in_state: Vec<Filed> = nodes_in(&dir, &mut said);
    // A node in both is ohjaa's already: its file in the checkout is left.
    let extra: Vec<Filed> = extra
        .into_iter()
        .filter(|(node, path)| {
            let twice = in_state.iter().any(|(held, _)| held.id == node.id);
            if twice {
                said.push(format!(
                    "left {}: `{}` is in ohjaa's state already",
                    path.display(),
                    node.id
                ));
            }
            !twice
        })
        .collect();
    let wanted = Wanted::of(&in_state.iter().chain(&extra).collect::<Vec<_>>());

    let sent = match attempt(
        checkout, state, ready, &wanted, &extra, dry_run, today, &mut said,
    ) {
        Ok(Some(sent)) => sent,
        Ok(None) => {
            return Delivered {
                complete: !said.iter().any(|line| line.starts_with("held back")),
                said,
            }
        }
        Err(why) => {
            said.push(format!("not delivered: {why}"));
            if !dry_run {
                for (id, _) in &wanted.lines {
                    let delivered = record.nodes.get(id).is_some_and(Delivery::delivered);
                    if in_state.iter().any(|(node, _)| node.id == *id) && !delivered {
                        let mut delivery = Delivery::new(State::Undelivered, today);
                        delivery.why = Some(why.clone());
                        record.nodes.insert(id.clone(), delivery);
                    }
                }
                record.checkout = Some(checkout.to_path_buf());
                if let Err(why) = record.write(&dir) {
                    said.push(why);
                }
            }
            stays(extra.len(), &mut said);
            return Delivered {
                said,
                complete: false,
            };
        }
    };

    // Each node as it now stands: a line the default branch held as it is
    // was merged; the others are in the pull request.
    let changed: BTreeSet<&String> = sent.plan.changes.sent().collect();
    for (id, _) in &wanted.lines {
        let delivery = if let Some((_, found)) = sent.plan.held.iter().find(|(of, _)| of == id) {
            let mut held = Delivery::new(State::Held, today);
            held.why = Some(format!(
                "{} is {}, and it names {}",
                sent.ready.slug,
                sent.ready.visibility.to_ascii_lowercase(),
                found.join(", ")
            ));
            held
        } else if sent.open.is_none() && !changed.contains(id) {
            Delivery::new(State::Merged, today)
        } else {
            let mut pr = Delivery::new(State::Pr, today);
            pr.url = sent.url.clone();
            pr.branch = sent.branch.clone();
            pr
        };
        record.nodes.insert(id.clone(), delivery);
    }
    for id in &wanted.gone {
        record.nodes.remove(id);
    }
    record.checkout = Some(checkout.to_path_buf());
    record.slug = Some(sent.ready.slug.clone());
    // What came from the checkout and is delivered goes into ohjaa's state.
    let mut moved = 0;
    let mut stayed = 0;
    for (node, path) in &extra {
        if sent.plan.held.iter().any(|(id, _)| *id == node.id) {
            stayed += 1;
            continue;
        }
        match move_in(path, &dir.join(format!("{}.md", node.id))) {
            Ok(()) => {
                moved += 1;
                record.moved.insert(
                    node.id.clone(),
                    Moved {
                        from: path.clone(),
                        url: sent.url.clone(),
                        at: today.to_string(),
                    },
                );
            }
            Err(why) => {
                stayed += 1;
                said.push(why);
            }
        }
    }
    if let Err(why) = record.write(&dir) {
        said.push(why);
    }
    if moved > 0 {
        said.push(format!(
            "moved {moved} file(s) out of {} into {}",
            knowledge::repo_knowledge(checkout).display(),
            dir.display()
        ));
        knowledge::tidy_untracked(checkout);
    }
    stays(stayed, &mut said);
    Delivered {
        said,
        complete: sent.plan.held.is_empty() && stayed == 0,
    }
}

/// The delivery itself: what it sends, checked first, then sent. `None` for
/// a dry run, which only says what it would do; an error says why nothing
/// was delivered.
#[allow(clippy::too_many_arguments)]
fn attempt(
    checkout: &Path,
    state: &Path,
    ready: Result<Ready, String>,
    wanted: &Wanted,
    extra: &[Filed],
    dry_run: bool,
    today: Date,
    said: &mut Vec<String>,
) -> Result<Option<Sent>, String> {
    let ready = ready?;
    let mut head = format!("{}, {}", ready.slug, ready.visibility.to_ascii_lowercase());
    if let Some(deny) = &ready.deny {
        let _ = write!(
            head,
            " ({}): all it is sent is checked first against {} names of what is yours and private",
            ready.visibility,
            deny.len()
        );
    }
    said.push(head);
    let open = open_pr(&ready.slug)?;
    let base_branch = open
        .as_ref()
        .map_or(ready.default_branch.clone(), |pr| pr.branch.clone());
    let tracking = |branch: &str| format!("refs/remotes/origin/{branch}");
    let tracked = |branch: &str| {
        git_in(
            checkout,
            &["rev-parse", "--verify", "--quiet", &tracking(branch)],
            process::LOCAL,
        )
        .is_ok()
    };
    let read_from = if dry_run {
        // The branch as last fetched here: a dry run fetches nothing.
        if tracked(&base_branch) {
            base_branch.clone()
        } else if tracked(&ready.default_branch) {
            ready.default_branch.clone()
        } else {
            return Err(format!(
                "origin/{} was never fetched here, and a dry run fetches nothing",
                ready.default_branch
            ));
        }
    } else {
        let refspec = format!("+refs/heads/{base_branch}:{}", tracking(&base_branch));
        git_in(
            checkout,
            &over_https(&["fetch", "--quiet", &github_url(&ready.slug), &refspec]),
            FETCH,
        )?;
        base_branch.clone()
    };
    let base = agents_md_at(checkout, &tracking(&read_from))?;
    let plan = plan(&base, &wanted.lines, &wanted.gone, ready.deny.as_ref());
    let out = outgoing(&ready, &plan, &wanted.located, open.as_ref(), today);
    if let Some(deny) = &ready.deny {
        let found = names_private(deny, &base, &plan, &out, open.is_none());
        if !found.is_empty() {
            return Err(format!(
                "what ohjaa itself would send to {} names {}, so nothing {} sent",
                ready.slug,
                found.join(", "),
                if dry_run { "would be" } else { "was" }
            ));
        }
    }
    if dry_run {
        said.push(format!(
            "AGENTS.md as of origin/{read_from}, as last fetched here"
        ));
    }
    say_plan(&plan, &wanted.lines, said);
    let unchanged = plan.text == base;
    if dry_run {
        said.push(match (&open, unchanged) {
            (_, true) => "nothing to write".to_owned(),
            (Some(pr), false) => format!("would add a commit to {} ({})", pr.url, pr.branch),
            (None, false) => format!("would open a pull request onto {}", ready.default_branch),
        });
        let moving = extra
            .iter()
            .filter(|(node, _)| !plan.held.iter().any(|(id, _)| *id == node.id))
            .count();
        if moving > 0 {
            said.push(format!(
                "would then move {moving} file(s) out of {} into ohjaa's state",
                knowledge::repo_knowledge(checkout).display()
            ));
        }
        return Ok(None);
    }
    if unchanged {
        said.push(match &open {
            Some(pr) => format!("nothing new for {}", pr.url),
            None => format!(
                "nothing new: AGENTS.md on {} holds it all",
                ready.default_branch
            ),
        });
        return Ok(Some(Sent {
            url: open.as_ref().map(|pr| pr.url.clone()),
            branch: open.as_ref().map(|pr| pr.branch.clone()),
            ready,
            plan,
            open,
        }));
    }
    let work = Work::add(checkout, state, &tracking(&base_branch))?;
    write_atomic(&work.path.join("AGENTS.md"), plan.text.as_bytes())?;
    git_in(&work.path, &["add", "AGENTS.md"], process::LOCAL)?;
    git_in(
        &work.path,
        &["commit", "--quiet", "-m", &out.message],
        HOOKS,
    )?;
    // Never a force: on an open pull request's branch, a commit on its tip.
    let refspec = format!("HEAD:refs/heads/{}", out.branch);
    git_in(
        &work.path,
        &over_https(&["push", "--quiet", &github_url(&ready.slug), &refspec]),
        HOOKS,
    )?;
    drop(work);
    let url = match &open {
        Some(pr) => {
            said.push(format!("added to {}", pr.url));
            pr.url.clone()
        }
        None => {
            let url = create_pr(&ready, &out).map_err(|why| {
                format!(
                    "{} is pushed, but no pull request was opened: {why}",
                    out.branch
                )
            })?;
            said.push(format!(
                "opened {url}: AGENTS.md ({HEADING}); merge it like any change"
            ));
            url
        }
    };
    Ok(Some(Sent {
        ready,
        plan,
        open,
        url: Some(url),
        branch: Some(out.branch),
    }))
}

/// What the plan adds, changes, takes out and holds back.
fn say_plan(plan: &Plan, lines: &[(String, String)], said: &mut Vec<String>) {
    let line_of = |id: &str| {
        lines
            .iter()
            .find(|(of, _)| of == id)
            .map_or(String::new(), |(_, line)| line.clone())
    };
    for id in &plan.changes.added {
        said.push(format!("+ {}", line_of(id)));
    }
    for id in &plan.changes.changed {
        said.push(format!("~ {}", line_of(id)));
    }
    for id in &plan.changes.removed {
        said.push(format!("- the line of `{id}`"));
    }
    for (id, found) in &plan.held {
        said.push(format!("held back: `{id}` names {}", found.join(", ")));
    }
}

/// The commit: what changed, by id. It names no tool.
fn commit_message(changes: &Changes) -> String {
    let mut message = format!(
        "{TITLE}\n\nThe lines in AGENTS.md's {HEADING}: repository knowledge a person accepted, \
         one line per node.\n\n"
    );
    for id in &changes.added {
        let _ = writeln!(message, "+ {id}");
    }
    for id in &changes.changed {
        let _ = writeln!(message, "~ {id}");
    }
    for id in &changes.removed {
        let _ = writeln!(message, "- {id}");
    }
    message.trim_end().to_owned()
}

/// A new pull request's body, in words that name no tool. For a private
/// repository it says why each was accepted, in the person's own words; for
/// any other it carries no reason.
fn pr_body(ready: &Ready, plan: &Plan, located: &[Located]) -> String {
    let mut body = format!(
        "Repository knowledge a person accepted, written into the `{HEADING}` section of \
         AGENTS.md: one line per node, each marked with its id. Every agent reads AGENTS.md, so \
         once this merges the briefing agents get here stops repeating these lessons; until \
         then it carries them, marked as pending this pull request. Merge it like any change; \
         while it is open, later lessons are added to it.\n\n"
    );
    for id in plan.changes.sent() {
        let _ = writeln!(body, "+ `{id}`");
    }
    for id in &plan.changes.removed {
        let _ = writeln!(body, "- `{id}`");
    }
    if ready.deny.is_some() {
        let _ = write!(
            body,
            "\nThis repository is {}: each line, and all else sent here, was checked first \
             against the names of your private repositories, your home path and your name, and \
             the reasons behind the lessons are not sent here.\n",
            ready.visibility.to_ascii_lowercase()
        );
    } else {
        body.push_str("\nWhy each was accepted:\n");
        for id in plan.changes.sent() {
            if let Some(located) = located.iter().find(|located| located.node.id == *id) {
                let _ = writeln!(body, "- `{id}`: {}", located.node.origin.join("; "));
            }
        }
    }
    body
}

/// Open the pull request for what was pushed, and say its URL.
fn create_pr(ready: &Ready, out: &Outgoing) -> Result<String, String> {
    let printed = gh(
        &[
            "pr",
            "create",
            "-R",
            &ready.slug,
            "--base",
            &ready.default_branch,
            "--head",
            &out.branch,
            "--title",
            TITLE,
            "--body-file",
            "-",
        ],
        Some(out.body.as_bytes()),
        GH_CREATE,
    )?;
    printed
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("https://"))
        .map(str::to_owned)
        .ok_or_else(|| "gh pr create printed no URL".to_owned())
}

/// Move a node's file out of the checkout into ohjaa's state: written there
/// first, then removed here, so a failure in between leaves both, never none.
fn move_in(from: &Path, to: &Path) -> Result<(), String> {
    let bytes =
        std::fs::read(from).map_err(|error| format!("cannot read {}: {error}", from.display()))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    write_atomic(to, &bytes)?;
    std::fs::remove_file(from).map_err(|error| {
        format!(
            "{} is copied into {}, but cannot be removed: {error}",
            from.display(),
            to.display()
        )
    })
}

// ---- which repositories are waiting ------------------------------------------------

/// The checkouts `ohjaa lessons publish` takes when named none: each
/// repository under `repos` with a node in its `.ohjaa/knowledge/`, and each
/// clone in ohjaa's state with a current node not delivered (by the
/// checkout its record names), once per clone.
pub fn waiting(state: &Path, repos: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut clones: BTreeSet<PathBuf> = BTreeSet::new();
    let mut ignored = Vec::new();
    for repo in repos {
        if !untracked(repo, &mut ignored).is_empty() {
            let dir = knowledge::accepted_dir(state, repo);
            if clones.insert(dir) {
                found.push(repo.clone());
            }
        }
    }
    let Ok(entries) = std::fs::read_dir(state.join("lessons")) else {
        return found;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    dirs.sort();
    for dir in dirs {
        if clones.contains(&dir) {
            continue;
        }
        let Ok(record) = Record::read(&dir) else {
            continue;
        };
        let undelivered = nodes_in(&dir, &mut ignored).iter().any(|(node, _)| {
            node.status == Status::Current
                && !record.nodes.get(&node.id).is_some_and(Delivery::delivered)
        });
        if let (true, Some(checkout)) = (undelivered, record.checkout) {
            if clones.insert(dir) {
                found.push(checkout);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use faija_knowledge::Kind;

    fn node(id: &str, kind: Kind, body: &str) -> Node {
        Node {
            id: id.to_owned(),
            kind,
            status: Status::Current,
            as_of: Date::new(2026, 10, 27).unwrap(),
            origin: vec!["a test".to_owned()],
            supersedes: Vec::new(),
            refines: None,
            holds_while: Vec::new(),
            body: body.to_owned(),
        }
    }

    fn lines(nodes: &[&Node]) -> Vec<(String, String)> {
        nodes.iter().map(|n| (n.id.clone(), line(n))).collect()
    }

    #[test]
    fn a_line_is_one_line_marked_with_its_id() {
        let multi = node("a-b", Kind::Lesson, "First\nsecond  line.\n\n  third.");
        assert_eq!(
            line(&multi),
            "- <!-- lesson: a-b --> **lesson**: First second line. third. (as of 2026-10-27)"
        );
        assert_eq!(marked(&line(&multi)), Some("a-b"));
        assert_eq!(marked("  - <!-- lesson: x-1 --> anything\r"), Some("x-1"));
        assert_eq!(marked("- <!-- lesson: Not An Id --> x"), None);
        assert_eq!(marked("- a person's line"), None);
        let found =
            lines_in("x\n- <!-- lesson: one --> a\r\n## Other\n  - <!-- lesson: two --> b\n");
        assert_eq!(found.len(), 2);
        let text = "x\n- <!-- lesson: one --> a\r\n## Other\n  - <!-- lesson: two --> b\n";
        assert_eq!(found["one"][0].text, "- <!-- lesson: one --> a");
        assert_eq!(found["two"][0].text, "- <!-- lesson: two --> b");
        // Where each ends in the file, its line ending left out.
        assert_eq!(&text[..found["one"][0].end], "x\n- <!-- lesson: one --> a");
        assert!(text[..found["two"][0].end].ends_with("--> b"));
    }

    #[test]
    fn agents_md_holds_a_node_only_in_its_words_as_they_are() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let mut confirmed = rust.clone();
        confirmed.as_of = Date::new(2026, 11, 2).unwrap();
        let standing = Standing {
            in_file: lines_in(&format!("# G\n\n{}\n", line(&rust))),
            accepted: ["rust".to_owned()].into_iter().collect(),
            ..Standing::default()
        };
        assert!(standing.in_agents_md(&rust));
        assert!(
            !standing.in_agents_md(&confirmed),
            "an older line is not the node as it is now"
        );
        assert_eq!(
            standing.note(&confirmed).unwrap(),
            "not delivered yet; AGENTS.md holds another line of it"
        );
    }

    #[test]
    fn the_section_is_added_after_what_the_file_holds() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let existing = "# Guide\n\nRead me.\n";
        let (text, changes) = with_lessons(existing, &lines(&[&rust]), &[]);
        assert_eq!(
            text,
            format!(
                "# Guide\n\nRead me.\n\n{HEADING}\n\n{INTRO}\n\n{}\n",
                line(&rust)
            )
        );
        assert_eq!(changes.added, vec!["rust"]);
        // Its own result: nothing changes.
        let (again, changes) = with_lessons(&text, &lines(&[&rust]), &[]);
        assert_eq!(again, text);
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn a_file_without_an_ending_newline_or_none_at_all() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let (text, _) = with_lessons("# Guide", &lines(&[&rust]), &[]);
        assert_eq!(
            text,
            format!("# Guide\n\n{HEADING}\n\n{INTRO}\n\n{}\n", line(&rust))
        );
        let (text, _) = with_lessons("", &lines(&[&rust]), &[]);
        assert_eq!(text, format!("{HEADING}\n\n{INTRO}\n\n{}\n", line(&rust)));
        let (text, changes) = with_lessons("# Guide\n", &[], &[]);
        assert_eq!(text, "# Guide\n");
        assert!(changes.is_empty());
    }

    #[test]
    fn a_line_is_replaced_by_its_id_and_a_gone_one_taken_out() {
        let old = node("rust", Kind::Decision, "Rust only.");
        let other = node("tests", Kind::Lesson, "Tests first.");
        let (text, _) = with_lessons("# G\n", &lines(&[&old, &other]), &[]);
        let mut confirmed = old.clone();
        confirmed.as_of = Date::new(2026, 11, 1).unwrap();
        let (text, changes) = with_lessons(&text, &lines(&[&confirmed]), &["tests".to_owned()]);
        assert!(text.contains(&line(&confirmed)), "{text}");
        assert!(!text.contains(&line(&old)), "{text}");
        assert!(!text.contains("Tests first."), "{text}");
        assert_eq!(changes.changed, vec!["rust"]);
        assert_eq!(changes.removed, vec!["tests"]);
        assert!(changes.added.is_empty());
    }

    #[test]
    fn what_ohjaa_does_not_know_stays_where_it_is() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let existing = format!(
            "# G\n\n{HEADING}\n\n- A line a person wrote.\n- {MARK}from-elsewhere{MARK_END} kept.\n\n\
             ## After\n\nMore.\n"
        );
        let (text, changes) = with_lessons(&existing, &lines(&[&rust]), &[]);
        assert_eq!(
            text,
            format!(
                "# G\n\n{HEADING}\n\n- A line a person wrote.\n- {MARK}from-elsewhere{MARK_END} kept.\n{}\n\n\
                 ## After\n\nMore.\n",
                line(&rust)
            ),
            "inside the section, after the last of ohjaa's, before the next heading"
        );
        assert_eq!(changes.added, vec!["rust"]);
    }

    #[test]
    fn an_empty_section_gets_its_lines_and_a_heading_in_a_fence_is_no_heading() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let existing = format!("{HEADING}\n## Next\n");
        let (text, _) = with_lessons(&existing, &lines(&[&rust]), &[]);
        assert_eq!(text, format!("{HEADING}\n\n{}\n\n## Next\n", line(&rust)));
        let fenced = format!("{HEADING}\n\n- mine\n```\n## not a heading\n```\n\n## Next\n");
        let (text, _) = with_lessons(&fenced, &lines(&[&rust]), &[]);
        assert_eq!(
            text,
            format!(
                "{HEADING}\n\n- mine\n```\n## not a heading\n```\n{}\n\n## Next\n",
                line(&rust)
            )
        );
    }

    #[test]
    fn crlf_files_keep_their_endings_and_a_second_line_of_an_id_goes() {
        let rust = node("rust", Kind::Decision, "Rust only.");
        let existing = format!(
            "# G\r\n\r\n{HEADING}\r\n\r\n{}\r\n{}\r\n",
            line(&rust),
            line(&rust)
        );
        let tests = node("tests", Kind::Lesson, "Tests first.");
        let (text, changes) = with_lessons(&existing, &lines(&[&rust, &tests]), &[]);
        assert_eq!(
            text,
            format!(
                "# G\r\n\r\n{HEADING}\r\n\r\n{}\r\n{}\r\n",
                line(&rust),
                line(&tests)
            )
        );
        assert_eq!(changes.removed, vec!["rust"]);
        assert_eq!(changes.added, vec!["tests"]);
    }

    #[test]
    fn the_deny_list_matches_words_case_blind_and_the_home_path_anywhere() {
        let private = Private {
            repos: vec![
                "ote".to_owned(),
                "secret-thing".to_owned(),
                "widget".to_owned(),
            ],
            people: vec!["octo-gh".to_owned(), "Octo Cat".to_owned()],
            ..Private::default()
        };
        let deny = DenyList::build(&private, Some(Path::new("/Users/octocat")), "acme/widget");
        assert!(
            deny.matches("a remote note, a quote").is_empty(),
            "inside words"
        );
        assert!(
            deny.matches("the widget repository").is_empty(),
            "its own name"
        );
        assert_eq!(deny.matches("calls OTE's API").len(), 1);
        assert_eq!(deny.matches("ote-selko and more").len(), 1);
        assert_eq!(
            deny.matches("see Secret-Thing"),
            vec!["`secret-thing` (the name of a private repository of yours)"]
        );
        assert_eq!(
            deny.matches("in /users/octocat/x"),
            vec![
                "`/Users/octocat` (your home path)",
                "`octocat` (your account's name)"
            ]
        );
        assert_eq!(deny.matches("Octo cat said").len(), 1);
        assert_eq!(
            deny.matches("octo-gh/tools"),
            vec!["`octo-gh` (you, as GitHub names you)"]
        );
        assert!(deny.matches("").is_empty());
        assert!(spelling::names_word("é", "e", &[]), "a diacritic is folded");
        assert!(
            spelling::names_word("x ote", "ote", &[]) && spelling::names_word("ote", "ote", &[])
        );
        assert!(!spelling::names_word("noted", "ote", &[]));
    }

    #[test]
    fn spellings_that_hide_a_private_name_are_still_named() {
        let private = Private {
            repos: ["selko", "habitat", "toimija", "viemari"]
                .map(str::to_owned)
                .to_vec(),
            people: Vec::new(),
            ..Private::default()
        };
        let deny = DenyList::build(&private, None, "acme/widget");
        for hidden in [
            "tapio's events reach selkod",
            "two habitats",
            "the toimijas",
            "selko2 is next",
            "flagged UNPROVEN in viemäri",
            "hab\u{200B}itat with a zero-width space",
            "hab\u{00AD}itat with a soft hyphen",
            "hab\u{2060}itat with a word joiner",
            "h&#97;bitat as an entity",
            "h&#x61;bitat as a hex entity",
            "hab<!-- -->itat split by a comment",
            "**hab**itat split by emphasis",
            "`hab`itat split by code",
            "ｈａｂｉｔａｔ in fullwidth",
            "Ha\u{0308}bitat with a combining mark",
        ] {
            assert!(!deny.matches(hidden).is_empty(), "not named: {hidden}");
        }
        // A letter outside ASCII that no fold reads as one inside it could
        // stand for another (a Cyrillic а, a Greek ο): such a line is held.
        for odd in ["h\u{0430}bitat with a Cyrillic a", "an \u{3bf}dd letter"] {
            let found = deny.matches(odd);
            assert!(
                found.iter().any(|named| named.contains("outside ASCII")),
                "{odd}: {found:?}"
            );
        }
        // What only looks like a name, or is plain text, is not.
        for plain in ["a habit", "café au lait", "an em dash — and “quotes”"] {
            assert!(
                deny.matches(plain).is_empty(),
                "{plain}: {:?}",
                deny.matches(plain)
            );
        }
    }

    #[test]
    fn a_marked_line_inside_a_fence_or_a_comment_is_not_held_by_the_file() {
        let found = lines_in(
            "```\n- <!-- lesson: fenced --> x\n```\n<!--\n- <!-- lesson: commented --> y\n-->\n\
             - <!-- lesson: real --> z\n",
        );
        assert_eq!(found.keys().collect::<Vec<_>>(), vec!["real"]);
    }

    #[test]
    fn a_name_inside_camel_case_or_beyond_ascii_is_still_named() {
        assert!(
            spelling::names_word("SelkoClient", "selko", &[]),
            "a capital starts a word"
        );
        assert!(spelling::names_word("myOteThing", "ote", &[]));
        assert!(
            !spelling::names_word("remote", "ote", &[])
                && !spelling::names_word("Xote", "ote", &[])
        );
        assert!(spelling::names_word("ask Étienne", "étienne", &[]));
        assert!(spelling::names_word("ÉTIENNE said", "Étienne", &[]));
        assert!(spelling::names_anywhere("/HOME/Ünal/x", "/home/ünal"));
    }

    #[test]
    fn the_tools_own_name_is_always_on_the_list_but_in_its_own_repository() {
        let deny = DenyList::build(&Private::default(), None, "acme/widget");
        let tool = vec!["`ohjaa` (the name of this tool, which is private)".to_owned()];
        assert_eq!(deny.matches("see .ohjaa/knowledge/"), tool);
        assert_eq!(deny.matches("OhjaaCli and ohjaa-marks"), tool);
        let own = DenyList::build(&Private::default(), None, "false-systems/ohjaa");
        assert!(own.matches("ohjaa").is_empty());
    }

    #[test]
    fn the_owner_of_the_repository_is_no_secret_in_it() {
        let private = Private {
            repos: vec!["secret-thing".to_owned()],
            people: vec!["octocat".to_owned(), "Octo Cat".to_owned()],
            ..Private::default()
        };
        // The person's own public repository: their login is in its URL.
        let deny = DenyList::build(&private, Some(Path::new("/Users/octocat")), "octocat/site");
        assert!(deny.matches("a personal octocat repo").is_empty());
        assert_eq!(
            deny.matches("Octo Cat wrote it").len(),
            1,
            "their name is still theirs"
        );
        assert_eq!(
            deny.matches("/Users/octocat/site").len(),
            1,
            "and their home path"
        );
        assert_eq!(deny.matches("uses secret-thing").len(), 1);
    }

    #[test]
    fn private_context_is_read_as_gh_prints_it() {
        assert_eq!(
            parse_user(br#"{"login":"me","name":"Ann Bee Lo"}"#).unwrap(),
            vec!["me", "Ann Bee Lo", "Ann", "Bee"]
        );
        assert_eq!(
            parse_user(br#"{"login":"me","name":null}"#).unwrap(),
            vec!["me"]
        );
        assert!(parse_user(br#"{"message":"Bad credentials"}"#).is_err());
        // One array, as gh joins the pages, or one per page.
        let joined = br#"[{"name":"a","full_name":"o/a"},{"name":"b"},{"name":"a"}]"#;
        assert_eq!(parse_repos(joined).unwrap().private, vec!["a", "b"]);
        let pages = b"[{\"name\":\"a\"}]\n[{\"name\":\"c\"}]";
        assert_eq!(parse_repos(pages).unwrap().private, vec!["a", "c"]);
        assert!(parse_repos(b"").is_err());
        assert!(parse_repos(br#"{"message":"Not Found"}"#)
            .unwrap_err()
            .contains("Not Found"));
        assert!(parse_repos(br#"[{"id":1}]"#).is_err());
        // Public ones are left out; internal and private ones, and one whose
        // visibility GitHub does not say, are kept.
        let mixed = br#"[{"name":"open","visibility":"public","private":false},
            {"name":"inner","visibility":"internal","private":true},
            {"name":"closed","visibility":"private","private":true},
            {"name":"old-public","private":false},{"name":"unsaid"}]"#;
        let listing = parse_repos(mixed).unwrap();
        assert_eq!(listing.private, vec!["inner", "closed", "unsaid"]);
        assert_eq!(
            listing.public.iter().collect::<Vec<_>>(),
            vec!["old-public", "open"]
        );
        let full = br#"[{"name":"A","full_name":"Acme/A","visibility":"private"}]"#;
        assert!(parse_repos(full).unwrap().listed.contains("acme/a"));
    }

    #[test]
    fn held_lines_are_left_out_and_their_old_text_kept() {
        let mut deny = DenyList::default();
        deny.add("ote", "the name of a private repository of yours", true);
        let clean = node("clean", Kind::Fact, "Nothing private.");
        let named = node("named", Kind::Fact, "Calls ote.");
        let first = plan("# G\n", &lines(&[&clean, &named]), &[], Some(&deny));
        assert_eq!(first.held.len(), 1);
        assert_eq!(first.held[0].0, "named");
        assert!(!first.text.contains("Calls ote."), "{}", first.text);
        assert_eq!(first.changes.added, vec!["clean"]);
        // Already there in its old words: they stay, the new ones are held.
        let old = node("named", Kind::Fact, "Calls nothing.");
        let (base, _) = with_lessons("# G\n", &lines(&[&old]), &[]);
        let again = plan(&base, &lines(&[&named]), &[], Some(&deny));
        assert_eq!(again.text, base);
        assert!(again.changes.is_empty());
    }

    #[test]
    fn a_record_reads_back_as_written() {
        let dir = std::env::temp_dir().join(format!("ohjaa-lessons-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Record::read(&dir).unwrap(), Record::default());
        let mut record = Record::default();
        let mut pr = Delivery::new(State::Pr, Date::new(2026, 10, 27).unwrap());
        pr.url = Some("https://github.com/o/r/pull/1".to_owned());
        record.nodes.insert("x".to_owned(), pr);
        record.write(&dir).unwrap();
        let read = Record::read(&dir).unwrap();
        assert_eq!(read.schema, SCHEMA);
        assert_eq!(read.nodes, record.nodes);
        std::fs::write(dir.join(RECORD), "{").unwrap();
        assert!(
            Record::read(&dir).is_err(),
            "a broken record is said, never read as empty"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
