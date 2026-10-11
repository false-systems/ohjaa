# Agent guide

<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

This repo is **ohjaa**, an internal False Systems tool: steering knowledge for
agents. Toimija reports facts and refuses to decide. Ohjaa holds what the
people agents work for know and have decided (faija-ontology.v2 *Knowledge*,
in the knowledge-node.v1 format) and briefs every agent with it before it acts.
It also runs each agent session from entering to leaving, and keeps a
machine-local log of every session it runs.

Keep changes small and reviewable. The value of this repo is what it refuses to
become.

## Boundaries

These mirror `.toimija/repo.yaml` verbatim; keep both in sync.

- One Rust crate producing a single `ohjaa` binary. Dependencies stay minimal —
  `anyhow`, `clap`, `serde`, `serde_json`, the vendored `faija-knowledge`
  (knowledge-node.v1), `redb` (the session log) and `libc` (signal
  dispositions while a session runs) — and every addition needs a reason. Tests
  alone also use `sha2` and `serde_yaml` (the parser toimija reads
  `.toimija/repo.yaml` with, so a config toimija cannot parse fails CI here).
- **Ohjaa holds Knowledge; toimija reports facts.** Never reimplement anything
  toimija derives — shell out to its CLI and consume its `--json`.
- **Ohjaa stores two things.** Knowledge, as knowledge-node.v1 files in the
  scope directories plus proposals waiting for a person, is the truth and lives
  in git. The **session log**, in redb at `$XDG_STATE_HOME/ohjaa/sessions.redb`,
  is machine-local history of what each session was briefed, read back,
  committed, finished and handed over, plus explicitly attributed usage receipts — deleting it loses no knowledge, it never
  blocks a launch, and a broken log is reported, never replaced. Everything
  else — fleet, perimeter, coverage — is derived now and stored never. No
  knowledge index, no cache, no daemon.
- **The workflow lives in the home, outside every repository.**
  `~/.config/ohjaa` holds each repository's entry (`repos/<owner>/<repo>/`:
  the branch and the gate, nothing else), the generated `git/` config, the
  hook dispatchers and `tools.lock`. Only repositories whose remote names an
  entry run ohjaa's hooks; no other git command anywhere starts ohjaa.
  Outside the home, `ohjaa apply` changes only one `include.path` in git's
  global config, a marked block in git's global excludes file and one line of
  `~/.zshrc`, records each, and `apply --undo` puts them back; `ohjaa adopt`
  writes only the entry and the home's `git/`. Nothing is installed on or
  written into a repository, and the dispatcher always runs a repository's own
  hooks after the gate.
- **`ohjaa run` stays the parent of the session until it ends**, so leaving
  always writes its record, whatever the exit. Session states are observed, not
  declared: the agent's lifecycle testimony is its read-back (`ohjaa oriented`)
  and handover (`ohjaa handover`). Imported usage is separately labelled source
  testimony; it never becomes observed work or authoritative billing.
- **Agents propose; only a person makes Knowledge current**, retires it or
  rejects it. Shared Knowledge (ours, repo) is reviewed in a pull request
  before any agent reads it. A proposal never lands in a shared repository by
  itself.
- **Cross-repo yes, messaging no.** Permanently refused: request/response
  between agents, presence, and dispatch (assigning work to an agent).
  Knowledge is not a message — it carries its time, its why and whose it is. A
  session's handover is a record for whoever comes next, shown in the next
  briefing, never a message to a particular agent.
- **Network only through `gh`, the drafter and cargo** — in `ohjaa intake`
  (review comments, drafting), when a session leaves (its branch's pull
  request, bounded, recorded as unknown when gh cannot say), and in
  `ohjaa apply --tools` (`cargo install` of the pinned revisions). Every other
  command works from the filesystem and local CLIs.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.

## If you are an agent working here

- Never run `ohjaa accept`, `reject`, `retire`, `confirm` or `review` on a
  person's behalf, and never fake a terminal to get past that refusal. Propose
  with `ohjaa propose` and say so.
- Inside an `ohjaa run` session: read back with `ohjaa oriented` before your
  first change, and write the handover with `ohjaa handover` before you finish.
  Never write another session's testimony.
- Never edit `vendor/faija-knowledge/`; refresh it with
  `scripts/vendor-faija-knowledge.sh`. The format belongs to faija.
- Tests must never read or write the person's real knowledge: the CLI tests
  sandbox `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `OHJAA_OURS`. The
  workflow-home tests (`tests/config_home.rs`) empty the environment and also
  sandbox `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`, `CARGO_HOME`, `TMPDIR`
  and `PATH`: `apply` writes git's global config and `~/.zshrc`.

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

## Languages

Code in this repository is **Rust, or Elixir/Erlang**. Never add Python, Go
or Node (JavaScript/TypeScript): not for tools, scripts, CI helpers, tests,
dashboards or glue. A thin shell step in a CI workflow is fine; anything with
logic is Rust. Existing Python or Node files are debt to be rewritten, not
precedent to follow.
