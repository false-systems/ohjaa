# Agent guide

Read `.toimija/current.md` before anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet, which supersedes it.

This is **ohjaa**, an internal False Systems tool. Toimija reports facts and refuses to decide; ohjaa holds what people decided and know (knowledge-node.v1) and writes it into each agent's own instruction file. It runs no sessions and no checks: the gate is sykli's, from each repository's own `sykli.json`. Keep changes small: the value of this repo is what it refuses to become. The README's "Boundaries in full" says what ohjaa keeps where; its other sections say how each command behaves.

## Boundaries

These mirror `.toimija/repo.yaml`; keep both in sync.

- One Rust crate, one `ohjaa` binary. Dependencies: anyhow, clap, serde, serde_json, the vendored faija-knowledge, redb (the session log) and libc; tests also sha2 and serde_yaml. Every addition needs a reason.
- Ohjaa holds Knowledge; toimija reports facts. Never reimplement what toimija derives: shell out to its CLI and read its --json.
- Ohjaa stores Knowledge, in git, and a machine-local session log. Everything else is derived when asked; the human view's snapshot is the one cache. No knowledge index and no daemon.
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
- Never edit `vendor/faija-knowledge/`; `scripts/vendor-faija-knowledge.sh` refreshes it.
- Tests never touch the person's real state (the README's "Tests" says what each suite sandboxes), and write a file they will run only through `write_executable`.
- The gate is this repository's own `sykli.json`, pinned by `sykli.lock`: `sykli run` proves HEAD. After adding a source or test file, add it to every task's inputs and run `sykli lock sykli.json`. A new git process on a hot path needs its budget in `tests/config_home.rs`.
- Code is Rust or Elixir/Erlang; never add Python, Go or Node.
- To update a branch under review, merge `origin/main` into it; never rebase or force-push it. `toimija sessions` shows who else is live.

## Validation

`cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`; then `sykli run`, the gate.

## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, written here through a pull request when a person accepts one. A line changed here by hand is replaced the next time it is written: change a lesson where it is decided. -->

- <!-- lesson: ohjaa-env-claimed-ids-are-testimony --> **decision**: Treat a session id claimed by the agent's environment as testimony, not observation, and label it that way. Rank the evidence: launch prompt, then receipt, then env claim, then window. When the launch prompt and the env claim disagree, keep the observation and name the disagreement. (as of 2026-10-27)
- <!-- lesson: ohjaa-marks-v1-only-gains-keys --> **decision**: The `ohjaa-marks.v1` schema only gains keys, and the keys viemari was built on never go. Dropping the unread `judgements` key was the one exception, so the schema keeps its name. (as of 2026-10-27)
- <!-- lesson: ohjaa-derived-gates-stay-out-of-config --> **decision**: Derived gate artifacts are never written into Ohjaa's config home; normal configuration and knowledge writes remain allowed. A gate that a later run needs is kept under $XDG_STATE_HOME/ohjaa/derived/<digest>/, and a gate that is only read goes into a private $TMPDIR folder that is removed when done. `ohjaa gate --print` writes nothing. (as of 2026-10-29)
