//! The system perimeter, derived at the moment it is asked for.
//!
//! Every repo in the fleet self-declares its identity in `.toimija/repo.yaml`,
//! and toimija surfaces that as the `repo` object of `context --json`. Ohjaa
//! derives the fleet's shape from those declarations plus live occupancy:
//! nothing is stored, and the view cannot be staler than the moment it was
//! computed. An agent gets this perimeter at launch (inside toimija's
//! composed launch prompt) and can re-derive it any time with `ohjaa system`
//! and `ohjaa fleet` — pull, never push.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::scan::{scan_fleet, Scan, Session};

/// One tool repo in the fleet, as it declares itself.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub path: PathBuf,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not: Vec<String>,
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
            not: Vec::new(),
        };
        let Ok(output) = crate::process::run(
            Command::new("toimija")
                .args(["context", "--json"])
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
        role: Some(repo_section.role).filter(|role| !role.is_empty()),
        not: repo_section.not_roles,
    })
}

#[derive(Debug, Deserialize)]
struct ContextPayload {
    repo: Option<RepoSection>,
}

#[derive(Debug, Deserialize)]
struct RepoSection {
    name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    not_roles: Vec<String>,
}

/// The fleet's derived shape: what each repo is, and who is live where.
#[derive(Debug, Serialize)]
pub struct SystemView {
    pub tools: Vec<Tool>,
    /// Repos whose role could not be derived. Absence is not evidence — a
    /// repo with an unreadable identity is reported, never silently skipped.
    pub unknown: Vec<PathBuf>,
    pub scans: Vec<Scan>,
}

/// Derive the whole system concurrently: one `toimija context` and one
/// `toimija sessions` per repo, in parallel, order preserved.
pub fn system_view(repos: &[PathBuf]) -> SystemView {
    std::thread::scope(|scope| {
        let tools_handle = scope.spawn(|| {
            let mut tools: Vec<Tool> = repos.iter().map(|repo| Tool::derive(repo)).collect();
            tools.sort_by(|a, b| a.name.cmp(&b.name));
            tools
        });
        let scans = scan_fleet(repos);
        let tools = tools_handle.join().expect("tool derivation panicked");
        let unknown = tools
            .iter()
            .filter(|tool| tool.role.is_none())
            .map(|tool| tool.path.clone())
            .collect();
        SystemView {
            tools,
            unknown,
            scans,
        }
    })
}

impl SystemView {
    fn scan_for(&self, tool: &Tool) -> Option<&Scan> {
        self.scans.iter().find(|scan| scan.repo == tool.path)
    }

