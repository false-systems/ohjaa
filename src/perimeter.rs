//! The briefing's perimeter, derived at the moment it is asked for.
//!
//! Every repo self-declares its identity in `.toimija/repo.yaml`, and
//! toimija surfaces that as the `repo` object of `context --identity
//! --json` (one git process). `ohjaa brief` derives the launch repository's
//! and each scoped repository's role from those declarations, plus who is
//! live there from `toimija sessions`: nothing is stored, and the view
//! cannot be staler than the moment it was computed. An agent gets this
//! perimeter at launch (inside toimija's composed launch prompt) and can
//! ask `toimija sessions` again any time: pull, never push.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::scan::{Scan, Session};

/// One repository, as it declares itself.
#[derive(Debug, Clone)]
pub struct Tool {
    pub path: PathBuf,
    pub name: String,
    pub role: Option<String>,
}

impl Tool {
    /// Ask the repo's own toimija what this repo is. Toimija owns the
    /// judgment (it reads repo.yaml); a second opinion here would be a second
    /// source of truth. Any failure degrades to an unknown role, never to a
    /// guess.
    fn derive(repo: &Path) -> Tool {
        let unknown = || Tool {
            path: repo.to_path_buf(),
            name: repo
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| repo.display().to_string()),
            role: None,
        };
        let Ok(output) = crate::process::run(
            Command::new("toimija")
                .args(["context", "--identity", "--json"])
                .current_dir(repo),
            None,
            crate::process::LOCAL,
        ) else {
            return unknown();
        };
        if !output.status.success() {
            return unknown();
        }
        match parse_context(repo, &String::from_utf8_lossy(&output.stdout)) {
            Some(tool) => tool,
            None => unknown(),
        }
    }
}

fn parse_context(repo: &Path, stdout: &str) -> Option<Tool> {
    let payload: ContextPayload = serde_json::from_str(stdout).ok()?;
    let repo_section = payload.repo?;
    Some(Tool {
        path: repo.to_path_buf(),
        name: repo_section.name,
        role: repo_section.role.filter(|role| !role.is_empty()),
    })
}

#[derive(Debug, Deserialize)]
struct ContextPayload {
    repo: Option<RepoSection>,
}

#[derive(Debug, Deserialize)]
struct RepoSection {
    name: String,
    /// toimija writes `null` for a repo.yaml that declares no role.
    #[serde(default)]
    role: Option<String>,
}

/// The perimeter: what each repository is, and who is live where.
#[derive(Debug)]
pub struct Perimeter {
    pub tools: Vec<Tool>,
    /// Repos whose role could not be derived. Absence is not evidence — a
    /// repo with an unreadable identity is reported, never silently skipped.
    pub unknown: Vec<PathBuf>,
    pub scans: Vec<Scan>,
}

/// The perimeter of `repos`, with the sessions the launch already asked
/// about. Repositories not selected are unobserved, not empty.
pub fn scoped_view(repos: &[PathBuf], scans: Vec<Scan>) -> Perimeter {
    let mut tools: Vec<_> = repos.iter().map(|repo| Tool::derive(repo)).collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let unknown = tools
        .iter()
        .filter(|t| t.role.is_none())
        .map(|t| t.path.clone())
        .collect();
    Perimeter {
        tools,
        unknown,
        scans,
    }
}

impl Perimeter {
    /// Exit code: 0 when every role was derived and every scan saw clearly,
    /// 4 otherwise.
    pub fn exit_code(&self) -> i32 {
        let scans_incomplete = self
            .scans
            .iter()
            .any(|scan| scan.coverage != crate::scan::Coverage::Available);
        if !self.unknown.is_empty() || scans_incomplete {
            4
        } else {
            0
        }
    }
}

