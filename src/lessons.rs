//! Repository knowledge's home: the repository's own AGENTS.md.
//!
//! Every agent reads the repository's AGENTS.md, so that is where repo
//! knowledge goes: a `## Lessons` section, one line per node, each marked
//! with its id ([`line`]). A person accepts a repo proposal, and `ohjaa
//! lessons` then writes the section into the AGENTS.md of a working copy of
//! that repository ([`write`]), for the person to commit and push like any
//! change. Ohjaa sends nothing anywhere: what it writes is in the working
//! copy, to be read before it is committed. Once a line is in AGENTS.md the
//! briefing leaves that node to the file, for an agent shown to read it;
//! until then it carries it, marked ([`Standing`]).
//!
//! ohjaa's state keeps each accepted node, with its why, in
//! `$XDG_STATE_HOME/ohjaa/lessons/<clone>/<id>.md`; `<clone>` is the clone's
//! common git directory, so every worktree of it shares them. Nodes accepted
//! before ohjaa kept them there sit in the checkout's `.ohjaa/knowledge/`,
//! and are read from there too. What ohjaa writes into a repository names no
//! tool: not its marks, nor its words around the lines.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use knowledge_node::{self as fk, Node, Scope, Status};

use crate::knowledge::{self, Dirs, Loaded};
use crate::{gitconfig, home};

/// The heading of the section ohjaa writes in AGENTS.md.
pub const HEADING: &str = "## Lessons";
/// What a line of the section begins with, after its `- `: the node's id
/// follows.
const MARK: &str = "<!-- lesson: ";
const MARK_END: &str = " -->";
/// What opens the section when ohjaa makes it.
pub const INTRO: &str = "<!-- This repository's accepted knowledge: one line per node, each \
     marked with its id. A line changed here by hand is replaced the next time it is written: \
     change a lesson where it is decided. -->";

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

// ---- where each node stands, for the briefing and `list` ---------------------

/// Where each repo node of the repository a command runs in stands: in its
/// AGENTS.md as the working copy holds it (read from the file, no git), or
/// not yet.
#[derive(Debug, Default)]
pub struct Standing {
    pub agents_md: PathBuf,
    /// The marked lines the file holds, by id.
    in_file: BTreeMap<String, Vec<Held>>,
}

impl Standing {
    /// `None` outside a repository.
    pub fn read(dirs: &Dirs) -> Option<Standing> {
        let root = dirs.repo_root.as_deref()?;
        let agents_md = root.join("AGENTS.md");
        let in_file = std::fs::read_to_string(&agents_md)
            .map(|text| lines_in(&text))
            .unwrap_or_default();
        Some(Standing { agents_md, in_file })
    }

    /// Whether AGENTS.md holds `node`'s line as it is now: a line of its id
    /// in other words (older, edited by hand) does not count.
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
    pub fn note(&self, node: &Node) -> &'static str {
        if self.in_agents_md(node) {
            "in AGENTS.md too"
        } else if self.in_file.contains_key(&node.id) {
            "AGENTS.md holds another line of it"
        } else {
            "not yet in AGENTS.md"
        }
    }

    /// What AGENTS.md still says of nodes no longer current (`loaded` holds
    /// them, retired or superseded): one line each, saying what became of
    /// it, and not to follow it.
    pub fn still_said(&self, loaded: &Loaded) -> Vec<String> {
        let mut said = Vec::new();
        for id in self.in_file.keys() {
            let Some(located) = loaded
                .nodes
                .iter()
                .find(|located| located.scope == Scope::Repo && located.node.id == *id)
            else {
                continue;
            };
            let what = match located.node.status {
                Status::Retired => {
                    // retire's own origin: `<date>: retired: <why>`.
                    let retired = located
                        .node
                        .origin
                        .iter()
                        .rev()
                        .find_map(|origin| origin.split_once(": retired: "));
                    match retired {
                        Some((date, why)) => format!("it was retired on {date} ({why})"),
                        None => "it was retired".to_owned(),
                    }
                }
                Status::Superseded => {
                    let successor = loaded.nodes.iter().find(|later| {
                        later.scope == Scope::Repo && later.node.supersedes.contains(id)
                    });
                    match successor {
                        Some(later) => format!("it was superseded by `{}`", later.node.id),
                        None => "it was superseded".to_owned(),
                    }
                }
                _ => continue,
            };
            said.push(format!(
                "AGENTS.md still says `{id}`, but {what}; `ohjaa lessons` takes its line out: do \
                 not follow it."
            ));
        }
        said
    }

    /// What `ohjaa list` says under a current repo node.
    pub fn status(&self, node: &Node) -> String {
        if self.in_agents_md(node) {
            format!("in AGENTS.md ({})", self.agents_md.display())
        } else {
            format!("{}: `ohjaa lessons` writes it there", self.note(node))
        }
    }
}

// ---- `ohjaa lessons` -----------------------------------------------------------

/// What `changes` did, line by line: `+` a line added, `~` one rewritten, `-`
/// one taken out.
fn said(changes: &Changes, lines: &[(String, String)]) -> Vec<String> {
    let line_of = |id: &str| {
        lines
            .iter()
            .find(|(of, _)| of == id)
            .map_or(String::new(), |(_, line)| line.clone())
    };
    let mut said = Vec::new();
    for id in &changes.added {
        said.push(format!("+ {}", line_of(id)));
    }
    for id in &changes.changed {
        said.push(format!("~ {}", line_of(id)));
    }
    for id in &changes.removed {
        said.push(format!("- the line of `{id}`"));
    }
    said
}

