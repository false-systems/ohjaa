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
  in git: ours in its repository, repo in the repository's own AGENTS.md (its
  `## Lessons`, one line per node, through a pull request ohjaa opens), mine
  and machine in the home's own history. Each accepted repo node is kept with
  its why, and where its line went, in `$XDG_STATE_HOME/ohjaa/lessons/<clone>/`
  (the nodes and `delivery.json`; `<clone>` the clone's common git directory),
  beside the worktrees a delivery writes in while it runs (`lessons/.work/`)
  and its lock (`lessons.lock`): knowledge, not a cache. The **session log**,
  in redb at `$XDG_STATE_HOME/ohjaa/sessions.redb`,
  is machine-local history of what each session was briefed, read back,
  committed, finished and handed over, plus explicitly attributed usage receipts, the ids of the agent's own sessions found at leaving in its transcripts (read, never written) and those its environment claimed from inside the session while it was open (the agent's testimony, never outranking what was found), and references to the validations `ohjaa gate` and ohjaa itself requested (sykli's run ids and receipts, never outcomes) — deleting it loses no knowledge, it never
  blocks a launch, and a broken log is reported, never replaced. Beside it,
  `$XDG_STATE_HOME/ohjaa` holds one validation folder per clone
  (`integrated/<clone>/`: the clone's one rolling validation checkout, the
  cargo target dir that checkout builds in, and copies of its runs'
  receipts), the lock that lets one ohjaa at a time ask for a clone's
  validation, and the gates derived from commits that a run outliving its
  ohjaa names (`derived/<digest>/`). It also holds intake's cursor
  (`intake-since`), intake's progress (`intake-handled`: a version line; per
  candidate the drafter answered for or failed on, its time and a digest of
  its words, never the words, and for a failure how often and why in ohjaa's
  words, kept until the drafter answers for it; an end line with the count),
  the lock that lets one intake run at a time (`intake.lock`), the
  report's two times (`report-at`, `report-shown-at`), and the human view's
  folder (`human/`: `seen.json`, what the person last saw and when, the keys
  of what was said in the last two hours, where the next digest begins and
  where the earliest of what a day's digest left out begins, each with the
  keys of what GitHub said in the two hours before it; `collected.json`,
  the snapshot `ohjaa collect` writes of what `ohjaa line` cannot derive
  without git or the network, namely the marks and open sessions, GitHub's
  pull requests and vessa's usage of the sessions ended lately, each with
  when it was read and what it could not read; and two empty lock files,
  neither a cache: `seen.lock`, held while the cursor is read and moved, by
  the terminal saying the line and by `ohjaa digest` shown to a person, and
  `collect.lock`, held by the one collect that runs). Deleting them loses
  no knowledge: intake then reads its default window again, and sends once
  more what is still in it, and the human view starts from the last day.
  Everything else — fleet, perimeter, coverage, marks — is derived now and
  stored never, but in the human view's snapshot: a copy that is said with
  its age wherever it is shown, read only by `ohjaa line`, `ohjaa digest`
  and `ohjaa collect` (which keeps from it, as of then, what a source or a
  branch held when that cannot be read again), never by a briefing, a gate
  or a report. No knowledge index and no daemon; that snapshot is the one
  cache.
