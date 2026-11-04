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
    let gitignore =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".gitignore"))
            .expect("read .gitignore");
    // This `.gitignore` alone, in a repository of the test's own, decides:
    // none of the person's rules does. Git reads a global excludes file
    // ($XDG_CONFIG_HOME/git/ignore, else ~/.config/git/ignore) even with no
    // global config, and a clone's own info/exclude, so neither is used.
    let base = std::env::temp_dir().join(format!("ohjaa-gitignore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&repo)
            .env("HOME", &base)
            .env("XDG_CONFIG_HOME", &base)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "core.excludesFile=/dev/null"])
            .args(args)
            .status()
            .expect("git runs")
    };
    assert!(git(&["init", "--quiet"]).success(), "git init");
    std::fs::write(repo.join(".gitignore"), gitignore).unwrap();
    // Exit 0: ignored; 1: not ignored.
    let ignored = |path: &str| git(&["check-ignore", "--quiet", "--no-index", path]).code();
    let answers: Vec<(&str, Option<i32>)> = [
        "README.md",
        ".toimija/repo.yaml",
        ".toimija/current.md",
        ".toimija/current.json",
        ".toimija/sessions/1793700398-28263/current.md",
        ".toimija/history.jsonl",
        ".toimija/history.lock",
        ".toimija/runs/r1/log",
        ".toimija/snapshots/s1",
        ".toimija/written-by-a-later-toimija",
    ]
    .into_iter()
    .map(|path| (path, ignored(path)))
    .collect();
    std::fs::remove_dir_all(&base).unwrap();
    for (path, answer) in answers {
        let tracked = path == "README.md" || path == ".toimija/repo.yaml";
        assert_eq!(
            answer,
            Some(if tracked { 1 } else { 0 }),
            "{path}: git {} it",
            if tracked {
                "ignores, though it is tracked,"
            } else {
                "does not ignore"
            }
        );
    }
}
