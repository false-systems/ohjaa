//! The admission policy — the one part of this repo that is a human decision.
//!
//! Admission answers two things at once: may this agent start, and where. The
//! refusal is the first-class outcome; a worktree is what you get when the
//! answer is yes.

use std::path::PathBuf;

use anyhow::Result;

use crate::scan::Session;

/// What the human is asking to start.
///
/// `agent` and `intent` are carried for the policy to use and passed through to
/// `toimija run`; `admit` itself reads only scope and worktree.
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
    pub fn to(worktree: PathBuf, reason: impl Into<String>) -> Self {
        Admission {
            worktree: Some(worktree),
            reason: reason.into(),
        }
    }

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
pub fn scopes_intersect(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter()
        .any(|x| b.iter().any(|y| x.starts_with(y) || y.starts_with(x)))
}

/// Admit only declared, non-overlapping work in an unoccupied worktree.
pub fn admit(request: &Request, live: &[Session]) -> Result<Admission> {
    if request.scope.is_empty() {
        return Ok(Admission::refused("declare at least one scope"));
    }

    for sibling in live {
        if sibling.worktree == request.worktree {
            return Ok(Admission::refused(format!(
                "worktree is already used by session {}",
                sibling.id
            )));
        }
        if scopes_intersect(&request.scope, &sibling.scope) {
            return Ok(Admission::refused(format!(
                "scope overlaps session {}",
                sibling.id
            )));
        }
    }

    // ponytail: admission is repo-local; scan the fleet if cross-repo scopes must block.
    Ok(Admission::to(
        request.worktree.clone(),
        "worktree is free and scope does not overlap a live session",
    ))
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

    fn request(worktree: &str, paths: &[&str]) -> Request {
        Request {
            agent: "claude".into(),
            intent: "why".into(),
            scope: scope(paths),
            worktree: PathBuf::from(worktree),
        }
    }

    fn session(worktree: &str, paths: &[&str]) -> Session {
        Session {
            id: "1-2".into(),
            agent: "codex".into(),
            worktree: PathBuf::from(worktree),
            branch: Some("agent/a".into()),
            intent: "work".into(),
            scope: scope(paths),
            liveness: "active".into(),
            idle_seconds: 0,
        }
    }

    #[test]
    fn unsafe_requests_are_refused() {
        assert!(admit(&request("/tmp/a", &[]), &[])
            .unwrap()
            .worktree
            .is_none());
        assert!(admit(
            &request("/tmp/a", &["src/a.rs"]),
            &[session("/tmp/a", &["src/b.rs"])]
        )
        .unwrap()
        .worktree
        .is_none());
        assert!(admit(
            &request("/tmp/a", &["src/a.rs"]),
            &[session("/tmp/b", &["src/"])]
        )
        .unwrap()
        .worktree
        .is_none());
    }

    #[test]
    fn disjoint_worktrees_and_scopes_are_admitted() {
        let admission = admit(
            &request("/tmp/a", &["src/a.rs"]),
            &[session("/tmp/b", &["src/b.rs"])],
        )
        .unwrap();
        assert_eq!(admission.worktree, Some(PathBuf::from("/tmp/a")));
    }
}
