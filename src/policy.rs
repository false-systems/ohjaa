//! The admission policy — the one part of this repo that is a human decision.
//!
//! Admission answers two things at once: may this agent start, and where. The
//! refusal is the first-class outcome; a worktree is what you get when the
//! answer is yes.

use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::scan::Session;

/// What the human is asking to start.
///
/// Every field is read by `admit` once it is written; until then the compiler
/// correctly sees them as unused.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Request {
    pub agent: String,
    pub intent: String,
    pub scope: Vec<String>,
    pub worktree: PathBuf,
}

/// The decision. `worktree` is where the agent runs; `None` is a refusal.
///
/// The `reason` is not decoration — it is printed either way, so a refusal
/// tells the human what to do next and an admission says what it observed.
#[derive(Debug, Clone)]
pub struct Admission {
    pub worktree: Option<PathBuf>,
    pub reason: String,
}

impl Admission {
    #[allow(dead_code)] // used once `admit` is written
    pub fn to(worktree: PathBuf, reason: impl Into<String>) -> Self {
        Admission {
            worktree: Some(worktree),
            reason: reason.into(),
        }
    }

    #[allow(dead_code)] // used once `admit` is written
    pub fn refused(reason: impl Into<String>) -> Self {
        Admission {
            worktree: None,
            reason: reason.into(),
        }
    }
}

/// True when two declared scopes could touch the same files.
///
/// An empty scope is *undeclared*, not empty — it could be anything, so it
/// intersects everything. Prefix comparison is coarse but honest: it
/// over-reports rather than missing a real collision.
///
/// Unused until `admit` is written; it is the tool that function needs.
#[allow(dead_code)]
pub fn scopes_intersect(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter()
        .any(|x| b.iter().any(|y| x.starts_with(y) || y.starts_with(x)))
}

/// Decide whether `request` may run, and where, given the live siblings.
///
/// Return `Ok(Admission::to(path, why))` to launch there, or
/// `Ok(Admission::refused(why))` to decline. You get every live session's
/// worktree, branch, declared scope and intent, plus [`scopes_intersect`].
/// `request.worktree` is where the human invoked.
///
/// TODO(yair): implement. This is deliberately unwritten — it encodes how much
/// friction is worth accepting to prevent collisions, and you are the one who
/// will feel it daily. The decisions it settles:
///
/// 1. Same worktree as a live sibling — always refuse? Two agents sharing one
///    working tree share an index. Gates stay safe (they run against an
///    isolated snapshot of a repository point) but edits collide. This is the
///    one case with no upside.
/// 2. Different worktree, intersecting scope — refuse, or allow and let a
///    merge-tree conflict proof catch a real collision later? Refusing is safe
///    but blocks the common "we will both brush `src/lib.rs`" case.
/// 3. Undeclared scope — [`scopes_intersect`] treats an empty scope as touching
///    everything, so an undeclared agent collides with everyone. Right pressure,
///    or too blunt?
/// 4. On refusal, offer a fresh worktree path rather than just saying no?
/// 5. Cross-repo scopes are real, not hypothetical: `ohjaa fleet` found a live
///    codex session in `vartio` declaring scope over `ahti`, `ruuma`, `jalki`,
///    `tapio`, and a path inside no repo at all. A sibling in another repo can
///    therefore declare scope reaching into yours. Does that count as
///    occupancy, and is `run` even the right place to check it?
pub fn admit(_request: &Request, _live: &[Session]) -> Result<Admission> {
    bail!(
        "admit is unimplemented: the admission policy is a human decision.\n\
         See the TODO in src/policy.rs — it lists the five calls it has to make."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn identical_paths_intersect() {
        assert!(scopes_intersect(
            &scope(&["src/gates.rs"]),
            &scope(&["src/gates.rs"])
        ));
    }

    #[test]
    fn disjoint_paths_do_not() {
        assert!(!scopes_intersect(
            &scope(&["src/a.rs"]),
            &scope(&["src/b.rs"])
        ));
    }

    #[test]
    fn a_directory_contains_its_files() {
        assert!(scopes_intersect(
            &scope(&["docs/"]),
            &scope(&["docs/proposals/0003.md"])
        ));
    }

    #[test]
    fn undeclared_scope_intersects_everything() {
        assert!(scopes_intersect(&[], &scope(&["src/a.rs"])));
        assert!(scopes_intersect(&scope(&["src/a.rs"]), &[]));
    }

    #[test]
    fn cross_repo_absolute_scope_reaches_in() {
        assert!(scopes_intersect(
            &scope(&["/Users/x/projects"]),
            &scope(&["/Users/x/projects/ahti/PROTOCOL.md"])
        ));
    }

    #[test]
    fn admit_is_unimplemented_on_purpose() {
        // Guards the human's decision: nobody should quietly fill this in.
        let request = Request {
            agent: "claude".into(),
            intent: "why".into(),
            scope: Vec::new(),
            worktree: PathBuf::from("/tmp/r"),
        };
        assert!(admit(&request, &[]).is_err());
    }
}
