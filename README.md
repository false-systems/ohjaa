# ohjaa

**Steering knowledge for agents, handed to them before they act.** *Ohjaa* is
Finnish for *steer*.

Toimija reports facts and refuses to decide. Ohjaa holds the other half: what
the people an agent works for **know and have decided**. That covers the
decisions, facts, taste and lessons that
[`faija-ontology.v2`](https://github.com/false-systems/faija) calls
**Knowledge**. Every agent launched through ohjaa starts with that Knowledge,
followed by context for its launch repository and declared scope. The full
fleet remains available on demand.

```bash
ohjaa run claude --intent "…"          # brief the agent, then hand off to toimija
ohjaa brief                            # the briefing an agent gets here
ohjaa brief --scope ../other-repo       # retrieve context after expanding scope
ohjaa propose --scope ours --id rust-only --kind decision \
      --origin "2026-09-16: 'we dont do python'" "Code is Rust or Elixir/Erlang."
ohjaa review                           # a person decides what becomes current
ohjaa intake                           # draft proposals from what people said
ohjaa check                            # validate, and flag what may no longer hold
ohjaa session                          # the record of the last ended session here, or of one by id
ohjaa graph rust-only                  # a node, its neighbours, who was briefed with it
ohjaa fleet                            # who is live, across every repo
ohjaa system                           # what every tool in the fleet is, derived now
```

## Why

Between April and September 2026, a person corrected coding agents about 151
times. The corrections break down roughly like this:

- **~60%:** the agent did not know the person's world: current names, the Lima
  VM, "Rust only", "products stand alone".
- **~26%:** the agent overstepped.
- **~10%:** the agent stalled, because it could not tell what was safe.

The top five corrections alone recurred about 70 times. A correction you have
to repeat is Knowledge that failed to reach the agent. Ohjaa is where that
Knowledge lives and how it reaches the agent.

## Knowledge

One node per file, in the [`knowledge-node.v1`](https://github.com/false-systems/faija/blob/main/docs/knowledge-node-v1.md)
format: TOML frontmatter, then the body an agent reads.

```
+++
id = "lima-for-linux"
kind = "fact"
status = "current"
as_of = 2026-09-24
origin = ["15 corrections, 2026-05-28 to 2026-09-05: 'we have lima'"]
holds_while = ["command:limactl"]
+++
Linux and eBPF builds and tests run in the Lima VM. Never report them as
blocked on macOS.
```

A node's scope, meaning whose it is, is the directory it lives in:

| Scope | Lives in | Changes by |
|---|---|---|
| ours | `<shared checkout>/knowledge/`, set by `OHJAA_OURS` or `~/.config/ohjaa/ours` | pull request |
| repo | `<repo>/.ohjaa/knowledge/` | pull request |
| mine | `~/.config/ohjaa/mine/` | you |
| machine | `~/.config/ohjaa/machine/` | you |

The launch repository's briefing is ordered **ours → repo → mine → machine**, then **decision →
fact → taste → lesson**. A personal node can never supersede a shared one, and
shared Knowledge never builds on personal Knowledge. Those rules come from
faija's contract, and ohjaa carries its reference implementation in
`vendor/faija-knowledge`.

Every explicitly scoped repository also contributes its complete repo knowledge,
labelled with its path. Global knowledge is delivered once, not repeated for each
repo. No keyword matcher drops a decision. Session records preserve the repository
identity of delivered repo nodes; older records without that identity still read.

## Agents propose, a person decides

- `ohjaa propose` writes a proposal to `~/.local/state/ohjaa/proposals/`,
  **outside every repository**. An agent's draft never lands in a shared repo
  by itself.
- `ohjaa review`, `accept`, `reject`, `retire` and `confirm` refuse to run
  without an interactive terminal, and agents run without one.
- Accepting a node that supersedes another marks the old node `superseded` in
  the same step, and refuses outright if the result would not hold together.
  Nothing is deleted: retired and superseded Knowledge keeps its time and its
  why.
- Shared Knowledge (ours, repo) is then committed and reviewed in a pull
  request, like code. For shared scopes that review is the real gate.

**How strong the person-only guard is:** it stops a careless agent, which is
the kind the correction history shows. It does not stop a determined one. A
program can fake a terminal, so for personal scopes the terminal check is a
line an honest agent does not cross, not a lock.

## Intake: corrections become proposals

```bash
ohjaa intake                                  # from where the last run stopped
ohjaa intake --since 2026-04-01 --max-batches 50
ohjaa intake --source transcripts --dry-run   # show the prompt, send nothing
```

`ohjaa intake` reads three sources:

- **Transcripts:** Claude Code and Codex sessions, keeping only what the person
  typed.
- **Reviews:** human comments on pull requests and issues, through `gh`.
- **Reverts:** reverted commits.

It sends them in batches to a **drafter**, an agent CLI that reads a prompt on
stdin and prints a JSON array. The default is `claude -p`; override it with
`--drafter` or `~/.config/ohjaa/drafter`. The drafter sees the existing
Knowledge. When a correction repeats something already known, it drafts a
**lesson** that refines that node rather than a duplicate, and that is how a
node that is not reaching agents gets noticed.

Every draft is validated like any proposal, and an invalid one is refused out
loud. The cursor moves only past what was actually drafted from.

## Sessions: entering to leaving

`ohjaa run` runs each agent session through five states, and stays its parent
until it ends so the last one always happens:

```
1 entering → 2 oriented → 3 working, phase 1 … n → 4 finished work → 5 leaving
```

| State | What happens | Written by |
|---|---|---|
| 1 entering | knowledge, the last session here, lifecycle instructions, and scoped context | ohjaa |
| 2 oriented | the agent reads back what it knows: `ohjaa oriented "<goal; rules; who else>"` | the agent |
| 3 working | small phases, each a commit and, where the repo has a sykli contract, a `sykli run` on it | observed from git and `.sykli/receipts` in every checkout `git worktree list` names: the commits a reflog (a checkout's HEAD, or a branch) shows created since entering and still reachable, never ones a pull or merge brought in, nor one an amend or rebase replaced (a rebase's replayed commits are made anew, and count). A commit is the session's own only where that can be shown (see below); every other one made meanwhile is recorded as observed, not attributed |
| 4 finished | the pull request of the branch this session's newest commit on a branch was made on (by name, then by commit once a squash merge deleted the branch; none asked when it made no commit on a branch), and what `sykli verify --commit` says about its head; every commit that reached the default branch during the session, checked the same way | observed through `gh`, git and sykli |
| 5 leaving | the record: observed part, then the handover (`ohjaa handover`) | ohjaa, and the agent's testimony |

States are **observed, not declared**: the agent writes only its read-back and
its handover, and the record shows them as its words. Leaving always writes the
record, crash or not; a missing handover is recorded as none. Only losing ohjaa
itself (a SIGKILL, a crash, a restart) skips leaving: the next launch finds that
ohjaa gone, closes the session as `orphaned (observed at next launch)`, queues it
for `ohjaa intake`, and opens its briefing with it when it ran here. Only the
absence is observed, never a cause, and an orphan ranks by when it started: a
session that really ended after that stays the last session here.

The next session here, and in every repository it named with `--scope` (never
one it was only found working in), is briefed with the last session's record:

```
session oh-1790… · claude · selko · 2026-09-26 10:02 UTC → 2026-09-26 11:47 UTC · ended: clean (exit 0)
─ observed (written by ohjaa; the agent cannot change it) ─
intent        retry uploads
knew          14 knowledge node(s) delivered · read back at 10:04 UTC
phases        a1b2c3d receipt: passed · d4e5f6a receipt: passed · 0718abc receipt: failed · 9c0d1e2 receipt: passed
finished      https://github.com/…/pull/57 (open) · head 9c0d1e2: verified: passed (sykli verify --commit)
─ handover (the agent's own words) ─
unfinished    Windows paths in uploader/retry.rs
next agent    Dima owns the queue; ask first
surprised by  the fixture helper silently skips when git is missing
```

A receipt's `outcome` is its own claim, and the record says it as one
(`receipt: passed`); a receipt is filed under the tree that ran (a run sykli's
pre-commit hook makes before the commit is that tree's run), and a passing run
outranks a newer failing or dirty one. Only sykli says a commit passed. It is
asked about the pull request's head; about the tip of every push of the
session's that reached the default branch (pushed from here to
`origin/<default>`, or made here and now on the local one; a fetch is not a
landing), which is what a pre-push Boundary gates; and about the commit
`origin/<default>` is left at when a push from this clone put it there,
whoever pushed. The commits before a tip are shown as their receipts' word.
Leaving runs `sykli verify <receipt> --contract sykli.json --commit <sha>` in a
checkout at that commit with nothing beside it, when there is one, else where
the receipt is:

| sykli says | the record says | flag |
|---|---|---|
| exit 0 | `verified: passed` | no |
| refused the commit's own run (another tree, a dirty run, a failed outcome, contradictory records) | `FAILED sykli verify: …` | **yes** |
| exit 1, 3 or 4, by a sykli that reads everything from the commit (sykli#67 as merged; its help says "nothing is read from the checkout") | `FAILED sykli verify: …` | **yes** |
| refused the tree, inputs or contract on disk, by an older sykli in a checkout that is not the commit | `stale: sykli checked the checkout, not the commit` | no |
| exit 2: it could not verify (a bad revision, arguments, not a receipt) | `unverified: sykli verify could not verify: …` | no |
| any other end: a crash, a kill, no answer in time | `unverified: …` | no |
| no `--commit` (`sykli verify --help` does not name it) | `unverified: sykli has no --commit` | no |
| — the commit has no sykli contract | `unverified: no sykli contract …` | no |
| — no receipt names the commit's tree (the commit has a contract) | `unchecked` | **yes** where it is `origin/<default>`'s tip; otherwise no |

A refusal of the session's own work **flags the record**, and so does the tip
`origin/<default>` was left at when sykli refused it or it was never run where
there is a contract: a broken main is the repository's state, whoever pushed it
(another session sharing the clone, a person by hand), so it is said either way,
as `observed, not attributed` when it is not shown to be this session's. Only
the session's own refused work becomes an intake candidate. The record opens
with its flags, leaving ends by saying them again (`ohjaa: flagged: …`), and
the next briefing here, `ohjaa brief` included, opens with them before the
knowledge and before the handover. "`origin/main` at `<sha>`" is said only of
the commit the branch sits at, when the record is written and again when a
briefing is made; once the branch has moved on, the session's own refused
commit is said as having reached it, and a tip it only observed is no longer
said:

```
WARNING from the last session here (oh-1791…, claude, ended 2026-10-09 21:30 UTC):
- origin/main at 3dbaafd in selko has no passing evidence — check before building on it (FAILED sykli verify: refused: outcome expected passed …)
```

An earlier session's flag about a branch here keeps opening the briefing for as
long as that branch still sits at the commit it flagged (`WARNING from an
earlier session here (…), still true`), so a quiet session in between cannot
hide a broken main; so does one a root session saw in a repository it only
found (`WARNING from a session that observed this repository`). Each tip is
said once.

The records live in a **session log**, redb at
`~/.local/state/ohjaa/sessions.redb`, on this machine only. Knowledge never
lives there, so deleting the log loses no knowledge. A busy or broken log never
blocks a launch; a broken one is reported and left alone, never replaced.
`ohjaa intake` drafts proposals from each ended session once: its handover,
what was said in its window, and what ohjaa observed going wrong in it (source
`observed`): its own work that sykli refused at the default branch or a pull
request head (never what could not be checked, nor what is not shown to be the
session's); a non-zero exit; a missing handover; an orphaned session. Those are
facts, not anyone's words, and the drafter is told so.

Signals, while a session runs (unix):

- **Ctrl-C** reaches the agent directly (it is in the same process group).
  ohjaa survives it and never passes it on: a second Ctrl-C makes agents like
  `claude` quit instead of interrupt.
- **SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2** sent to ohjaa are survived and
  passed on to the session once each. To stop a session from outside, send
  SIGTERM to ohjaa.
- **After the session ends**, any of these cuts leaving short (the tools still
  running are killed) and the record is still written, with what was known by
  then; the normal dispositions come back once it is saved.
- **SIGKILL** cannot be caught: it loses the record.

## Session usage

Usage stays in the existing session log. It is **imported source testimony**,
separate from ohjaa's observed work and from provider billing. No new service,
database or sibling-repository dependency is required.

```bash
# A local collector such as Vessa creates a native-session snapshot.
vessa receipt --input /path/to/session.jsonl > /tmp/session-usage.json
ohjaa usage import /tmp/session-usage.json --session oh-EXISTING-ID
ohjaa usage export > /tmp/ohjaa-sessions.json
```

Inside a run, `usage import` defaults to `OHJAA_SESSION`. To pick up a receipt
automatically when the agent command ends, use `ohjaa run ... --usage-file
/path/to/fresh-receipt.json`. Ohjaa passes that path as `OHJAA_USAGE_FILE`; the
launched command or its wrapper must write an `agent-usage.v1` receipt there,
and its `agent` must be the basename of the command ohjaa launched (a wrapper
launched as the agent claims its own name, not `claude`). A refused receipt is
recorded as `refused: <reason>`, never as not collected.
Native agent transcripts are not that receipt format. Existing paths are
refused so an old receipt cannot be accidentally reused. Missing/invalid
receipts are reported and never change the agent's exit status. No automatic
time-window or directory matching is performed between concurrent sessions.

For Codex launches, ohjaa also passes the generated `OHJAA_SESSION`, the
resolved `XDG_STATE_HOME`, and optional `OHJAA_USAGE_FILE` through per-session
`shell_environment_policy.set` overrides. Codex tool commands can run in a
persistent daemon that did not inherit the wrapper environment. Toimija passes
its own handshake independently. Neither wrapper modifies global Codex config
or relaxes environment filtering or sandbox permissions. Existing sessions need
a fresh wrapped launch; no session is recovered by guessing from directory or
time. See [Codex's configuration reference](https://developers.openai.com/codex/config-reference/).

`agent-usage.v1` is a cumulative snapshot of one native session. It contains
`agent`, `session_id`, `source`, `observed_at_ms`, `complete`, `limitations`,
and `entries` grouped by `provider` and `model`. Each entry has nullable
`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
`estimated_usd_micros`, `estimate_basis`, `credits` and `billed`. Input excludes
cache categories. An amount is `{unit, micros, source}`: credit units are
provider-specific; billed units are currencies. `null` means unknown, never
zero. Estimates require their rate basis; amounts require their source.

Imports validate the shape and agent, not the truth of a provider claim.
Repeating an identical snapshot changes nothing; a newer snapshot replaces
the earlier one, rather than adding its cumulative totals. Stale/conflicting
snapshots are refused. A native session can belong to only one wrapper
session; after resuming it, update that original association rather than
double-attribute its entire history to a second run. This first contract does
not split a native session's consumption across several wrapper runs.

`usage export` emits `ohjaa-sessions.v1`, with `sessions` and `unreadable`.
Unreadable records produce exit 4. `--session ID` selects one record. Existing
records without usage remain readable and explicitly show "not collected".
Exports contain private session context; choose their destination accordingly.

## Keeping it true

`ohjaa check` validates every node and the rules across them. It also flags
what may no longer hold:

- a `holds_while` predicate that is false right now;
- a **fact** not re-confirmed for 90 days (`ohjaa confirm <id>`);
- a briefing longer than an agent will reliably read (8,000 characters). The
  fix is to merge or retire nodes, never to truncate.

A flag never hides a node. The briefing shows it with the flag beside it.

For agents not launched through ohjaa, `ohjaa brief --write <file>` writes the
Knowledge as a generated block into an instruction file such as `CLAUDE.md` or
`AGENTS.md`, and leaves everything else in the file alone.

## The fleet and the perimeter

`ohjaa system` derives what every tool in the fleet is, from each repo's own
`.toimija/repo.yaml`. `ohjaa fleet` reports who is live, from `toimija
sessions` in each repo and `toimija sessions --outside-repos` for the sessions
running outside any repo (listed under "outside any repository", by folder).
Both are **derived now, stored never**: a derived view cannot be staler than
the moment it was computed. `ohjaa run` and `ohjaa brief` include only the launch
repository and repositories named by `--scope`, after the Knowledge. Sessions
outside repositories are included when they are in the launch folder or their
declared scope reaches a selected repository. Other repositories are explicitly
unobserved; `ohjaa system` and `ohjaa fleet` still derive the full fleet.

`run` reuses its launch preflight scans when composing the briefing. It does not
ask Toimija for every repository's context or repeat the session scans. Coverage
failures remain visible and selected-repository launch checks still refuse to
launch blind. `brief --scope PATH` can retrieve context when a task grows; it
does not declare session scope, so declare it with `toimija intent` before work.
Missing scope repositories produce exit 4 from `brief`, never an empty success.

The briefing tells an agent to use delivered facts instead of repeating discovery.
With no concrete task, it acknowledges and waits: the wrapper already recorded
entry. Orientation precedes the first task change; handover belongs at work
completion or session exit, not after every conversational reply. No knowledge
is truncated to save tokens, and no new index, database or model call is needed.

The root list is derived as the sibling directories of your git root (outside
a repo, the folder you are in), unless `~/.config/ohjaa/roots` or `--root`
overrides it. A root that does not exist is an error, never an empty fleet.

## Two agents, one checkout

```bash
# terminal 1
ohjaa run codex --intent "change the parser" --task "fix the lexer" --scope src/parser/
# terminal 2, same directory
ohjaa run claude --intent "refresh the guide"
# everything after `--` reaches the agent command itself
ohjaa run codex --intent "change the parser" -- --model o3
```

- Each agent gets its own toimija session.
- Knowledge and lifecycle context travel as `toimija run --briefing`, separately
  from `--task`. With an updated Toimija, an idle Codex launch opens without a
  model acknowledgement turn; the first user message still receives the briefing.
  Explicit tasks and other agents retain prompt delivery. Update Toimija before
  using this version of Ohjaa; older versions reject the new flag rather than
  silently dropping context.
- `--dry-run` prints the exact command it would exec, without launching.
- If toimija cannot fully read the sessions, or does not report its coverage,
  ohjaa refuses to launch blind.

## Agents launched from the root

Launch agents from the folder that holds your repositories, and name the ones
each agent will work in with `--scope`:

```bash
cd ~/projects
ohjaa run claude --intent "rename the flag everywhere" --scope ~/projects/a --scope ~/projects/b
```

ohjaa briefs it with your Knowledge, each scoped repo's Knowledge and context,
and the last session in this folder. With no repository selected, it adds no
repository roster. It hands the session to toimija, which opens it outside every repo: no
packet, but visible — `ohjaa fleet` lists it under "outside any repository".
Every repository named with `--scope` is treated like the one a session is
launched in:

- **at launch**, ohjaa counts the sessions live in each scoped repo, and
  refuses to launch blind if it cannot read them;
- **at leaving**, it records, per scoped repo, the commits the agent made there
  (with what sykli saw on each) and the branch's pull request;
- **the other way round**, a session launched inside a repo counts the sessions
  from the root whose `--scope` names it (`ohjaa fleet` lists them under
  "outside any repository", with their scope);
- **the next agent there** is briefed with it: "the last session here" is the
  one that ended last, launched in that repo or elsewhere with a `--scope`
  naming it, and one launched elsewhere says where it was launched.

**Observed, not asserted (law 3).** A commit is the session's own only where
that can be shown: it was made in the checkout the session was launched in or
named with `--scope`, or in a worktree or on a branch created during the
session, or pushed from here; and no other session sharing that place was there
at that second (other sessions in the log, and live ones toimija saw at launch;
for a place created during the session, any session in the same clone). Only
its own commits are its phases, its pull request and its flags. Everything else
made during the session is recorded as `observed … not attributed`, with the
session it shared the place with (`shared with oh-… in work, …`).

A repository nobody named is observed when a commit was made in it: at leaving,
ohjaa looks at the repositories directly in the launch folder (and in any folder
`--scope` named that is not itself a repository), and records each where a
commit was made, or pushed from, during the session, in any of its worktrees,
as **found, not named**. Nothing in a found repository is attributed to the
session: it is not its phases or flags, no pull request is looked up, and the
session never becomes that repository's "last session here". Name repositories
with `--scope` wherever you can. A `--scope` path that is neither in a repository nor
a folder is named as unobserved. Scope can be named from inside a repo too, for
a second repository an agent will touch.

A toimija too old to list sessions outside repos makes ohjaa refuse to launch
from the root, and makes `ohjaa fleet` incomplete — never quietly empty.

## Coverage is a fact

A scan that failed is not a scan that found nothing:

```
coverage: scanned 17/18 repo(s)
  incomplete — absence here is not evidence of absence:
    somerepo: not a Git repository
```

- `ohjaa fleet` exits `4` when coverage is incomplete.
- A session toimija reports as `stale` stays visible, with its age, because
  "quiet for ninety minutes" and "gone" look the same from outside.

## What it will never do

- **No messaging between agents, no request/response, no presence.** A message
  from an agent that has since died is a silent lie. Knowledge is not a
  message: it carries its time, its why and whose it is, and a person reviewed
  it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests. A handover is a record for whoever comes next here, shown in
  the next briefing, never a message addressed to a particular agent.
