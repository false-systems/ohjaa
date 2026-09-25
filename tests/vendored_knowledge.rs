//! The knowledge-node crate ohjaa carries is the one faija wrote.
//!
//! Not a security boundary: the same commit could change the vendored crate and
//! the digest recorded beside it. What it catches is drift. An edit here fails
//! the build, and what a reviewer sees is a diff touching `vendor/`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The construction `scripts/vendor-faija-knowledge.sh` records: one line per
/// file, `<relative path>  <sha256>`, sorted by path, digested whole.
fn tree_digest(dir: &Path) -> (String, usize) {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<(String, PathBuf)>) {
        for entry in std::fs::read_dir(dir).expect("read vendored dir").flatten() {
            let path = entry.path();
            let relative = prefix.join(entry.file_name());
            if path.is_dir() {
                walk(&path, &relative, out);
            } else {
                out.push((relative.to_string_lossy().replace('\\', "/"), path));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut listing = String::new();
    for (relative, path) in &files {
        let bytes = std::fs::read(path).expect("read vendored file");
        listing.push_str(&format!("{relative}  {:x}\n", Sha256::digest(&bytes)));
    }
    let trimmed = listing.strip_suffix('\n').unwrap_or(&listing);
    (
        format!("sha256:{:x}", Sha256::digest(trimmed.as_bytes())),
        files.len(),
    )
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('='))
        .map(str::trim)
        .unwrap_or_else(|| panic!("`{key}` missing from the contract"))
}

#[test]
fn the_vendored_crate_is_the_one_the_contract_names() {
    let root = repo_root();
    let contract =
        std::fs::read_to_string(root.join("vendor/faija-knowledge.contract")).expect("contract");
    let (digest, files) = tree_digest(&root.join("vendor/faija-knowledge"));
    assert_eq!(
        digest,
        field(&contract, "tree-digest"),
        "vendor/faija-knowledge drifted from the contract; refresh it with \
         scripts/vendor-faija-knowledge.sh instead of editing it"
    );
    assert_eq!(files.to_string(), field(&contract, "files"));
}
