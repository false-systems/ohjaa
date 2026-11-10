//! Does knowledge work? What intake saw of each current node.
//!
//! The briefing gives agents knowledge; nothing else measures whether it
//! changes what they do. A person who corrects an agent on what a current
//! node already says is teaching it again: the drafter names that node for
//! the message (`reteaches`), and intake keeps it here, per node, by the
//! message's time, the digest intake keys it by, and its session, never its
//! words. A draft that refines or supersedes a current node *cites* it, and
//! the newest such time is kept too.
//!
//! `ohjaa list`, `check` and `review` say per node how often it was re-taught
//! since a person last accepted or confirmed it (its `as_of`), and when it was
//! last touched; `check` suggests rewording what keeps being re-taught and
//! asking whether what nothing touches still holds. Suggestions only: ohjaa
//! never changes a node itself.
//!
//! It is history
//! (`$XDG_STATE_HOME/ohjaa/knowledge-evidence.json`): deleting it loses no
//! knowledge, only the counts. A message answered for again (its progress
//! lost, or sent again with `--retry-failed`) is the same key, and counts
//! once. A damaged file is an error, never read as less and never replaced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use knowledge_node::{Date, Node, Scope};
use serde::{Deserialize, Serialize};

use crate::intake::Key;
use crate::knowledge::{self, day_number, Dirs};

/// The form of the file, its first field.
pub const SCHEMA: &str = "ohjaa-knowledge-evidence.v1";

/// Re-taught this often within [`RECENT_DAYS`]: the node is not working.
pub const RETAUGHT_OFTEN: usize = 3;
/// The window [`RETAUGHT_OFTEN`] is counted in.
pub const RECENT_DAYS: i64 = 30;
/// Nothing re-taught or cited it, and no person accepted or confirmed it,
/// for this long: it may no longer hold.
pub const IDLE_DAYS: i64 = 60;

const DAY_MS: i64 = 86_400_000;

/// Which node: its scope and id, and for a repo node the clone it is of
/// ([`knowledge::clone_key`]), so every worktree of a clone shares its count
/// and another repository's node of the same id has its own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeKey {
    pub scope: Scope,
    pub clone: Option<PathBuf>,
    pub id: String,
}

impl NodeKey {
    /// A node of `scope`; `repo` is the repository a repo node is of.
    pub fn new(scope: Scope, repo: Option<&Path>, id: &str) -> NodeKey {
        NodeKey {
            scope,
            clone: repo
                .filter(|_| scope == Scope::Repo)
                .map(knowledge::clone_key),
            id: id.to_owned(),
        }
    }
}

/// What was seen of one node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    /// Each message that re-taught it, by its key, with its session.
    retaught: BTreeMap<Key, Option<String>>,
    /// The newest time a draft cited it.
    cited_ms: Option<i64>,
}

/// Everything seen, per node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    nodes: BTreeMap<NodeKey, Record>,
}

#[derive(Serialize, Deserialize)]
struct File {
    schema: String,
    nodes: Vec<FileNode>,
}

#[derive(Serialize, Deserialize)]
struct FileNode {
    scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clone: Option<PathBuf>,
    id: String,
    retaught: Vec<FileEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cited_ms: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct FileEvent {
    at_ms: i64,
    /// The digest of the message's words ([`Key`]), in hex.
    digest: String,
    session: Option<String>,
}

impl Evidence {
    pub fn path(dirs: &Dirs) -> PathBuf {
        dirs.state.join("knowledge-evidence.json")
    }

    /// What was kept, or nothing when there is no file yet.
    pub fn read(dirs: &Dirs) -> Result<Evidence> {
        let path = Evidence::path(dirs);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Evidence::default())
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        Evidence::parse(&text).with_context(|| {
            format!(
                "{} is damaged; remove it to start over (it is history: the counts of \
                 re-teaching start again, and no knowledge is lost)",
                path.display()
            )
        })
    }

    fn parse(text: &str) -> Result<Evidence> {
        let file: File = serde_json::from_str(text).context("it is not its JSON")?;
        if file.schema != SCHEMA {
            bail!("it is `{}`, not {SCHEMA}", file.schema);
        }
        let mut evidence = Evidence::default();
        for node in file.nodes {
            let scope = Scope::from_name(&node.scope)
                .with_context(|| format!("`{}` is not a scope", node.scope))?;
            if (scope == Scope::Repo) != node.clone.is_some() {
                bail!(
                    "`{}`: only a repo node, and every one, names its clone",
                    node.id
                );
            }
            let mut record = Record {
                cited_ms: node.cited_ms,
                ..Record::default()
            };
            for event in node.retaught {
                let digest = Some(event.digest.as_str())
                    .filter(|d| d.len() == 16)
                    .and_then(|d| u64::from_str_radix(d, 16).ok())
                    .with_context(|| format!("`{}` is not a digest", event.digest))?;
                let key = Key {
                    at_ms: event.at_ms,
                    digest,
                };
                record.retaught.insert(key, event.session);
            }
            let key = NodeKey {
                scope,
                clone: node.clone,
                id: node.id,
            };
            if evidence.nodes.insert(key, record).is_some() {
                bail!("a node is in it twice");
            }
        }
        Ok(evidence)
    }

