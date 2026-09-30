# ohjaa

**Steering knowledge for agents, handed to them before they act.** *Ohjaa* is
Finnish for *steer*.

Toimija reports facts and refuses to decide. Ohjaa holds the other half: what
the people an agent works for **know and have decided**. That covers the
decisions, facts, taste and lessons that
[`faija-ontology.v2`](https://github.com/false-systems/faija) calls
**Knowledge**. Every agent launched through ohjaa starts with that Knowledge,
followed by the live perimeter of the fleet.

```bash
ohjaa run claude --intent "…"          # brief the agent, then hand off to toimija
ohjaa brief                            # the briefing an agent gets here
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

The briefing is ordered **ours → repo → mine → machine**, then **decision →
fact → taste → lesson**. A personal node can never supersede a shared one, and
shared Knowledge never builds on personal Knowledge. Those rules come from
faija's contract, and ohjaa carries its reference implementation in
`vendor/faija-knowledge`.

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
| 1 entering | the briefing: knowledge, the last session here, how this session works, the perimeter | ohjaa |
| 2 oriented | the agent reads back what it knows: `ohjaa oriented "<goal; rules; who else>"` | the agent |
| 3 working | small phases, each a commit and, where the repo has a sykli contract, a `sykli run` on it | observed from git and `.sykli/receipts`: the commits HEAD's reflog shows made in this checkout since entering, never ones a pull or merge brought in |
| 4 finished | the branch's pull request, and whether its head has evidence | observed through `gh` |
| 5 leaving | the record: observed part, then the handover (`ohjaa handover`) | ohjaa, and the agent's testimony |

States are **observed, not declared**: the agent writes only its read-back and
its handover, and the record shows them as its words. Leaving always writes the
record, crash or not; a missing handover is recorded as none. The next session
here, and in every repository it named with `--scope`, is briefed with it:

```
session oh-1790… · claude · selko · 2026-09-26 10:02 UTC → 2026-09-26 11:47 UTC · ended: clean (exit 0)
─ observed (written by ohjaa; the agent cannot change it) ─
intent        retry uploads
knew          14 knowledge node(s) delivered · read back at 10:04 UTC
phases        a1b2c3d passed · d4e5f6a passed · 0718abc failed · 9c0d1e2 passed
finished      https://github.com/…/pull/57 (open) · evidence on its head: passed
─ handover (the agent's own words) ─
unfinished    Windows paths in uploader/retry.rs
next agent    Dima owns the queue; ask first
surprised by  the fixture helper silently skips when git is missing
```

The records live in a **session log**, redb at
`~/.local/state/ohjaa/sessions.redb`, on this machine only. Knowledge never
lives there, so deleting the log loses no knowledge. A busy or broken log never
blocks a launch; a broken one is reported and left alone, never replaced.
`ohjaa intake` drafts proposals from each ended session once: its handover and
what was said in its window.

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
launched command or its wrapper must write an `agent-usage.v1` receipt there.
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
the moment it was computed. `ohjaa run` adds that perimeter to the agent's
briefing, after the Knowledge.

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

ohjaa briefs it as usual (your Knowledge, the fleet, the last session in this
folder) and hands it to toimija, which opens a session outside every repo: no
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

What ohjaa still cannot see, it says: a `--scope` path outside every repository
is named as unobserved, and work in a repository nobody named is not recorded,
so the handover has to say what changed there. Scope can be named from inside
a repo too, for a second repository an agent will touch.

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