/// That `agents_md` is the working copy `root`'s own: when it is a link, one
/// that leads to a file inside `root` (both canonical). A link that leads
/// outside it, or to nothing, is refused: writing would change a file
/// outside the working copy, or replace the link.
fn within(root: &Path, agents_md: &Path) -> Result<()> {
    let linked =
        std::fs::symlink_metadata(agents_md).is_ok_and(|meta| meta.file_type().is_symlink());
    if !linked {
        return Ok(());
    }
    let root = std::fs::canonicalize(root)
        .with_context(|| format!("cannot resolve {}", root.display()))?;
    match std::fs::canonicalize(agents_md) {
        Ok(target) if target.starts_with(&root) => Ok(()),
        Ok(target) => anyhow::bail!(
            "{} is a link that leads outside the working copy, to {}, so it is not written",
            agents_md.display(),
            target.display()
        ),
        Err(_) => anyhow::bail!(
            "{} is a link that leads to no file, so it is not written over",
            agents_md.display()
        ),
    }
}

/// `ohjaa lessons [<path>]`: the Lessons section of the AGENTS.md of the
/// working copy at or above `at`, brought in line with the repository's
/// knowledge (current nodes' lines in, retired and superseded ones' out),
/// for the person to commit and push. Nothing else is written, and nothing
/// is sent anywhere. AGENTS.md is read whole or not at all: a file that is
/// not UTF-8 is never written over. A repo node that could not be read
/// keeps whatever line AGENTS.md holds of it, and is said (exit 4).
pub fn write(at: &Path, dry_run: bool) -> Result<i32> {
    let (root, _) = gitconfig::repo_at(at)?;
    let dirs = Dirs::resolve(Some(&root))?;
    let loaded = knowledge::load(&dirs)?;
    let homes = [
        knowledge::accepted_dir(&dirs.state, &root),
        knowledge::repo_knowledge(&root),
    ];
    let broken: Vec<_> = loaded
        .broken
        .iter()
        .filter(|broken| homes.iter().any(|dir| broken.path.starts_with(dir)))
        .collect();
    for broken in &broken {
        eprintln!(
            "ohjaa lessons: unreadable {}: {}; AGENTS.md keeps what it holds of it",
            broken.path.display(),
            broken.reason
        );
    }
    for left in &loaded.shadowed {
        eprintln!(
            "ohjaa lessons: left {}: ohjaa's state holds that node, so this copy is not read; \
             take it out of the checkout",
            left.display()
        );
    }
    let repo: Vec<fk::Located> = loaded
        .nodes
        .iter()
        .filter(|located| located.scope == Scope::Repo)
        .cloned()
        .collect();
    let lines: Vec<(String, String)> = fk::briefing(&repo)
        .into_iter()
        .map(|located| (located.node.id.clone(), line(&located.node)))
        .collect();
    let gone: Vec<String> = repo
        .iter()
        .filter(|located| matches!(located.node.status, Status::Retired | Status::Superseded))
        .map(|located| located.node.id.clone())
        .collect();
    let agents_md = root.join("AGENTS.md");
    within(&root, &agents_md)?;
    let existing = match std::fs::read(&agents_md) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!(
                "{} is not UTF-8, so it is not written over",
                agents_md.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", agents_md.display()))
        }
    };
    let (text, changes) = with_lessons(&existing, &lines, &gone);
    let code = if broken.is_empty() {
        0
    } else {
        crate::INCOMPLETE
    };
    if text == existing {
        if lines.is_empty() && changes.removed.is_empty() {
            println!(
                "ohjaa lessons: no lesson is accepted for {}; nothing to write",
                root.display()
            );
        } else {
            println!(
                "ohjaa lessons: nothing to change: {} holds this repository's lessons ({})",
                agents_md.display(),
                lines.len()
            );
        }
        return Ok(code);
    }
    for line in said(&changes, &lines) {
        println!("  {line}");
    }
    if dry_run {
        println!(
            "ohjaa lessons: dry run; {} is unchanged",
            agents_md.display()
        );
        return Ok(code);
    }
    home::write_file(&agents_md, &text, None)?;
    println!(
        "ohjaa lessons: wrote the lessons into {}: review it, then commit and push it like any \
         change",
        agents_md.display()
    );
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use knowledge_node::{Date, Kind};

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
            ..Standing::default()
        };
        assert!(standing.in_agents_md(&rust));
        assert!(
            !standing.in_agents_md(&confirmed),
            "an older line is not the node as it is now"
        );
        assert_eq!(
            standing.note(&confirmed),
            "AGENTS.md holds another line of it"
        );
        assert_eq!(
            standing.note(&node("go", Kind::Decision, "Go.")),
            "not yet in AGENTS.md"
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
    fn a_marked_line_inside_a_fence_or_a_comment_is_not_held_by_the_file() {
        let found = lines_in(
            "```\n- <!-- lesson: fenced --> x\n```\n<!--\n- <!-- lesson: commented --> y\n-->\n\
             - <!-- lesson: real --> z\n",
        );
        assert_eq!(found.keys().collect::<Vec<_>>(), vec!["real"]);
    }
}