    /// Keep it whole or not at all.
    pub fn write(&self, dirs: &Dirs) -> Result<()> {
        std::fs::create_dir_all(&dirs.state)?;
        crate::intake::replace_durably(&Evidence::path(dirs), self.to_text().as_bytes())
    }

    fn to_text(&self) -> String {
        let nodes = self
            .nodes
            .iter()
            .map(|(key, record)| FileNode {
                scope: key.scope.name().to_owned(),
                clone: key.clone.clone(),
                id: key.id.clone(),
                retaught: record
                    .retaught
                    .iter()
                    .map(|(key, session)| FileEvent {
                        at_ms: key.at_ms,
                        digest: format!("{:016x}", key.digest),
                        session: session.clone(),
                    })
                    .collect(),
                cited_ms: record.cited_ms,
            })
            .collect();
        let file = File {
            schema: SCHEMA.to_owned(),
            nodes,
        };
        let mut text = serde_json::to_string_pretty(&file).expect("plain data serializes");
        text.push('\n');
        text
    }

    /// `node` re-taught by the message `key`, said in `session`. Whether it
    /// is new: a message counts once per node, however often it is answered
    /// for.
    pub fn retaught(&mut self, node: &NodeKey, key: Key, session: Option<&str>) -> bool {
        let record = self.nodes.entry(node.clone()).or_default();
        if record.retaught.contains_key(&key) {
            return false;
        }
        record.retaught.insert(key, session.map(str::to_owned));
        true
    }

    /// `node` cited as of `at_ms`. Whether that is newer than what was kept.
    pub fn cited(&mut self, node: &NodeKey, at_ms: i64) -> bool {
        let record = self.nodes.entry(node.clone()).or_default();
        if record.cited_ms.is_some_and(|cited| cited >= at_ms) {
            return false;
        }
        record.cited_ms = Some(at_ms);
        true
    }

    /// What was seen of the current `node` of `scope` (a repo one of `repo`),
    /// as of `today`.
    pub fn tally(&self, scope: Scope, repo: Option<&Path>, node: &Node, today: Date) -> Tally {
        let record = self.nodes.get(&NodeKey::new(scope, repo, &node.id));
        Tally::of(record, node.as_of, today)
    }
}

/// One node's evidence since a person last accepted or confirmed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Its `as_of`: when a person last accepted or confirmed it.
    pub since: Date,
    /// How often it was re-taught since.
    pub retaught: usize,
    /// Of those, how many in the last [`RECENT_DAYS`].
    pub recent: usize,
    /// The newest re-teaching or citation since, in milliseconds.
    pub last_ms: Option<i64>,
    /// Days since it was last touched: re-taught, cited, accepted or
    /// confirmed.
    pub idle_days: i64,
}

/// What `ohjaa check` suggests a person do with a node. Never done by ohjaa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suggestion {
    /// Re-taught [`RETAUGHT_OFTEN`] times or more in [`RECENT_DAYS`].
    Reword { recent: usize },
    /// Nothing touched it in [`IDLE_DAYS`].
    StillTrue,
}

impl Suggestion {
    pub fn describe(&self) -> String {
        match self {
            Suggestion::Reword { recent } => format!(
                "re-taught {recent} times in {RECENT_DAYS} days: reword it, or make it a decision"
            ),
            Suggestion::StillTrue => {
                format!("nothing has re-taught or cited it in {IDLE_DAYS} days: still true?")
            }
        }
    }
}

impl Tally {
    /// Only what came on or after the day of `as_of` counts: a message said
    /// before a person made the node current (its origin, say) did not
    /// re-teach it.
    pub fn of(record: Option<&Record>, as_of: Date, today: Date) -> Tally {
        let since_day = day_number(as_of);
        let today_day = day_number(today);
        let day = |at_ms: i64| at_ms.div_euclid(DAY_MS);
        let mut tally = Tally {
            since: as_of,
            retaught: 0,
            recent: 0,
            last_ms: None,
            idle_days: today_day - since_day,
        };
        let Some(record) = record else {
            return tally;
        };
        let counted: Vec<i64> = record
            .retaught
            .keys()
            .map(|key| key.at_ms)
            .filter(|at| day(*at) >= since_day)
            .collect();
        tally.retaught = counted.len();
        tally.recent = counted
            .iter()
            .filter(|at| today_day - day(**at) < RECENT_DAYS)
            .count();
        tally.last_ms = counted
            .iter()
            .copied()
            .chain(record.cited_ms.filter(|at| day(*at) >= since_day))
            .max();
        if let Some(last) = tally.last_ms {
            tally.idle_days = today_day - day(last).max(since_day);
        }
        tally
    }

