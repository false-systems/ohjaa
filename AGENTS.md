# Agent guide

<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

This repo is **ohjaa**, an internal False Systems tool: the deciding layer above
toimija. Toimija reports facts and refuses to decide (no dispatch, no
assignment, no network). Ohjaa is where deciding is allowed to live.

Keep changes small and reviewable. The value of this repo is what it refuses to
become.

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; keep both in sync.

- One Rust crate producing a single `ohjaa` binary. Dependencies stay minimal —
  `anyhow`, `clap`, `serde`, `serde_json` — and every addition needs a reason.
- **Ohjaa decides; toimija reports.** Never reimplement anything toimija or
  sauma already derives — shell out to their CLIs and consume their `--json`.
  Ohjaa never speaks the plugin protocol and never compiles a packet.
- **Derive, never store.** No inbox, no index, no cache, no database, no daemon,
  no persistence. The configured root list holds paths, never contents.
- **Cross-repo yes, messaging no.** Permanently refused: an inbox,
  request/response, and presence. `ohjaa cycle` may derive the facts snapshot
  from Toimija, Teko, and sealed artifacts, reduce it to one action, and act
  on that one action through Toimija; it stores no workflow state and is not
  a scheduler or agent-to-agent handoff. A
  message that can go stale without confessing it does not belong here — the
  admission test from `toimija`'s proposal 0010 applies to this repo too.
- Anything crossing a repo boundary travels as a **digest anchored to a
  repository point**, verified by sauma, so it self-invalidates when the tree
  moves. Nothing is trusted because it was sent.
- **No network.** Cross-repo is filesystem reads. Cross-machine is a separate,
  explicit boundary decision — not a side effect of cross-repo.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.

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

`ohjaa fleet` will show you live sibling sessions across every repo — including
siblings whose declared scope reaches into this one.
