//! `knowledge-node.v1`: one unit of Knowledge per file (see `docs/ontology.md`).
//!
//! Parsing, validation and canonical writing only. This crate does no IO, never
//! evaluates a predicate, and never renders a briefing: those belong to the
//! tool. What a valid node is, and which nodes a briefing contains in what
//! order, belong here. See `docs/knowledge-node-v1.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The contract this crate implements.
pub const FORMAT: &str = "knowledge-node.v1";

/// Longest id the format allows.
pub const MAX_ID_LEN: usize = 64;

/// Whose Knowledge it is. Declared from most to least shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    Ours,
    Repo,
    Mine,
    Machine,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Ours, Scope::Repo, Scope::Mine, Scope::Machine];

    pub fn name(self) -> &'static str {
        match self {
            Scope::Ours => "ours",
            Scope::Repo => "repo",
            Scope::Mine => "mine",
            Scope::Machine => "machine",
        }
    }

    pub fn from_name(name: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|scope| scope.name() == name)
    }

    /// `ours` and `repo` change by review; `mine` and `machine` by their owner.
    pub fn is_shared(self) -> bool {
        matches!(self, Scope::Ours | Scope::Repo)
    }
}

/// Declared in briefing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Decision,
    Fact,
    Taste,
    Lesson,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Decision, Kind::Fact, Kind::Taste, Kind::Lesson];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Decision => "decision",
            Kind::Fact => "fact",
            Kind::Taste => "taste",
            Kind::Lesson => "lesson",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Proposed,
    Current,
    Superseded,
    Retired,
}

impl Status {
    pub const ALL: [Status; 4] = [
        Status::Proposed,
        Status::Current,
        Status::Superseded,
        Status::Retired,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Current => "current",
            Status::Superseded => "superseded",
            Status::Retired => "retired",
        }
    }

    pub fn from_name(name: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|status| status.name() == name)
    }
}

/// A calendar date, the only precision `as_of` carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// `None` for a date that does not exist, such as 2026-02-29.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Date> {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days)
            .contains(&day)
            .then_some(Date { year, month, day })
    }

    pub fn year(self) -> u16 {
        self.year
    }

    pub fn month(self) -> u8 {
        self.month
    }

    pub fn day(self) -> u8 {
        self.day
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A fact that must stay true for a node to hold. Evaluated by the tool.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Predicate {
    /// An executable with this name is on `PATH`.
    Command(String),
    /// This path exists: absolute, `~/`-relative, or relative to the project.
    Path(String),
}

impl Predicate {
    pub fn parse(text: &str) -> Option<Predicate> {
        let (kind, argument) = text.split_once(':')?;
        match kind {
            "command" if valid_command(argument) => Some(Predicate::Command(argument.to_owned())),
            "path" if valid_path(argument) => Some(Predicate::Path(argument.to_owned())),
            _ => None,
        }
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Predicate::Command(name) => write!(f, "command:{name}"),
            Predicate::Path(path) => write!(f, "path:{path}"),
        }
    }
}

fn valid_command(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') {
        return false;
    }
    let rest = if let Some(rest) = path.strip_prefix("~/") {
        rest
    } else if path.starts_with('~') {
        return false;
    } else {
        path.strip_prefix('/').unwrap_or(path)
    };
    !rest.is_empty() && rest.split('/').all(|part| part != "..")
}

/// One unit of Knowledge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    pub as_of: Date,
    pub origin: Vec<String>,
    pub supersedes: Vec<String>,
    pub refines: Option<String>,
    pub holds_while: Vec<Predicate>,
    pub body: String,
}

