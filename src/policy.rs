//! The admission policy — the one part of this repo that is a human decision.
//!
//! Admission answers two things at once: may this agent start, and where. The
//! refusal is the first-class outcome; a worktree is what you get when the
//! answer is yes.

use std::path::{Path, PathBuf};

use crate::scan::Session;

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

/// Admit an agent only when its worktree is unoccupied.
pub fn admit(worktree: &Path, live: &[Session]) -> Admission {
    for sibling in live {
        if sibling.worktree == worktree {
            return Admission::refused(format!(
                "worktree is already used by session {}",
                sibling.id
            ));
        }
    }

    Admission::to(worktree.to_path_buf(), "worktree is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(worktree: &str) -> Session {
        Session {
            id: "1-2".into(),
            agent: "codex".into(),
            worktree: PathBuf::from(worktree),
            branch: Some("agent/a".into()),
            intent: "work".into(),
            scope: Vec::new(),
            liveness: "active".into(),
            idle_seconds: 0,
        }
    }

    #[test]
    fn occupied_worktree_is_refused() {
        assert!(admit(Path::new("/tmp/a"), &[session("/tmp/a")])
            .worktree
            .is_none());
    }

    #[test]
    fn separate_worktree_is_admitted() {
        let admission = admit(Path::new("/tmp/a"), &[session("/tmp/b")]);
        assert_eq!(admission.worktree, Some(PathBuf::from("/tmp/a")));
    }
}
