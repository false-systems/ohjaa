//! Possible duplicates among the proposals a person reviews.
//!
//! Every proposal costs a person a review, so `ohjaa review` shows one that
//! may say again what is already present right after the one it may repeat,
//! marked so. A node has no title but its id; two ids that say mostly the
//! same words ([`near`]) are near. The mark is derived when a person reviews,
//! and stored never. A person decides whether it is one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use knowledge_node::{Node, Scope, Status};

use crate::knowledge::{self, Loaded};

/// A node a proposal may repeat: one in a scope directory, or a proposal
/// waiting for a person.
struct Present {
    id: String,
    scope: Scope,
    /// For a repo node: its repository.
    repo: Option<PathBuf>,
    status: Status,
    words: BTreeSet<String>,
}

impl Present {
    fn new(scope: Scope, repo: Option<&Path>, node: &Node) -> Present {
        Present {
            id: node.id.clone(),
            scope,
            repo: repo.filter(|_| scope == Scope::Repo).map(Path::to_path_buf),
            status: node.status,
            words: title_words(&node.id),
        }
    }

    /// Present as a lesson: current, or proposed and waiting. A superseded or
    /// retired node was put aside by a person, who may want to see it come
    /// back.
    fn holds(&self) -> bool {
        matches!(self.status, Status::Current | Status::Proposed)
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
fn title_words(id: &str) -> BTreeSet<String> {
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
/// `prove-with-ohjaa-gate-before-merging` only 2 of 4 and 5 (0.44).
fn near(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Option<usize> {
    let shared = a.intersection(b).count();
    let total = a.len() + b.len();
    (shared >= 2 && 10 * shared >= 3 * total).then(|| 2_000 * shared / total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::parse_date;
    use knowledge_node::Kind;

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
        // Near pairs of the kind an agent proposes, and one of a node
        // already current.
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
        // The nearest pairs of 131 real ids that are not one lesson.
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
        // One lesson in other words is the person's to see.
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

    fn waiting(scope: Scope, repo: Option<&str>, id: &str, day: &str) -> knowledge::Pending {
        knowledge::Pending {
            scope,
            repo: repo.map(PathBuf::from),
            path: PathBuf::from(format!("/proposals/{id}.md")),
            node: Node {
                as_of: parse_date(day).unwrap(),
                ..lesson(id, Status::Proposed, "Waiting.")
            },
            staged: true,
        }
    }

    #[test]
    fn a_waiting_proposal_is_shown_as_a_possible_duplicate_of_what_came_before_it() {
        use Scope::{Mine, Ours, Repo};
        let loaded = Loaded {
            nodes: [
                ("rust-elixir-only", Status::Current),
                ("leave-a-handover-always", Status::Retired),
            ]
            .into_iter()
            .map(|(id, status)| knowledge_node::Located {
                scope: Mine,
                node: lesson(id, status, "In a scope."),
            })
            .collect(),
            ..Loaded::default()
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
            waiting(Repo, Some("/r/quarry"), "quarry-linux-only", "2026-10-27"),
            waiting(Repo, Some("/r/beacon"), "beacon-linux-only", "2026-10-26"),
            waiting(
                Repo,
                Some("/r/beacon"),
                "beacon-is-linux-only",
                "2026-10-27",
            ),
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
                of("beacon-linux-only"),
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
                "quarry-linux-only",
                "beacon-linux-only",
                "beacon-is-linux-only",
                "start-sessions-through-ohjaa-run",
                "start-agents-through-ohjaa-run",
            ]
        );
    }

    /// What a proposal refines, current or waiting, or supersedes, it says
    /// again by design; of several near ones, the nearest is named, and of
    /// two as near, current knowledge first.
    #[test]
    fn a_refinement_is_no_duplicate_and_the_nearest_is_named() {
        use Scope::Mine;
        let loaded = Loaded {
            nodes: ["read-back-briefing", "rust-elixir-only"]
                .into_iter()
                .map(|id| knowledge_node::Located {
                    scope: Mine,
                    node: lesson(id, Status::Current, "In a scope."),
                })
                .collect(),
            ..Loaded::default()
        };
        let refining = |id: &str, refines: Option<&str>, supersedes: &[&str]| {
            let mut pending = waiting(Mine, None, id, "2026-10-27");
            pending.node.refines = refines.map(str::to_owned);
            pending.node.supersedes = supersedes.iter().map(|id| (*id).to_owned()).collect();
            pending
        };
        let alone = |pending: knowledge::Pending| {
            possible_duplicates(&loaded, &[pending], None)
                .pop()
                .flatten()
        };
        assert_eq!(
            alone(refining("read-the-briefing-back", None, &[])).as_deref(),
            Some("read-back-briefing")
        );
        assert_eq!(
            alone(refining(
                "read-back-briefing-in-codex",
                Some("read-back-briefing"),
                &[],
            )),
            None
        );
        assert_eq!(
            alone(refining(
                "read-back-briefing-first",
                None,
                &["read-back-briefing"]
            )),
            None
        );
        let tie = title_words("code-in-rust-or-elixir-only");
        assert_eq!(
            near(&tie, &title_words("rust-elixir-only")),
            near(&tie, &title_words("code-is-rust-or-elixir"))
        );
        let marks = possible_duplicates(
            &loaded,
            &[
                waiting(Mine, None, "code-is-rust-or-elixir", "2026-10-26"),
                refining("code-in-rust-or-elixir-only", None, &[]),
            ],
            None,
        );
        assert_eq!(marks[1].as_deref(), Some("rust-elixir-only"));
    }
}