/// Compose the launch perimeter: a compact part of the briefing that travels
/// in `toimija run --briefing`, so the agent starts already knowing what system it
/// operates in — and how to re-derive every claim in it.
pub fn briefing(view: &Perimeter, here: &Path) -> String {
    use std::fmt::Write;

    let mut out = String::from(
        "System perimeter (derived by ohjaa at launch; who is live: `toimija sessions`):\n\n\
         Repositories — each tool self-declares its role in its repo:\n",
    );
    for tool in &view.tools {
        let role = tool.role.as_deref().unwrap_or("role not derivable");
        let marker = if tool.path == here {
            "  <- you are here"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "- {} — {} ({}){}",
            tool.name,
            role,
            tool.path.display(),
            marker
        );
    }
    if !view.tools.iter().any(|tool| tool.path == here) {
        let _ = writeln!(
            out,
            "You are in {}, which is not one of these repositories.",
            here.display()
        );
    }
    let live: Vec<&Session> = view
        .scans
        .iter()
        .flat_map(|scan| scan.occupying())
        .collect();
    out.push_str(
        "\nSession observations at launch (Toimija liveness; not proof of current work):\n",
    );
    for scan in &view.scans {
        if scan.coverage != crate::scan::Coverage::Available {
            let what = if scan.outside {
                scan.name()
            } else {
                scan.repo.display().to_string()
            };
            let _ = writeln!(
                out,
                "- {what}: incomplete — {}",
                crate::coverage_detail(scan)
            );
        }
    }
    if live.is_empty() {
        out.push_str("No possibly running sessions reported by available scans.\n");
    } else {
        let _ = writeln!(out, "{} possibly running session(s):", live.len());
        for scan in &view.scans {
            for session in scan.occupying() {
                let intent = if session.intent.is_empty() {
                    "(undeclared)"
                } else {
                    &session.intent
                };
                let place = if scan.outside {
                    format!(
                        "{} (in {})",
                        crate::scan::OUTSIDE,
                        session.worktree.display()
                    )
                } else {
                    format!("in {}", scan.name())
                };
                let _ = writeln!(
                    out,
                    "- {} {}: \"{}\" ({}; seen {})",
                    session.agent,
                    place,
                    intent,
                    session.liveness,
                    crate::scan::ago(session.idle_seconds)
                );
            }
        }
    }
    out.push_str(
        "\nCoordination: claiming work stays in GitHub issues/PRs; sessions are\n\
         observations, not locks. Refresh when scope or session state may have changed.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Coverage;

    const CONTEXT: &str =
        r#"{"repo":{"name":"tasker","role":"durable task contract","not_roles":["inbox"]}}"#;

    fn tool(name: &str, role: Option<&str>) -> Tool {
        Tool {
            path: PathBuf::from(format!("/fleet/{name}")),
            name: name.into(),
            role: role.map(String::from),
        }
    }

    #[test]
    fn a_tool_identity_comes_from_its_own_context_json() {
        let tool = parse_context(Path::new("/fleet/tasker"), CONTEXT).unwrap();
        assert_eq!(tool.name, "tasker");
        assert_eq!(tool.role.as_deref(), Some("durable task contract"));

        // Nothing derivable is not a wrong guess: a context without a repo
        // section is not an identity, and unparseable output is nothing at
        // all — both fall back to an unknown role in `Tool::derive`.
        assert!(parse_context(Path::new("/fleet/x"), "{}").is_none());
        assert!(parse_context(Path::new("/fleet/x"), "not json").is_none());
    }

    /// toimija writes `"role": null` for a repo.yaml that declares no role
    /// (its `RepoInfo.role` is an `Option`): the repo keeps its name, and only
    /// its role is unknown. This is toimija's own output,
    /// byte for byte, for `name: tasker` and `not: [inbox]`.
    #[test]
    fn a_repo_that_declares_no_role_keeps_its_name() {
        const NO_ROLE: &str = r#"{
  "schema": "toimija-identity.v1",
  "repo": {
    "name": "tasker",
    "role": null,
    "not_roles": [
      "inbox"
    ],
    "important_docs": [
      "README.md"
    ],
    "boundaries": []
  }
}
"#;
        let tool = parse_context(Path::new("/fleet/tasker-checkout"), NO_ROLE)
            .expect("an identity with no role is still an identity");
        assert_eq!(tool.name, "tasker");
        assert_eq!(tool.role, None);
    }

    #[test]
    fn the_briefing_preserves_session_liveness_and_marks_home() {
        let mut view = Perimeter {
            tools: vec![
                tool("tasker", Some("durable task contract")),
                tool("kelo", None),
            ],
            unknown: vec![PathBuf::from("/fleet/kelo")],
            scans: vec![Scan {
                repo: PathBuf::from("/fleet/tasker"),
                outside: false,
                sessions: vec![Session {
                    id: "1-1".into(),
                    agent: "codex".into(),
                    worktree: PathBuf::from("/fleet/tasker"),
                    branch: Some("main".into()),
                    intent: "change the parser".into(),
                    scope: vec![],
                    liveness: "stale".into(),
                    pid_check: "unknown".into(),
                    idle_seconds: 600,
                }],
                warnings: vec![],
                coverage: Coverage::Available,
                error: None,
            }],
        };

        let text = briefing(&view, Path::new("/fleet/tasker"));
        assert!(text.contains("tasker — durable task contract"), "{text}");
        assert!(text.contains("you are here"), "{text}");
        assert!(
            text.contains("role not derivable"),
            "an underivable role must not become a guess: {text}"
        );
        assert!(
            text.contains("codex in tasker: \"change the parser\""),
            "{text}"
        );
        assert!(text.contains("(stale; seen 10m ago)"), "{text}");
        assert!(!text.contains("Live now"), "{text}");
        assert!(
            text.contains("sessions are\nobservations, not locks"),
            "{text}"
        );

        assert_eq!(
            view.exit_code(),
            4,
            "an underivable role makes it incomplete"
        );

        for liveness in ["active", "unknown", "suspended", "closed"] {
            view.scans[0].sessions[0].liveness = liveness.into();
            let text = briefing(&view, Path::new("/fleet/tasker"));
            assert!(!text.contains("Live now"), "{text}");
            if liveness == "closed" {
                assert!(!text.contains("change the parser"), "{text}");
            } else {
                assert!(text.contains(liveness), "{text}");
            }
        }
    }

    #[test]
    fn a_fully_derived_quiet_fleet_is_complete() {
        let mut view = Perimeter {
            tools: vec![tool("tasker", Some("durable task contract"))],
            unknown: vec![],
            scans: vec![Scan {
                repo: PathBuf::from("/fleet/tasker"),
                outside: false,
                sessions: vec![],
                warnings: vec![],
                coverage: Coverage::Available,
                error: None,
            }],
        };
        assert_eq!(view.exit_code(), 0);

        let text = briefing(&view, Path::new("/fleet/tasker"));
        assert!(
            text.contains("No possibly running sessions reported"),
            "{text}"
        );

        for coverage in [Coverage::Degraded, Coverage::Failed] {
            view.scans[0].coverage = coverage;
            view.scans[0].error = Some("cannot inspect sessions".into());
            assert_eq!(view.exit_code(), 4);
            let text = briefing(&view, Path::new("/fleet/tasker"));
            assert!(
                text.contains("incomplete — cannot inspect sessions"),
                "{text}"
            );
            assert!(!text.contains("nobody"), "{text}");
        }
    }
}