/// A node together with the scope it was loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    pub scope: Scope,
    pub node: Node,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    MissingFrontmatter,
    BadToml,
    UnknownField,
    MissingField,
    WrongType,
    BadId,
    IdMismatch,
    BadKind,
    BadStatus,
    BadDate,
    EmptyOrigin,
    EmptyBody,
    BadPredicate,
    SelfReference,
    DuplicateReference,
    DuplicateId,
    DanglingReference,
    CrossScopeSupersedes,
    RefinesNarrowerScope,
    SupersedesLiveNode,
    UnexplainedSupersession,
    Cycle,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 22] = [
        ErrorCode::MissingFrontmatter,
        ErrorCode::BadToml,
        ErrorCode::UnknownField,
        ErrorCode::MissingField,
        ErrorCode::WrongType,
        ErrorCode::BadId,
        ErrorCode::IdMismatch,
        ErrorCode::BadKind,
        ErrorCode::BadStatus,
        ErrorCode::BadDate,
        ErrorCode::EmptyOrigin,
        ErrorCode::EmptyBody,
        ErrorCode::BadPredicate,
        ErrorCode::SelfReference,
        ErrorCode::DuplicateReference,
        ErrorCode::DuplicateId,
        ErrorCode::DanglingReference,
        ErrorCode::CrossScopeSupersedes,
        ErrorCode::RefinesNarrowerScope,
        ErrorCode::SupersedesLiveNode,
        ErrorCode::UnexplainedSupersession,
        ErrorCode::Cycle,
    ];

    /// The stable string the fixtures and tools use.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::MissingFrontmatter => "missing-frontmatter",
            ErrorCode::BadToml => "bad-toml",
            ErrorCode::UnknownField => "unknown-field",
            ErrorCode::MissingField => "missing-field",
            ErrorCode::WrongType => "wrong-type",
            ErrorCode::BadId => "bad-id",
            ErrorCode::IdMismatch => "id-mismatch",
            ErrorCode::BadKind => "bad-kind",
            ErrorCode::BadStatus => "bad-status",
            ErrorCode::BadDate => "bad-date",
            ErrorCode::EmptyOrigin => "empty-origin",
            ErrorCode::EmptyBody => "empty-body",
            ErrorCode::BadPredicate => "bad-predicate",
            ErrorCode::SelfReference => "self-reference",
            ErrorCode::DuplicateReference => "duplicate-reference",
            ErrorCode::DuplicateId => "duplicate-id",
            ErrorCode::DanglingReference => "dangling-reference",
            ErrorCode::CrossScopeSupersedes => "cross-scope-supersedes",
            ErrorCode::RefinesNarrowerScope => "refines-narrower-scope",
            ErrorCode::SupersedesLiveNode => "supersedes-live-node",
            ErrorCode::UnexplainedSupersession => "unexplained-supersession",
            ErrorCode::Cycle => "cycle",
        }
    }

    pub fn from_name(code: &str) -> Option<ErrorCode> {
        ErrorCode::ALL
            .into_iter()
            .find(|known| known.as_str() == code)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why one file is not a valid node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeError {
    pub code: ErrorCode,
    pub detail: String,
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for NodeError {}

/// Why a set of individually valid nodes does not hold together.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SetError {
    pub id: String,
    pub code: ErrorCode,
    pub detail: String,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.id, self.detail)
    }
}

fn fail<T>(code: ErrorCode, detail: impl Into<String>) -> Result<T, NodeError> {
    Err(NodeError {
        code,
        detail: detail.into(),
    })
}

/// `[a-z0-9]+(-[a-z0-9]+)*`, at most [`MAX_ID_LEN`] characters.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

const FIELDS: [&str; 8] = [
    "id",
    "kind",
    "status",
    "as_of",
    "origin",
    "supersedes",
    "refines",
    "holds_while",
];
const REQUIRED: [&str; 5] = ["id", "kind", "status", "as_of", "origin"];