    /// Exit code: 0 when every role was derived and every scan saw clearly,
    /// 4 otherwise. Same honesty rule as `fleet`.
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

/// Render `ohjaa system` for humans.
pub fn render_system(view: &SystemView) -> String {
    use std::fmt::Write;

    let mut out = String::from("system perimeter — derived now, stored never\n\n");
    for tool in &view.tools {
        let _ = writeln!(out, "{}", tool.name);
        let _ = writeln!(out, "  path: {}", tool.path.display());
        match &tool.role {
            Some(role) => {
                let _ = writeln!(out, "  role: {role}");
                if !tool.not.is_empty() {
                    let _ = writeln!(out, "  not:  {}", tool.not.join(", "));
                }
            }
            None => {
                let _ = writeln!(out, "  role: unknown — could not be derived");
            }
        }
        if let Some(scan) = view.scan_for(tool) {
            if scan.coverage != crate::scan::Coverage::Available {
                let _ = writeln!(
                    out,
                    "  sessions: incomplete — {}",
                    crate::coverage_detail(scan)
                );
            }
            let occupying: Vec<&Session> = scan.occupying().collect();
            if occupying.is_empty() && scan.coverage == crate::scan::Coverage::Available {
                let _ = writeln!(out, "  sessions: none reported");
            } else if !occupying.is_empty() {
                let _ = writeln!(out, "  possibly running sessions: {}", occupying.len());
                for session in occupying {
                    let intent = if session.intent.is_empty() {
                        "(undeclared)"
                    } else {
                        &session.intent
                    };
                    let _ = writeln!(
                        out,
                        "    {}  {}  \"{}\"  {}  seen {}",
                        session.agent,
                        session.id,
                        intent,
                        session.liveness,
                        crate::scan::ago(session.idle_seconds)
                    );
                }
            }
        } else {
            out.push_str("  sessions: unknown — no scan available\n");
        }
        out.push('\n');
    }
    if !view.unknown.is_empty() {
        out.push_str(
            "roles could not be derived for every repo — absence here is not\n\
             evidence of absence:\n",
        );
        for path in &view.unknown {
            let _ = writeln!(out, "  {}", path.display());
        }
    }
    out
}

/// Compose the launch perimeter: a compact briefing that travels inside
/// `toimija run --task`, so the agent starts already knowing what system it
/// operates in — and how to re-derive every claim in it.
pub fn briefing(view: &SystemView, here: &Path) -> String {
    use std::fmt::Write;

    let mut out = String::from(
        "System perimeter (derived by ohjaa at launch; re-derive with\n\
         `ohjaa system`, session observations via `ohjaa fleet`):\n\n\
         Fleet — each tool self-declares its role in its repo:\n",
    );
    for tool in &view.tools {
        let role = tool
            .role
            .as_deref()
            .unwrap_or("role not derivable — see `ohjaa system`");
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
         observations, not locks. Re-derive them before acting.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Coverage;

    const CONTEXT: &str =
        r#"{"repo":{"name":"teko","role":"durable task contract","not_roles":["inbox"]}}"#;

    fn tool(name: &str, role: Option<&str>) -> Tool {
        Tool {
            path: PathBuf::from(format!("/fleet/{name}")),
            name: name.into(),
            role: role.map(String::from),
            not: vec!["inbox".into()],
        }
    }

    #[test]
    fn a_tool_identity_comes_from_its_own_context_json() {
        let tool = parse_context(Path::new("/fleet/teko"), CONTEXT).unwrap();
        assert_eq!(tool.name, "teko");
        assert_eq!(tool.role.as_deref(), Some("durable task contract"));
        assert_eq!(tool.not, vec!["inbox".to_string()]);

        // Nothing derivable is not a wrong guess: a context without a repo
        // section is not an identity, and unparseable output is nothing at
        // all — both fall back to an unknown role in `Tool::derive`.
        assert!(parse_context(Path::new("/fleet/x"), "{}").is_none());
        assert!(parse_context(Path::new("/fleet/x"), "not json").is_none());
    }

    #[test]
    fn the_briefing_preserves_session_liveness_and_marks_home() {
        let mut view = SystemView {
            tools: vec![
                tool("teko", Some("durable task contract")),
                tool("kelo", None),
            ],
            unknown: vec![PathBuf::from("/fleet/kelo")],
            scans: vec![Scan {
                repo: PathBuf::from("/fleet/teko"),
                outside: false,
                sessions: vec![Session {
                    id: "1-1".into(),
                    agent: "codex".into(),
                    worktree: PathBuf::from("/fleet/teko"),
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

        let text = briefing(&view, Path::new("/fleet/teko"));
        assert!(text.contains("teko — durable task contract"), "{text}");
        assert!(text.contains("you are here"), "{text}");
        assert!(
            text.contains("role not derivable"),
            "an underivable role must not become a guess: {text}"
        );
        assert!(
            text.contains("codex in teko: \"change the parser\""),
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

        let rendered = render_system(&view);
        assert!(rendered.contains("role: unknown"), "{rendered}");
        assert!(
            rendered.contains("possibly running sessions: 1"),
            "{rendered}"
        );
        assert!(rendered.contains("stale  seen 10m ago"), "{rendered}");

        for liveness in ["active", "unknown", "suspended", "closed"] {
            view.scans[0].sessions[0].liveness = liveness.into();
            for text in [
                briefing(&view, Path::new("/fleet/teko")),
                render_system(&view),
            ] {
                assert!(!text.contains("Live now"), "{text}");
                if liveness == "closed" {
                    assert!(!text.contains("change the parser"), "{text}");
                } else {
                    assert!(text.contains(liveness), "{text}");
                }
            }
        }
    }

    #[test]
    fn a_fully_derived_quiet_fleet_is_complete() {
        let mut view = SystemView {
            tools: vec![tool("teko", Some("durable task contract"))],
            unknown: vec![],
            scans: vec![Scan {
                repo: PathBuf::from("/fleet/teko"),
                outside: false,
                sessions: vec![],
                warnings: vec![],
                coverage: Coverage::Available,
                error: None,
            }],
        };
        assert_eq!(view.exit_code(), 0);

        let text = briefing(&view, Path::new("/fleet/teko"));
        assert!(
            text.contains("No possibly running sessions reported"),
            "{text}"
        );

        for coverage in [Coverage::Degraded, Coverage::Failed] {
            view.scans[0].coverage = coverage;
            view.scans[0].error = Some("cannot inspect sessions".into());
            assert_eq!(view.exit_code(), 4);
            for text in [
                briefing(&view, Path::new("/fleet/teko")),
                render_system(&view),
            ] {
                assert!(
                    text.contains("incomplete — cannot inspect sessions"),
                    "{text}"
                );
                assert!(!text.contains("sessions: none reported"), "{text}");
                assert!(!text.contains("nobody"), "{text}");
            }
        }
    }
}
