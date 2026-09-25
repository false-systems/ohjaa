# Agent guide

Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
This repo is **ohjaa**, an internal False Systems tool: steering knowledge for
agents. Toimija reports facts and refuses to decide. Ohjaa holds what the
people agents work for know and have decided (faija-ontology.v2 *Knowledge*,
in the knowledge-node.v1 format) and briefs every agent with it before it acts.

Keep changes small and reviewable. The value of this repo is what it refuses to
become.

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; keep both in sync.

- One Rust crate producing a single `ohjaa` binary. Dependencies stay minimal —
  `anyhow`, `clap`, `serde`, `serde_json`, and the vendored `faija-knowledge`
  (knowledge-node.v1) — and every addition needs a reason.
- **Ohjaa holds Knowledge; toimija reports facts.** Never reimplement anything
  toimija derives — shell out to its CLI and consume its `--json`.
- **Knowledge is the one thing ohjaa stores**, as knowledge-node.v1 files in
  the scope directories, plus proposals waiting for a person. Everything else —
  fleet, perimeter, coverage — is derived now and stored never. No index, no
  cache, no database, no daemon.
- **Agents propose; only a person makes Knowledge current**, retires it or
  rejects it. Shared Knowledge (ours, repo) is reviewed in a pull request
  before any agent reads it. A proposal never lands in a shared repository by
  itself.
- **Cross-repo yes, messaging no.** Permanently refused: request/response
  between agents, presence, and agent-to-agent handoff. Knowledge is not a
  message — it carries its time, its why and whose it is.
- **Network only in `ohjaa intake`**, and only through the tools it names —
  `gh` for review comments and the configured drafter. Every other command
  works from the filesystem and local CLIs.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.

## If you are an agent working here

- Never run `ohjaa accept`, `reject`, `retire`, `confirm` or `review` on a
  person's behalf, and never fake a terminal to get past that refusal. Propose
  with `ohjaa propose` and say so.
- Never edit `vendor/faija-knowledge/`; refresh it with
  `scripts/vendor-faija-knowledge.sh`. The format belongs to faija.
- Tests must never read or write the person's real knowledge: the CLI tests
  sandbox `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `OHJAA_OURS`.

## Validation

Before claiming completion, run:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## Working with other agents

Several agents may work this repo at once. Coordinate through Git and GitHub
primitives, not assumptions: claim an issue before starting, re-orient with
`toimija context` and `git fetch` before acting, keep branches small and
short-lived, and rebase on `origin/main` right before opening or merging.

`ohjaa fleet` shows live sibling sessions across every repo.