/// Parse one node from the text of `<stem>.md`.
pub fn parse_node(stem: &str, text: &str) -> Result<Node, NodeError> {
    let (frontmatter, body) = split(text)?;
    let table: toml::Table = match frontmatter.parse() {
        Ok(table) => table,
        Err(error) => return fail(ErrorCode::BadToml, error.to_string().trim().to_owned()),
    };

    if let Some(unknown) = table
        .keys()
        .filter(|key| !FIELDS.contains(&key.as_str()))
        .min()
    {
        return fail(
            ErrorCode::UnknownField,
            format!("`{unknown}` is not a {FORMAT} field"),
        );
    }
    if let Some(missing) = REQUIRED.iter().find(|field| !table.contains_key(**field)) {
        return fail(ErrorCode::MissingField, format!("`{missing}` is required"));
    }

    let id = string(&table, "id")?;
    if !is_valid_id(&id) {
        return fail(ErrorCode::BadId, format!("`{id}` is not a valid id"));
    }
    if id != stem {
        return fail(
            ErrorCode::IdMismatch,
            format!("id `{id}` in a file named `{stem}.md`"),
        );
    }

    let kind_name = string(&table, "kind")?;
    let Some(kind) = Kind::from_name(&kind_name) else {
        return fail(ErrorCode::BadKind, format!("`{kind_name}` is not a kind"));
    };
    let status_name = string(&table, "status")?;
    let Some(status) = Status::from_name(&status_name) else {
        return fail(
            ErrorCode::BadStatus,
            format!("`{status_name}` is not a status"),
        );
    };
    let as_of = date(&table)?;

    let origin = strings(&table, "origin")?;
    if origin.is_empty() || origin.iter().any(|reason| reason.trim().is_empty()) {
        return fail(
            ErrorCode::EmptyOrigin,
            "origin must say why this Knowledge exists",
        );
    }

    let supersedes = strings(&table, "supersedes")?;
    let mut seen = BTreeSet::new();
    for target in &supersedes {
        reference(&id, target, "supersedes")?;
        if !seen.insert(target.as_str()) {
            return fail(
                ErrorCode::DuplicateReference,
                format!("supersedes `{target}` twice"),
            );
        }
    }

    let refines = match table.get("refines") {
        None => None,
        Some(_) => {
            let target = string(&table, "refines")?;
            reference(&id, &target, "refines")?;
            Some(target)
        }
    };

    let mut holds_while = Vec::new();
    for text in strings(&table, "holds_while")? {
        match Predicate::parse(&text) {
            Some(predicate) => holds_while.push(predicate),
            None => {
                return fail(
                    ErrorCode::BadPredicate,
                    format!("`{text}` is not a predicate"),
                )
            }
        }
    }

    if body.is_empty() {
        return fail(
            ErrorCode::EmptyBody,
            "the body is what an Agent reads; it must not be empty",
        );
    }

    Ok(Node {
        id,
        kind,
        status,
        as_of,
        origin,
        supersedes,
        refines,
        holds_while,
        body: body.to_owned(),
    })
}

/// Separate the frontmatter from the trimmed body.
fn split(text: &str) -> Result<(String, &str), NodeError> {
    let mut rest = text;
    let mut lines = Vec::new();
    let mut opened = false;
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(end) => (&rest[..end], &rest[end + 1..]),
            None => (rest, ""),
        };
        rest = next;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !opened {
            if line != "+++" {
                break;
            }
            opened = true;
        } else if line == "+++" {
            return Ok((lines.join("\n"), rest.trim()));
        } else {
            lines.push(line);
        }
    }
    fail(
        ErrorCode::MissingFrontmatter,
        "a node starts with a `+++` line and closes its frontmatter with another",
    )
}

fn string(table: &toml::Table, field: &str) -> Result<String, NodeError> {
    match table.get(field) {
        Some(toml::Value::String(text)) => Ok(text.clone()),
        Some(other) => fail(
            ErrorCode::WrongType,
            format!("`{field}` must be a string, not {}", other.type_str()),
        ),
        None => fail(ErrorCode::MissingField, format!("`{field}` is required")),
    }
}

/// An absent optional array is empty.
fn strings(table: &toml::Table, field: &str) -> Result<Vec<String>, NodeError> {
    let Some(value) = table.get(field) else {
        return Ok(Vec::new());
    };
    let wrong = || {
        fail(
            ErrorCode::WrongType,
            format!("`{field}` must be an array of strings"),
        )
    };
    let toml::Value::Array(items) = value else {
        return wrong();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            toml::Value::String(text) => out.push(text.clone()),
            _ => return wrong(),
        }
    }
    Ok(out)
}

