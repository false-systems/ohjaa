# Agent guide

Read `.toimija/current.md` before anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet, which supersedes it.

This is **ohjaa**. Toimija reports facts and refuses to decide; ohjaa holds what people decided and know (knowledge-node.v1) and writes it into each agent's own instruction file. It runs no sessions and no checks: the gate is sykli's, from each repository's own `sykli.json`. Keep changes small: the value of this repo is what it refuses to become. `docs/reference.md` says how each command behaves, and its "Boundaries in full" says what ohjaa keeps where; the README is the short introduction for a person.

## Boundaries

These mirror `.toimija/repo.yaml`; keep both in sync.

- One `ohjaa` binary, and beside it the crate of the knowledge-node format, `crates/knowledge-node`. Dependencies: anyhow, clap, serde, serde_json and libc; the format's crate, toml; tests also serde_yaml. Every addition needs a reason.
- Ohjaa holds Knowledge; toimija reports facts. Never reimplement what toimija derives: shell out to its CLI and read its --json.
- Ohjaa stores Knowledge, in git. Everything else is derived when asked; the human view's snapshot is the one cache. No knowledge index and no daemon.
- The workflow lives in the home (~/.config/ohjaa), outside every repository. No file, hook or config of a repository's checkout is written.
- Ohjaa checks nothing and refuses nothing for the workflow: the checks a commit must pass, and refusing a push without them, are sykli's, through the repository's own hooks.
- Agents propose; only a person makes Knowledge current, retires it or rejects it. What ohjaa writes into a repository names no tool, and a public one is sent nothing that names a private one.
- Cross-repo yes, messaging no: no request/response between agents, no presence, no dispatch.
- Network only through gh, the drafter, and git to GitHub to deliver repo knowledge. Every other command works from the filesystem and local CLIs.
- Failed coverage is never rendered as an empty result. If a scan could not look, it says so and exits non-zero.

## Working here

- Output for a person is plain and contextual: name the repository or session, say what happened or is missing, and show how to see more or act.
- Never run `ohjaa accept`, `reject`, `retire`, `confirm` or `review` for a person, or fake a terminal to get past them, except in a test's own sandbox. Propose with `ohjaa propose`.
- Before you finish, leave `toimija handover` for the next agent: what is unfinished, what it needs to know, what surprised you.
- `knowledge-node.v1` is frozen: never edit `fixtures/knowledge-node.v1/`. A change to what a valid node is makes a new format version, with fixtures of its own.
- Tests never touch the person's real state (the "Tests" section of `docs/reference.md` says what each suite sandboxes), and write a file they will run only through `write_executable`.
- The gate is this repository's own `sykli.json`, pinned by `sykli.lock`: `sykli run` proves HEAD. After adding a source or test file, add it to every task's inputs and run `sykli lock sykli.json`. A new git process on a hot path needs its budget in `tests/config_home.rs`.
- Code is Rust or Elixir/Erlang; never add Python, Go or Node.
- To update a branch under review, merge `origin/main` into it; never rebase or force-push it. `toimija sessions` shows who else is live.

## Validation

`cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`; then `sykli run`, the gate.

## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, written here through a pull request when a person accepts one. A line changed here by hand is replaced the next time it is written: change a lesson where it is decided. -->

