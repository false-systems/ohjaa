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