fn date(table: &toml::Table) -> Result<Date, NodeError> {
    let bad = |detail: &str| fail(ErrorCode::BadDate, format!("as_of: {detail}"));
    let Some(toml::Value::Datetime(datetime)) = table.get("as_of") else {
        return bad("must be an unquoted TOML date such as 2026-09-24");
    };
    if datetime.time.is_some() || datetime.offset.is_some() {
        return bad("must be a date only, with no time or offset");
    }
    let Some(day) = datetime.date else {
        return bad("must be a date");
    };
    match Date::new(day.year, day.month, day.day) {
        Some(date) => Ok(date),
        None => bad("is not a real calendar date"),
    }
}

fn reference(own: &str, target: &str, field: &str) -> Result<(), NodeError> {
    if !is_valid_id(target) {
        return fail(
            ErrorCode::BadId,
            format!("{field} `{target}` is not a valid id"),
        );
    }
    if target == own {
        return fail(
            ErrorCode::SelfReference,
            format!("a node cannot {field} itself"),
        );
    }
    Ok(())
}

/// A TOML basic string that always stays on one line. The frontmatter ends at
/// the first `+++` line whatever the TOML string state, so a multi-line string
/// holding such a line would cut the node short: canonical form never writes one.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write a node in canonical form. `parse_node` of the result gives it back.
pub fn to_text(node: &Node) -> String {
    let list = |items: &[String]| {
        let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
        format!("[{}]", quoted.join(", "))
    };

    let mut out = String::from("+++\n");
    out.push_str(&format!("id = {}\n", quote(&node.id)));
    out.push_str(&format!("kind = {}\n", quote(node.kind.name())));
    out.push_str(&format!("status = {}\n", quote(node.status.name())));
    out.push_str(&format!("as_of = {}\n", node.as_of));
    out.push_str(&format!("origin = {}\n", list(&node.origin)));
    if !node.supersedes.is_empty() {
        out.push_str(&format!("supersedes = {}\n", list(&node.supersedes)));
    }
    if let Some(target) = &node.refines {
        out.push_str(&format!("refines = {}\n", quote(target)));
    }
    if !node.holds_while.is_empty() {
        let predicates: Vec<String> = node.holds_while.iter().map(ToString::to_string).collect();
        out.push_str(&format!("holds_while = {}\n", list(&predicates)));
    }
    out.push_str("+++\n");
    out.push_str(node.body.trim());
    out.push('\n');
    out
}