- **The workflow lives in the home, outside every repository.**
  `~/.config/ohjaa` holds each repository's entry (`repos/<owner>/<repo>/`:
  `repo.toml`, the branch ohjaa observes and the env lines its gate gives
  cargo tasks, plus its generated hooks folder and `gated.config`; every
  commit's gate is derived, from its own `sykli.json` and `sykli.lock`, or
  from `sykli init` on its tree for a repository with none, plus the
  entry's env lines, and ohjaa keeps no stored gate), the generated `git/`
  config and `roots`. Only
  repositories whose remote names an entry run ohjaa's hooks, and only the
  hooks that entry needs; no other git command anywhere starts ohjaa, and
  ohjaa works its git facts out once instead of asking git again. The home
  is also a local git repository, its history: ohjaa makes it with its first
  commit (or uses the one the home already is), never gives it a remote and
  never pushes it, and commits there, with the person's git identity, each
  decision on mine or machine knowledge and each `apply`, `apply --undo` and
  `adopt` (`git add -A`, then `git commit`), each under the home's lock
  (`.git/ohjaa.lock`) and holding only what ohjaa keeps there (an allowlist
  in the repository's own `.git/info/exclude`); a commit that fails is said,
  and the change stands. Outside the
  home, `ohjaa apply` changes only one `include.path` in git's global config,
  a marked block in git's global excludes file, one line of `~/.zshrc`, and a
  marked block of mine and machine knowledge (`ohjaa:knowledge`) in each
  agent's own global instruction file that already exists outside every
  repository (Claude Code's `~/.claude/CLAUDE.md`, Codex's
  `~/.codex/AGENTS.md`, pi's `~/.pi/agent/AGENTS.md`, opencode's
  `~/.config/opencode/AGENTS.md`, each where the agent finds it; none is
  created, no text around the block is touched, and a block apply did not
  write is the person's and stays), records each before making it, and
  `apply --undo` puts them back; each decision rewrites that block where it
  is, and nowhere else; `ohjaa
  adopt` writes only in the home. Nothing is installed on or written into a
  repository but what git keeps beside it: the clone's one validation
  checkout, a linked worktree kept in ohjaa's state
  directory (`$XDG_STATE_HOME/ohjaa/integrated/<clone>/checkout`, whose admin
  folder git keeps in the clone's `.git/worktrees/`), and, while repo
  knowledge is delivered, a worktree off the remote's branch
  (`$XDG_STATE_HOME/ohjaa/lessons/.work/`), removed once its branch is pushed.
  No file, hook or config of a repository's checkout is written; `ohjaa
  lessons publish` takes out of it the `.ohjaa/knowledge/` files a person
  accepted before, once they are in a pull request, and
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
  rejects it. Ours is reviewed in a pull request before any agent reads it.
  Repo knowledge a person accepts goes into the repository's AGENTS.md
  through a pull request ohjaa opens and the person merges; until its line is
  on the checked-out commit, the briefing carries it, marked, and after, it
  leaves it to AGENTS.md only for an agent shown to read that file (`repo_docs`).
  What ohjaa writes into a repository names no tool, and a repository that
  is not private is sent nothing that names what is (this tool, the
  person's private or internal repositories, those under their roots gh
  does not show as public, home path, account, login or name; never on
  gh's list alone, which must hold every repository under the roots): a
  line that would is held back, anything else that would stops the
  delivery, and no why is sent. A proposal never lands in a shared
  repository by itself.
- **Cross-repo yes, messaging no.** Permanently refused: request/response
  between agents, presence, and dispatch (assigning work to an agent).
  Knowledge is not a message — it carries its time, its why and whose it is. A
  session's handover is a record for whoever comes next, shown in the next
  briefing, never a message to a particular agent. The human view (`ohjaa
  line`, `ohjaa digest`) is a record a person reads at their own terminal,
  never a notification.
- **Network only through `gh`, the drafter and, to deliver repo knowledge,
  git to GitHub with gh's credentials** — in `ohjaa intake` (review
  comments, drafting), when a session leaves (its branch's pull request,
  bounded, recorded as unknown when gh cannot say), in `ohjaa collect` (one
  search for the pull requests of the entries' owners, at most two calls),
  and after a person's decision on repo knowledge and in `ohjaa lessons
  publish` (gh: the repository's visibility, the person's private
  repositories and name once a run, its open lessons pull request, `pr
  create`; git: one fetch and one push to the repository on GitHub, by HTTPS
  with gh's credentials; a dry run fetches and pushes nothing). Every other
  command works from the filesystem and local CLIs, and `ohjaa line` starts
  no git process either.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.

## If you are an agent working here

- User-facing output must be plain and contextual: name the repository or
  session involved, explain what happened or is missing, and show how to
  see details or act. Do not leave internal terms such as "unproven" or
  "read-back" unexplained, or truncate the command that helps the person.
- Never run `ohjaa accept`, `reject`, `retire`, `confirm` or `review` on a
  person's behalf, and never fake a terminal to get past that refusal. Propose
  with `ohjaa propose` and say so. The one exception is a test that plays the
  person in a world it made: an emptied environment whose HOME,
  XDG_CONFIG_HOME, XDG_STATE_HOME and OHJAA_OURS are all in its own temporary
  directory, as `tests/loop.rs` does. Faking a terminal against any other
  state, the person's own or a repository's, is deciding on their behalf.
- Inside an `ohjaa run` session: read back with `ohjaa oriented` before your
  first change, and write the handover with `ohjaa handover` before you finish.
  Never write another session's testimony.
- Never edit `vendor/faija-knowledge/`; refresh it with
  `scripts/vendor-faija-knowledge.sh`. The format belongs to faija.
- Tests must never read or write the person's real knowledge: the CLI tests
  sandbox `HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `OHJAA_OURS`. The
  workflow-home tests (`tests/config_home.rs`) empty the environment and also
  sandbox `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM`, `CARGO_HOME`, `TMPDIR`
  and `PATH`: `apply` writes git's global config, `~/.zshrc` and the agents'
  own instruction files, and commits in the home. A person's decisions are
  tested at a real terminal (`tests/terminal.rs`), sandboxed the same way.
- A test writes a file it will run (a stand-in, a hook, a copied binary)
  only through its crate's `write_executable` (`process::write_executable`
  in unit tests), never with `fs::write` or `fs::copy`: on Linux a sibling
  test's forked child can hold the test process's write descriptor, and the
  run fails with `Text file busy` (the README's "Tests").

## Validation

Before claiming completion, run:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Optional, where sykli, toimija and vessa are installed (CI's runners have
none): the whole loop once on the real tools, from `ohjaa run` to the next
briefing (`tests/loop.rs`; measured on 2026-10-27, it takes 8 to 16 seconds
on macOS and 3 to 8 on Linux; the README's "Tests" says what it walks):

```bash
OHJAA_TEST_SYKLI=$(command -v sykli) OHJAA_TEST_TOIMIJA=$(command -v toimija) \
  OHJAA_TEST_VESSA=$(command -v vessa) cargo test --test loop -- --ignored
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

## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, written here through a pull request when a person accepts one. A line changed here by hand is replaced the next time it is written: change a lesson where it is decided. -->

- <!-- lesson: ohjaa-env-claimed-ids-are-testimony --> **decision**: Treat a session id claimed by the agent's environment as testimony, not observation, and label it that way. Rank the evidence: launch prompt, then receipt, then env claim, then window. When the launch prompt and the env claim disagree, keep the observation and name the disagreement. (as of 2026-10-27)
- <!-- lesson: ohjaa-marks-v1-only-gains-keys --> **decision**: The `ohjaa-marks.v1` schema only gains keys, and the keys viemari was built on never go. Dropping the unread `judgements` key was the one exception, so the schema keeps its name. (as of 2026-10-27)
- <!-- lesson: ohjaa-never-writes-its-config-home --> **decision**: ohjaa writes nothing into its config home. A gate that a later run needs is kept under $XDG_STATE_HOME/ohjaa/derived/<digest>/, and a gate that is only read goes into a private $TMPDIR folder that is removed when done. `ohjaa gate --print` writes nothing. (as of 2026-10-27)
