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
  committed, finished and handed over — deleting it loses no knowledge, it never
  blocks a launch, and a broken log is reported, never replaced. Everything
  else — fleet, perimeter, coverage — is derived now and stored never. No
  knowledge index, no cache, no daemon.
- **`ohjaa run` stays the parent of the session until it ends**, so leaving
  always writes its record, whatever the exit. Session states are observed, not
  declared: the agent writes only its read-back (`ohjaa oriented`) and its
  handover (`ohjaa handover`).
- **Agents propose; only a person makes Knowledge current**, retires it or
  rejects it. Shared Knowledge (ours, repo) is reviewed in a pull request
  before any agent reads it. A proposal never lands in a shared repository by
  itself.
- **Cross-repo yes, messaging no.** Permanently refused: request/response
  between agents, presence, and dispatch (assigning work to an agent).
  Knowledge is not a message — it carries its time, its why and whose it is. A
  session's handover is a record for whoever comes next, shown in the next
  briefing, never a message to a particular agent.
- **Network only through `gh` and the drafter** — in `ohjaa intake` (review
  comments, drafting) and when a session leaves (its branch's pull request,
  bounded, recorded as unknown when gh cannot say). Every other command works
  from the filesystem and local CLIs.
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

## Languages

Code in this repository is **Rust, or Elixir/Erlang**. Never add Python, Go
or Node (JavaScript/TypeScript): not for tools, scripts, CI helpers, tests,
dashboards or glue. A thin shell step in a CI workflow is fine; anything with
logic is Rust. Existing Python or Node files are debt to be rewritten, not
precedent to follow.