/// Check the rules that span nodes. Sorted, so the same set always reports the
/// same errors in the same order.
pub fn validate_set(nodes: &[Located]) -> Vec<SetError> {
    let mut errors = BTreeSet::new();
    let mut add = |id: &str, code: ErrorCode, detail: String| {
        errors.insert(SetError {
            id: id.to_owned(),
            code,
            detail,
        });
    };

    let mut by_id: BTreeMap<&str, Vec<&Located>> = BTreeMap::new();
    for located in nodes {
        by_id
            .entry(located.node.id.as_str())
            .or_default()
            .push(located);
    }
    for (id, found) in &by_id {
        if found.len() > 1 {
            let scopes: Vec<&str> = found.iter().map(|l| l.scope.name()).collect();
            add(
                id,
                ErrorCode::DuplicateId,
                format!("defined in {}", scopes.join(", ")),
            );
        }
    }
    // A duplicated id cannot be resolved; it is already reported above.
    let unique: BTreeMap<&str, &Located> = by_id
        .iter()
        .filter(|(_, found)| found.len() == 1)
        .map(|(id, found)| (*id, found[0]))
        .collect();

    let mut successors: BTreeSet<&str> = BTreeSet::new();
    for located in nodes {
        let node = &located.node;
        for target in &node.supersedes {
            successors.insert(target.as_str());
            if !by_id.contains_key(target.as_str()) {
                add(
                    &node.id,
                    ErrorCode::DanglingReference,
                    format!("supersedes `{target}`, which is not in the set"),
                );
                continue;
            }
            let Some(old) = unique.get(target.as_str()) else {
                continue;
            };
            if old.scope != located.scope {
                add(
                    &node.id,
                    ErrorCode::CrossScopeSupersedes,
                    format!(
                        "a {} node cannot supersede `{target}` in {}",
                        located.scope.name(),
                        old.scope.name()
                    ),
                );
            } else if matches!(node.status, Status::Current | Status::Superseded)
                && old.node.status != Status::Superseded
            {
                add(
                    &node.id,
                    ErrorCode::SupersedesLiveNode,
                    format!(
                        "supersedes `{target}`, which is still {}",
                        old.node.status.name()
                    ),
                );
            }
        }
        if let Some(target) = &node.refines {
            if !by_id.contains_key(target.as_str()) {
                add(
                    &node.id,
                    ErrorCode::DanglingReference,
                    format!("refines `{target}`, which is not in the set"),
                );
            } else if let Some(base) = unique.get(target.as_str()) {
                if base.scope > located.scope {
                    add(
                        &node.id,
                        ErrorCode::RefinesNarrowerScope,
                        format!(
                            "a {} node cannot build on `{target}` in {}",
                            located.scope.name(),
                            base.scope.name()
                        ),
                    );
                }
            }
        }
    }

    for located in nodes {
        let node = &located.node;
        if node.status == Status::Superseded && !successors.contains(node.id.as_str()) {
            add(
                &node.id,
                ErrorCode::UnexplainedSupersession,
                "superseded, but no node supersedes it".to_owned(),
            );
        }
    }

    for id in on_cycles(&unique) {
        add(
            id,
            ErrorCode::Cycle,
            "reaches itself through supersedes or refines".to_owned(),
        );
    }

    errors.into_iter().collect()
}

/// Ids that can reach themselves through resolvable `supersedes`/`refines` edges.
fn on_cycles<'a>(unique: &BTreeMap<&'a str, &'a Located>) -> Vec<&'a str> {
    let edges = |id: &str| -> Vec<&'a str> {
        let node = &unique[id].node;
        node.supersedes
            .iter()
            .map(String::as_str)
            .chain(node.refines.as_deref())
            .filter_map(|target| unique.get_key_value(target).map(|(key, _)| *key))
            .collect()
    };
    let mut cyclic = Vec::new();
    for &start in unique.keys() {
        let mut stack = edges(start);
        let mut visited = BTreeSet::new();
        while let Some(next) = stack.pop() {
            if next == start {
                cyclic.push(start);
                break;
            }
            if visited.insert(next) {
                stack.extend(edges(next));
            }
        }
    }
    cyclic
}

