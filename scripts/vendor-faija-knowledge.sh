#!/usr/bin/env bash
# Refresh the knowledge-node crate ohjaa carries.
#
#   scripts/vendor-faija-knowledge.sh <faija-checkout> [revision]
#
# Copies `crates/faija-knowledge/` out of faija at the named revision, byte for
# byte, and rewrites `vendor/faija-knowledge.contract`. It never edits the
# crate: if the copy needs changing, the change belongs in faija.
#
# Carried rather than fetched, like sauma-canonical in toimija: faija is a
# private repository and publishes nothing. There is still exactly one
# implementation of knowledge-node.v1, distributed rather than linked, and
# `tests/vendored_knowledge.rs` recomputes the digest on every run, so an edit
# here fails the build and shows up as a diff touching `vendor/`.
set -euo pipefail
faija="${1:?usage: vendor-faija-knowledge.sh <faija-checkout> [revision]}"
revision="${2:-origin/main}"
root="$(cd "$(dirname "$0")/.." && pwd)"

full=$(git -C "$faija" rev-parse "$revision^{commit}")
tag=$(git -C "$faija" describe --tags --exact-match "$full" 2>/dev/null || echo untagged)

# Extract into scratch BEFORE touching the repository: a revision without the
# crate fails here and changes nothing.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
git -C "$faija" archive "$full" crates/faija-knowledge/Cargo.toml crates/faija-knowledge/src \
  | tar -x -C "$scratch"
[ -s "$scratch/crates/faija-knowledge/Cargo.toml" ] && [ -d "$scratch/crates/faija-knowledge/src" ] \
  || { echo "vendor-faija-knowledge: $full lacks the crate; nothing was changed" >&2; exit 1; }

rm -rf "$root/vendor/faija-knowledge"
mkdir -p "$root/vendor"
cp -R "$scratch/crates/faija-knowledge" "$root/vendor/faija-knowledge"

# The digest `tests/vendored_knowledge.rs` reproduces: one line per file,
# `<relative path>  <sha256>`, sorted by path, digested whole.
listing=$(cd "$root/vendor/faija-knowledge" && find . -type f | sed 's|^\./||' | LC_ALL=C sort \
  | while IFS= read -r f; do printf '%s  %s\n' "$f" "$(shasum -a 256 "$f" | cut -d' ' -f1)"; done)
digest=$(printf '%s' "$listing" | shasum -a 256 | cut -d' ' -f1)
files=$(printf '%s\n' "$listing" | grep -c .)

cat > "$root/vendor/faija-knowledge.contract" <<EOF
# The knowledge-node.v1 crate ohjaa carries.
#
# faija owns the format: what a valid node is, the rules across a set of nodes,
# the canonical form and the briefing order. ohjaa holds that implementation
# here rather than fetching it from a private repository on every build.
#
# \`vendor/faija-knowledge/\` is byte-identical to \`crates/faija-knowledge/\` at
# \`faija-revision\`, and \`tests/vendored_knowledge.rs\` recomputes
# \`tree-digest\` on every run, so an edit here fails the build. Not a security
# boundary: it makes drift visible as a diff touching \`vendor/\`.
#
# Refresh with \`scripts/vendor-faija-knowledge.sh <faija-checkout> <revision>\`,
# never by editing the crate in place.
faija-tag = $tag
faija-revision = $full
tree-digest = sha256:$digest
files = $files
EOF
echo "vendored faija-knowledge at $full ($files files, sha256:$digest)"
