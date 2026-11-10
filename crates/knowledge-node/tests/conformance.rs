//! The golden fixtures in `fixtures/knowledge-node.v1` are the contract.
//! Another implementation conforms when it reproduces every expected result
//! here; this crate is held to the same bar.

use std::fs;
use std::path::{Path, PathBuf};

use ohjaa_knowledge_node::{briefing, parse_node, validate_set, ErrorCode, Located, Scope};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/knowledge-node.v1")
}

fn expected(dir: &Path) -> toml::Table {
    fs::read_to_string(dir.join("expected.toml"))
        .expect("expected.toml")
        .parse()
        .expect("expected.toml parses")
}

fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files
}

fn stem(path: &Path) -> &str {
    path.file_stem().unwrap().to_str().unwrap()
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_str().unwrap().to_owned()
}

#[test]
fn every_node_fixture_gives_its_expected_result() {
    let dir = fixtures().join("nodes");
    let expected = expected(&dir);
    let valid: Vec<&str> = expected["valid"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file.as_str().unwrap())
        .collect();
    let invalid = expected["invalid"].as_table().unwrap();

    let files = markdown_files(&dir);
    assert_eq!(
        files.len(),
        valid.len() + invalid.len(),
        "every fixture file is listed exactly once in expected.toml"
    );
    for path in &files {
        let file = name(path);
        let text = fs::read_to_string(path).unwrap();
        let result = parse_node(stem(path), &text);
        if valid.contains(&file.as_str()) {
            assert!(result.is_ok(), "{file} should be valid: {result:?}");
        } else {
            let code = invalid
                .get(&file)
                .and_then(|code| code.as_str())
                .unwrap_or_else(|| panic!("{file} is not listed in expected.toml"));
            let code = ErrorCode::from_name(code).unwrap_or_else(|| panic!("unknown code {code}"));
            assert_eq!(result.map_err(|error| error.code), Err(code), "{file}");
        }
    }
}

#[test]
fn every_set_fixture_gives_its_expected_errors_and_briefing() {
    let root = fixtures().join("sets");
    let mut cases: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty());

    for case in cases {
        let label = name(&case);
        let mut nodes = Vec::new();
        for scope in Scope::ALL {
            let dir = case.join(scope.name());
            if !dir.is_dir() {
                continue;
            }
            for path in markdown_files(&dir) {
                let text = fs::read_to_string(&path).unwrap();
                let node = parse_node(stem(&path), &text)
                    .unwrap_or_else(|error| panic!("{label}/{}: {error}", name(&path)));
                nodes.push(Located { scope, node });
            }
        }
        let expected = expected(&case);

        let mut want: Vec<(String, String)> = expected["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| {
                (
                    error["code"].as_str().unwrap().to_owned(),
                    error["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let mut got: Vec<(String, String)> = validate_set(&nodes)
            .into_iter()
            .map(|error| (error.code.as_str().to_owned(), error.id))
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{label}: set errors");

        let want: Vec<&str> = expected["briefing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        let got: Vec<&str> = briefing(&nodes)
            .iter()
            .map(|l| l.node.id.as_str())
            .collect();
        assert_eq!(got, want, "{label}: briefing order");
    }
}

/// FNV-1a, 64-bit: enough to notice an edit, and not a hash anything else relies on.
fn fnv1a64(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, base, out);
        } else {
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/");
            out.push((relative, fs::read(&path).unwrap()));
        }
    }
}

/// `knowledge-node.v1` is frozen. Editing a fixture changes what conforming
/// means, which is a new version, not an edit: add `knowledge-node.v2` instead.
#[test]
fn v1_fixtures_are_frozen() {
    let base = fixtures();
    let mut files = Vec::new();
    walk(&base, &base, &mut files);
    files.sort();
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for (path, bytes) in &files {
        hash = fnv1a64(path.as_bytes(), hash);
        hash = fnv1a64(&[0], hash);
        hash = fnv1a64(bytes, hash);
        hash = fnv1a64(&[0], hash);
    }
    assert_eq!(format!("fnv1a64:{hash:016x}"), "fnv1a64:1540fe3244768492");
}