/// The nodes an Agent reads before acting: exactly the `current` ones, ordered
/// by scope, then kind, then id.
pub fn briefing(nodes: &[Located]) -> Vec<&Located> {
    let mut current: Vec<&Located> = nodes
        .iter()
        .filter(|located| located.node.status == Status::Current)
        .collect();
    current.sort_by(|a, b| {
        (a.scope, a.node.kind, &a.node.id).cmp(&(b.scope, b.node.kind, &b.node.id))
    });
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> Node {
        Node {
            id: id.to_owned(),
            kind: Kind::Fact,
            status: Status::Current,
            as_of: Date::new(2026, 9, 24).unwrap(),
            origin: vec!["a reason".to_owned()],
            supersedes: Vec::new(),
            refines: None,
            holds_while: Vec::new(),
            body: "Some knowledge.".to_owned(),
        }
    }

    #[test]
    fn canonical_form_round_trips_every_field() {
        let mut full = node("full");
        full.kind = Kind::Decision;
        full.status = Status::Proposed;
        full.origin = vec![
            "quote \"with\" escapes\\".to_owned(),
            "a second, reason".to_owned(),
        ];
        full.supersedes = vec!["older".to_owned(), "oldest".to_owned()];
        full.refines = Some("principle".to_owned());
        full.holds_while = vec![
            Predicate::Command("limactl".to_owned()),
            Predicate::Path("~/.lima".to_owned()),
        ];
        full.body = "Line one.\n\nLine three, with +++ inside.".to_owned();
        let text = to_text(&full);
        assert_eq!(parse_node("full", &text), Ok(full));
        // Writing is a fixed point: an edit tool never produces spurious diffs.
        assert_eq!(to_text(&parse_node("full", &text).unwrap()), text);
    }

    #[test]
    fn canonical_form_keeps_every_value_on_one_line() {
        let mut awkward = node("awkward");
        // A multi-line TOML string holding a `+++` line would close the
        // frontmatter early; so would a raw control character.
        awkward.origin = vec![
            "line one\n+++\nline three".to_owned(),
            "tab\there, cr\r, bell\u{7}, del\u{7f}".to_owned(),
        ];
        let text = to_text(&awkward);
        assert_eq!(
            text.lines().filter(|line| *line == "+++").count(),
            2,
            "{text}"
        );
        assert_eq!(parse_node("awkward", &text), Ok(awkward));
    }

    #[test]
    fn minimal_node_omits_empty_optional_fields() {
        let text = to_text(&node("small"));
        assert!(!text.contains("supersedes"));
        assert!(!text.contains("refines"));
        assert!(!text.contains("holds_while"));
        assert_eq!(parse_node("small", &text), Ok(node("small")));
    }

    #[test]
    fn dates_are_real_calendar_dates() {
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(2026, 2, 29).is_none());
        assert!(Date::new(1900, 2, 29).is_none());
        assert!(Date::new(2000, 2, 29).is_some());
        assert!(Date::new(2026, 4, 31).is_none());
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 1, 0).is_none());
    }

    #[test]
    fn ids_follow_the_syntax() {
        for good in [
            "a",
            "lima-for-linux",
            "rule-2",
            "x".repeat(MAX_ID_LEN).as_str(),
        ] {
            assert!(is_valid_id(good), "{good}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "a--b",
            "A",
            "a_b",
            "a.b",
            "x".repeat(MAX_ID_LEN + 1).as_str(),
        ] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn predicates_refuse_escapes_and_ambiguity() {
        assert_eq!(
            Predicate::parse("command:limactl"),
            Some(Predicate::Command("limactl".into()))
        );
        assert_eq!(
            Predicate::parse("path:/etc/hosts"),
            Some(Predicate::Path("/etc/hosts".into()))
        );
        assert_eq!(
            Predicate::parse("path:docs/a.md"),
            Some(Predicate::Path("docs/a.md".into()))
        );
        for bad in [
            "command:",
            "command:a/b",
            "command:a b",
            "path:",
            "path:/",
            "path:~",
            "path:~other/x",
            "path:../x",
            "path:a/../b",
            "path:~/..",
            "repo-active:widget",
            "limactl",
        ] {
            assert_eq!(Predicate::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn every_error_code_has_one_stable_name() {
        let names: BTreeSet<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
        assert_eq!(names.len(), ErrorCode::ALL.len());
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_name(code.as_str()), Some(code));
        }
    }

    #[test]
    fn briefing_holds_only_current_nodes_in_contract_order() {
        let mut lesson = node("b-lesson");
        lesson.kind = Kind::Lesson;
        let mut decision = node("z-decision");
        decision.kind = Kind::Decision;
        let mut proposed = node("proposed");
        proposed.status = Status::Proposed;
        let set = vec![
            Located {
                scope: Scope::Mine,
                node: node("a-mine"),
            },
            Located {
                scope: Scope::Ours,
                node: lesson,
            },
            Located {
                scope: Scope::Ours,
                node: decision,
            },
            Located {
                scope: Scope::Ours,
                node: proposed,
            },
            Located {
                scope: Scope::Ours,
                node: node("c-fact"),
            },
        ];
        let order: Vec<&str> = briefing(&set).iter().map(|l| l.node.id.as_str()).collect();
        assert_eq!(order, ["z-decision", "c-fact", "b-lesson", "a-mine"]);
    }
}
