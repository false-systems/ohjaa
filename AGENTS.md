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
  dispositions while a session runs, and the kernel's word on processes) — and
  every addition needs a reason. Tests
  alone also use `sha2` and `serde_yaml` (the parser toimija reads
  `.toimija/repo.yaml` with, so a config toimija cannot parse fails CI here).
- **Ohjaa holds Knowledge; toimija reports facts.** Never reimplement anything
  toimija derives — shell out to its CLI and consume its `--json`.
- **Ohjaa stores two things.** Knowledge, as knowledge-node.v1 files in the
  scope directories plus proposals waiting for a person, is the truth and lives
  in git. The **session log**, in redb at `$XDG_STATE_HOME/ohjaa/sessions.redb`,
  is machine-local history of what each session was briefed, read back,
  committed, finished and handed over, plus explicitly attributed usage receipts, references to the validations `ohjaa gate` and ohjaa itself requested (sykli's run ids and receipts, never outcomes), and the Judgements `ohjaa judge` recorded with their answers (each written once, never changed) — deleting it loses no knowledge, it never
  blocks a launch, and a broken log is reported, never replaced. Beside it,
  `$XDG_STATE_HOME/ohjaa` holds one validation folder per clone
  (`integrated/<clone>/`: the clone's one rolling validation checkout, the
  cargo target dir that checkout builds in, and copies of its runs'
  receipts) and the locks that let one ohjaa at a time ask for a clone's
  validation or write its notes. Everything
  else — fleet, perimeter, coverage, marks — is derived now and stored never. No
  knowledge index, no cache, no daemon.
- **The workflow lives in the home, outside every repository.**
  `~/.config/ohjaa` holds each repository's entry (`repos/<owner>/<repo>/`:
  the branch ohjaa observes (`repo.toml`, nothing else), how its Judgements
  are shared (`sharing.toml`, written by a person), the gate it is judged by,
  plus its generated hooks folder and `gated.config`), the generated `git/`
  config and `roots`. Only
  repositories whose remote names an entry run ohjaa's hooks, and only the
  hooks that entry needs; no other git command anywhere starts ohjaa, and
  ohjaa works its git facts out once instead of asking git again. Outside the
  home, `ohjaa apply` changes only one `include.path` in git's global config,
  a marked block in git's global excludes file and one line of `~/.zshrc`,
  records each before making it, and `apply --undo` puts them back; `ohjaa
  adopt` writes only in the home. Nothing is installed on or written into a
  repository but what git keeps beside it: Judgements' notes, data refs
  (`refs/notes/ohjaa/judgements`, append-only, pushed only when asked, and
  `refs/notes/ohjaa/fetched/judgements`, origin's as last fetched); and the
  clone's one validation checkout, a linked worktree kept in ohjaa's state
  directory (`$XDG_STATE_HOME/ohjaa/integrated/<clone>/checkout`, whose admin
  folder git keeps in the clone's `.git/worktrees/`). No file, hook or config
  of a repository is written, and
  the dispatcher always runs a repository's own hooks after observing.
- **Observe, mark and flag; never refuse for the workflow.** A push to an
  entry's branch is handed to sykli and, when sykli does not prove it,
  pushed and flagged, never stopped; only a repository's own hooks refuse.
  Each commit on that branch is marked proven, failed or unproven from the
  receipts on disk, and a briefing opens with the flag while the branch is
  not proven at its tip. A landing on it whose tree sykli has no current
  receipt of is validated as the integrated State (a durable sykli run,
  recorded before it is asked for), and a candidate validated on a base the
  branch has moved from since is flagged, never refused.
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
- **Network only through `gh`, the drafter and the Judgements' notes ref** —
  in `ohjaa intake` (review comments, drafting), when a session leaves (its
  branch's pull request, bounded, recorded as unknown when gh cannot say), and
  `git push` or `git fetch` of `refs/notes/ohjaa/judgements` only when asked
  (`ohjaa judge --push` or an entry's `push_judgements`, for an entry whose
  `sharing.toml` shares them; `ohjaa judgements --fetch`, into
  `refs/notes/ohjaa/fetched/judgements`). A push publishes the union of this
  clone's lines, origin's and the session log's, never a merge that drops
  one; for an entry that is not private, never a line or a notes commit that
  names anyone, at the tip or anywhere in the history it would send, and
  none while origin's own notes already name anyone. Every
  other command works from the filesystem and local CLIs.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.

## If you are an agent working here

- Never run `ohjaa accept`, `reject`, `retire`, `confirm` or `review` on a
  person's behalf, and never fake a terminal to get past that refusal. Propose
  with `ohjaa propose` and say so.
- Inside an `ohjaa run` session: read back with `ohjaa oriented` before your
  first change, and write the handover with `ohjaa handover` before you finish.
  Never write another session's testimony.
- Never judge a Change you made, and never work around `ohjaa judge`'s law-2
  refusal (another identity, another session's id, a dropped
  `OHJAA_SESSION`, a session started inside your own, a process left
  behind). This rule is testimony, not a Boundary: ohjaa observes the
  judging process from the kernel (any session, live or ended, whose process
  group holds it or an ancestor), not its environment, and records what it
  cannot show as not established. `established` means no sign of an author,
  never proof of a person: a process that fully left every recorded process
  group and acquired a terminal can still look like one. Answer a Judgement
  with `ohjaa judge respond`; never edit or remove one.
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
