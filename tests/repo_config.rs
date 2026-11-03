//! `.toimija/repo.yaml` is read by toimija on every packet refresh here. If it
//! does not parse, every refresh fails and the live packet goes stale — which
//! c6673a1 did, with one boundary starting with a backtick. Nothing else in CI
//! reads the file, so this test parses it with the parser toimija uses.

use std::path::Path;

fn repo_yaml() -> serde_yaml::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".toimija/repo.yaml");
    let text = std::fs::read_to_string(&path).expect("read .toimija/repo.yaml");
    serde_yaml::from_str(&text)
        .unwrap_or_else(|error| panic!("toimija could not parse {}: {error}", path.display()))
}

#[test]
fn repo_yaml_parses_the_way_toimija_reads_it() {
    let config = repo_yaml();
    assert_eq!(config["repo"]["name"].as_str(), Some("ohjaa"));
    assert!(config["repo"]["role"].is_string(), "repo.role is text");
    let boundaries = config["boundaries"]
        .as_sequence()
        .expect("boundaries is a list");
    assert!(!boundaries.is_empty());
    for boundary in boundaries {
        // A plain scalar with `: ` inside becomes a mapping, and ` #` cuts it
        // short as a comment: either way it is no longer the boundary written.
        assert!(
            boundary.is_string(),
            "a boundary that is not plain text (quote it): {boundary:?}"
        );
    }
}

/// toimija writes its working files under `.toimija/`: the packet, sessions,
/// history, runs, snapshots, and whatever it adds next. Git must ignore all
/// of them but `repo.yaml`, which is tracked. Naming each file let
/// `snapshots/` through, so `toimija doctor --strict` failed here.
#[test]
fn git_ignores_toimijas_working_files_but_not_its_config() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Exit 0: ignored; 1: not ignored. The person's own global ignore rules
    // are kept out, so only this repository's decide.
    let ignored = |path: &str| {
        std::process::Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["check-ignore", "--quiet", "--no-index", path])
            .status()
            .ok()
            .and_then(|status| status.code())
    };
    if ignored("README.md") != Some(1) {
        eprintln!("skipped: not a git checkout, or git did not run");
        return;
    }
    for path in [
        ".toimija/current.md",
        ".toimija/current.json",
        ".toimija/sessions/1793700398-28263/current.md",
        ".toimija/history.jsonl",
        ".toimija/history.lock",
        ".toimija/runs/r1/log",
        ".toimija/snapshots/s1",
        ".toimija/written-by-a-later-toimija",
    ] {
        assert_eq!(ignored(path), Some(0), "git does not ignore {path}");
    }
    assert_eq!(
        ignored(".toimija/repo.yaml"),
        Some(1),
        "git ignores .toimija/repo.yaml, which is tracked"
    );
}