    /// `re-taught N times since <as_of>, last <date>`; with no `last` when
    /// nothing touched it since.
    pub fn line(&self) -> String {
        let times = match self.retaught {
            1 => "1 time".to_owned(),
            n => format!("{n} times"),
        };
        let mut line = format!("re-taught {times} since {}", self.since);
        if let Some(last) = self.last_ms {
            let date = knowledge::date_from_day_number(last.div_euclid(DAY_MS));
            line.push_str(&format!(", last {date}"));
        }
        line
    }

    pub fn suggestion(&self) -> Option<Suggestion> {
        if self.recent >= RETAUGHT_OFTEN {
            Some(Suggestion::Reword {
                recent: self.recent,
            })
        } else if self.idle_days >= IDLE_DAYS {
            Some(Suggestion::StillTrue)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::parse_date;

    fn date(text: &str) -> Date {
        parse_date(text).unwrap()
    }

    /// Milliseconds at noon UTC of `text`'s day.
    fn noon(text: &str) -> i64 {
        day_number(date(text)) * DAY_MS + DAY_MS / 2
    }

    fn sandbox(name: &str) -> Dirs {
        let root =
            std::env::temp_dir().join(format!("ohjaa-evidence-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Dirs {
            ours_checkout: None,
            repo_root: None,
            mine: root.join("config/mine"),
            machine: root.join("config/machine"),
            proposals: root.join("state/proposals"),
            state: root.join("state"),
        }
    }

    #[test]
    fn a_message_counts_once_per_node_however_often_it_is_answered_for() {
        let dirs = sandbox("once");
        let rust = NodeKey::new(Scope::Mine, None, "rust-elixir-only");
        let lima = NodeKey::new(Scope::Machine, None, "linux-work-in-lima");
        let said = Key {
            at_ms: noon("2026-09-21"),
            digest: 0x0123_4567_89ab_cdef,
        };
        let again = Key {
            at_ms: noon("2026-09-22"),
            digest: 0xfedc_ba98_7654_3210,
        };
        let mut evidence = Evidence::default();
        assert!(evidence.retaught(&rust, said, Some("s1")));
        assert!(
            !evidence.retaught(&rust, said, Some("s1")),
            "the same message"
        );
        assert!(
            !evidence.retaught(&rust, said, None),
            "whatever session is said"
        );
        assert!(evidence.retaught(&rust, again, Some("s2")));
        // One message may re-teach two nodes: once each.
        assert!(evidence.retaught(&lima, said, Some("s1")));
        assert!(evidence.cited(&lima, noon("2026-09-23")));
        assert!(
            !evidence.cited(&lima, noon("2026-09-22")),
            "only a newer citation"
        );
        evidence.write(&dirs).unwrap();
        let text = std::fs::read_to_string(Evidence::path(&dirs)).unwrap();
        assert_eq!(Evidence::read(&dirs).unwrap(), evidence, "{text}");
        assert!(text.contains("\"digest\": \"0123456789abcdef\""), "{text}");
        assert!(text.contains("\"session\": \"s2\""), "{text}");
        // Written again unchanged, it is the same file.
        evidence.write(&dirs).unwrap();
        assert_eq!(
            std::fs::read_to_string(Evidence::path(&dirs)).unwrap(),
            text
        );

        let node = |id: &str, as_of: &str| Node {
            id: id.to_owned(),
            kind: knowledge_node::Kind::Decision,
            status: knowledge_node::Status::Current,
            as_of: date(as_of),
            origin: vec!["a test".to_owned()],
            supersedes: Vec::new(),
            refines: None,
            holds_while: Vec::new(),
            body: "A rule.".to_owned(),
        };
        let today = date("2026-09-24");
        let tally = evidence.tally(
            Scope::Mine,
            None,
            &node("rust-elixir-only", "2026-09-20"),
            today,
        );
        assert_eq!(
            tally.line(),
            "re-taught 2 times since 2026-09-20, last 2026-09-22"
        );
        // Of another scope, the same id is another node.
        let tally = evidence.tally(
            Scope::Ours,
            None,
            &node("rust-elixir-only", "2026-09-20"),
            today,
        );
        assert_eq!(tally.line(), "re-taught 0 times since 2026-09-20");
        let tally = evidence.tally(
            Scope::Machine,
            None,
            &node("linux-work-in-lima", "2026-09-20"),
            today,
        );
        assert_eq!(
            tally.line(),
            "re-taught 1 time since 2026-09-20, last 2026-09-23"
        );
        let _ = std::fs::remove_dir_all(dirs.state.parent().unwrap());
    }

    #[test]
    fn damaged_evidence_is_an_error_never_read_as_less() {
        let dirs = sandbox("damaged");
        assert_eq!(
            Evidence::read(&dirs).unwrap(),
            Evidence::default(),
            "none yet"
        );
        std::fs::create_dir_all(&dirs.state).unwrap();
        let event = r#"{"at_ms": 1, "digest": "0123456789abcdef", "session": null}"#;
        for damaged in [
            String::new(),
            "{\"schema\":\"ohjaa-knowledge-evidence.v1\",\"nodes\":[".to_owned(),
            r#"{"schema":"ohjaa-knowledge-evidence.v0","nodes":[]}"#.to_owned(),
            format!(
                r#"{{"schema":"{SCHEMA}","nodes":[{{"scope":"nowhere","id":"a","retaught":[]}}]}}"#
            ),
            format!(
                r#"{{"schema":"{SCHEMA}","nodes":[{{"scope":"repo","id":"a","retaught":[]}}]}}"#
            ),
            format!(
                r#"{{"schema":"{SCHEMA}","nodes":[{{"scope":"mine","id":"a","retaught":[{{"at_ms":1,"digest":"xyz","session":null}}]}}]}}"#
            ),
            format!(
                r#"{{"schema":"{SCHEMA}","nodes":[{{"scope":"mine","id":"a","retaught":[{event}]}},{{"scope":"mine","id":"a","retaught":[]}}]}}"#
            ),
        ] {
            std::fs::write(Evidence::path(&dirs), &damaged).unwrap();
            let error = format!("{:#}", Evidence::read(&dirs).unwrap_err());
            assert!(
                error.contains("knowledge-evidence.json is damaged; remove it to start over"),
                "{damaged}: {error}"
            );
        }
        let _ = std::fs::remove_dir_all(dirs.state.parent().unwrap());
    }

    #[test]
    fn a_node_is_counted_since_a_person_last_made_it_current_and_suggested_on() {
        let today = date("2026-10-28");
        let mut record = Record::default();
        let mut say = |day: &str, digest: u64| {
            record.retaught.insert(
                Key {
                    at_ms: noon(day),
                    digest,
                },
                None,
            );
        };
        // Its origin, said before it was accepted; one 38 days back; two in
        // the last 30 days.
        say("2026-08-01", 1);
        say("2026-09-20", 2);
        say("2026-10-20", 3);
        say("2026-10-27", 4);
        let tally = Tally::of(Some(&record), date("2026-08-02"), today);
        assert_eq!((tally.retaught, tally.recent), (3, 2));
        assert_eq!(
            tally.line(),
            "re-taught 3 times since 2026-08-02, last 2026-10-27"
        );
        assert_eq!(
            tally.suggestion(),
            None,
            "twice in 30 days is not yet often"
        );
        // Three times inside 30 days (27 days back is inside): not working.
        let mut often = record.clone();
        often.retaught.insert(
            Key {
                at_ms: noon("2026-10-01"),
                digest: 5,
            },
            None,
        );
        let tally = Tally::of(Some(&often), date("2026-08-02"), today);
        assert_eq!((tally.retaught, tally.recent), (4, 3));
        assert_eq!(
            tally.suggestion().map(|s| s.describe()).as_deref(),
            Some("re-taught 3 times in 30 days: reword it, or make it a decision")
        );
        // Confirmed since: only what came after counts.
        let tally = Tally::of(Some(&often), date("2026-10-21"), today);
        assert_eq!(
            tally.line(),
            "re-taught 1 time since 2026-10-21, last 2026-10-27"
        );
        assert_eq!(tally.suggestion(), None);

        // Nothing touched it in 60 days: still true? A citation is a touch,
        // and so is a person accepting or confirming it.
        let idle = Tally::of(None, date("2026-08-29"), today);
        assert_eq!(idle.idle_days, 60);
        assert_eq!(
            idle.suggestion().map(|s| s.describe()).as_deref(),
            Some("nothing has re-taught or cited it in 60 days: still true?")
        );
        assert_eq!(
            Tally::of(None, date("2026-08-30"), today).suggestion(),
            None
        );
        let cited = Record {
            cited_ms: Some(noon("2026-10-01")),
            ..Record::default()
        };
        let tally = Tally::of(Some(&cited), date("2026-06-01"), today);
        assert_eq!(
            tally.line(),
            "re-taught 0 times since 2026-06-01, last 2026-10-01"
        );
        assert_eq!(tally.suggestion(), None);
        // A citation before it was confirmed is no touch since.
        let tally = Tally::of(Some(&cited), date("2026-10-02"), today);
        assert_eq!(tally.line(), "re-taught 0 times since 2026-10-02");
    }
}
