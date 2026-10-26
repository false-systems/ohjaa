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
ohjaa run claude --intent-id in-…      # continue that Intent in a new session
ohjaa brief                            # the briefing an agent gets here
ohjaa brief --scope ../other-repo       # retrieve context after expanding scope
ohjaa propose --scope ours --id rust-only --kind decision \
      --origin "2026-09-16: 'we dont do python'" "Code is Rust or Elixir/Erlang."
ohjaa review                           # a person decides what becomes current
ohjaa intake                           # draft proposals from what people said
ohjaa report                           # what went around the workflow this week
ohjaa check                            # validate, and flag what may no longer hold
ohjaa session                          # the record of the last ended session here, or of one by id
ohjaa graph rust-only                  # a node, its neighbours, who was briefed with it
ohjaa fleet                            # who is live, across every repo
ohjaa system                           # what every tool in the fleet is, derived now
ohjaa apply                            # apply the workflow from ~/.config/ohjaa, outside every repo
ohjaa adopt ~/projects/sykli           # give a repo an entry: the branch ohjaa observes, and its gate
ohjaa doctor                           # what differs from what apply would make it
ohjaa gate                             # prove HEAD: run this repo's gate, as a push is judged
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
ohjaa intake --since 2026-10-01 --no-prefilter  # every message, not only likely corrections
```

`ohjaa intake` reads four sources:

- **Transcripts:** what people wrote in agent sessions (Claude Code, Codex,
  pi), through `vessa messages --since <cursor> --json`
  (`vessa-messages.v1`). Vessa is the one reader of agent conversations:
  intake parses no transcript itself, and has no fallback. Each message
  travels with the agent's reply before it (its last 600 characters; marked
  when that turn failed), how it was said (typed mid-turn, an answer to the
  agent's question, a reason for refusing a tool call, a launch's task) and
  its session. One longer than 1,500 characters is a pasted brief, and
  counted, never sent.
- **Reviews:** human comments on pull requests and issues, through `gh`.
- **Reverts:** reverted commits.
- **Report:** what the bypass report finds (see below).

**A local pre-filter comes first** (no network). Most of what people write
to agents is the work, not a correction. Of what vessa lists, only what looks
like a correction or a repeated instruction is sent: the archived seula's
rules, ported unchanged (`phrase-rules.v3`, and the same words said more than
once among the messages a run reads), and every reason a person gave for
refusing a tool call. On 2026-10-26 its precision was about 65% strict, 87%
lenient, and its recall low, so it is never silent:
- Every run says how many messages vessa listed, how many were briefs, and how
  many the filter kept (by rule) and dropped (by agent), by counts, never the
  words; then how many candidates the drafter answered for.
- `--no-prefilter` sends every message.
- A dropped message is not handled, and the filter's verdict is not kept. The
  cursor passes it like any other, so it comes back only when a run reads it
  again: a `--since` before the cursor, with `--no-prefilter` or with rules
  changed since. A rule change does not reach back by itself; that would need
  a record of every message read, not only of those sent.
- PR comments, reverts, the report, handovers and observed failures are not
  filtered.

It sends them in batches to a **drafter**, an agent CLI that reads a prompt on
stdin and prints a JSON array. The default is `claude -p`; override it with
`--drafter` or `~/.config/ohjaa/drafter`. The drafter sees the existing
Knowledge. When a correction repeats something already known, it drafts a
**lesson** that refines that node rather than a duplicate, and that is how a
node that is not reaching agents gets noticed.

Every draft is validated like any proposal, and an invalid one is refused out
loud.

A run drafts from at most `--batch` × `--max-batches` candidates (400 by
default), oldest first, and never from one the drafter has already answered
for.

**Progress is kept as handled.** As soon as the drafter answers for a batch,
its candidates go into `$XDG_STATE_HOME/ohjaa/intake-handled`. Each has a time
and a digest, never the words, and the file has a version line and an end
line. An empty or cut-short file is an error, never read as less. A run
killed between an answer and that write sends that one batch again. A
message's digest is of vessa's text, which is the text intake's own reader
gave (reminder blocks stripped, trimmed), so what that reader handled is not
sent again. The exception is a Codex reply to the agent's question tool, which
vessa gives as the person's own answers: those are sent once more (15 in the
fourteen days measured on 2026-10-26; none passed the pre-filter).

**A refused batch is split.** When the drafter fails or answers no drafts, the
batch is split in halves, down to the candidate it fails on. That candidate
is tried again in later runs. After failing alone three times it is set
aside, and said by its time and source in every run after. Its record is kept
until the drafter answers for it, and a session holding one stays queued.
`--retry-failed` sends every set-aside candidate again: it reads back to the
oldest one, however far the cursor has moved past it. One that fails again
stays set aside. A run stops when it gets no answer at all, or when two
candidates fail alone in a row; what came before is kept.

**Every run makes progress while the drafter answers**, and repeated runs
drain the queue. A run exits 4 when it leaves work for the next, sets a
candidate aside, or could not read a source. Once nothing is left and every
source was read, a run says `nothing new to draft from` and exits 0. Only one
intake runs at a time: a second says `intake already running` and exits 3.

**One cursor covers every source** (`intake-since`). It moves only after a run
that read all four sources, none with a warning, and then only past
candidates that were handled, never back.
- A run with `--source`, or with a warning, leaves the cursor where it was,
  and says so: where it stays, since when, and which source holds it. So a
  warning that recurs on every run keeps the window growing until it is fixed.
- With no repositories under the roots, reviews and reverts warn, and hold
  the cursor: nothing read is not nothing there.
- vessa's exit 4 (a partial list) is such a warning: each source warning, gap
  and session limitation it names is said, and holds the cursor, while what
  it did list is drafted from. A store vessa knows of and does not read
  (opencode's SQLite) is a gap only when it was written in the window, so a
  machine where opencode is in use holds the cursor until vessa reads it.
  A vessa that cannot list messages (older than `messages`) is told to be
  updated, and holds the cursor too.
- The report's own gaps are said, and do not hold the cursor.
- A first run, with no cursor yet, keeps the start of its window (fourteen
  days back) as the cursor before it drafts.
- A `--since` before the cursor reads that stretch again, and sends once more
  what is no longer kept as handled.

## The bypass report: what went around the workflow

```bash
ohjaa report                    # the last 7 days
ohjaa report --since 30d --json # ohjaa-report.v1
```

`ohjaa report` derives, now, the marks on every entry's branch (see "Observe,
mark, flag" below) and three lists for the window:

- **Sessions without read-back or handover:** ended sessions in the log that
  never ran `ohjaa oriented`, or left no `ohjaa handover`.
- **Commits on a default branch without passing evidence.**
  - For every git repository under the roots, these are the first-parent
    commits its default branch (`origin/<default>`, else the local one; for a
    repository with an entry, `origin/<the entry's branch>`) gained in the
    window, by commit time, at most 100 per repository.
  - Each is checked with `sykli verify --commit` against every receipt of its
    tree, in every checkout and under the git common dir, and against the
    entry's gate when the repository has an entry (else the contract in the
    commit). It passes when any one of them verifies.
  - Listed: what sykli refused, and what was never run in a checkout that
    still exists.
  - The rest are counted by why: verified (and how many of those reused
    tasks from sykli's cache, `verified_reusing_cache`); no sykli contract; made in a
    checkout that is gone, with no receipt kept (merge commits among them);
    sykli could not say. A window over the cap is said.
- **Agent sessions a person started without ohjaa:** with `vessa` on PATH,
  the top-level Claude, Codex and pi sessions that vessa's header listing
  (`vessa sessions --json --headers`, `vessa-session-headers.v1`) lists in
  the window and no `ohjaa run` launched. opencode is not judged: vessa
  does not read its sessions.
  - A session belongs to a launch when the launch's usage receipt names it;
    a launch with a receipt claims no other session.
  - Otherwise a launch claims at most one session: the same agent, in the
    launch's repository or below, started while the launch ran (from 5 s
    before it was logged until it left, or now while it runs). An agent can
    write its first record minutes into its launch.
  - When more than one could be a launch's, the one whose transcript was
    last written closest to the launch's end is its own (for a launch still
    running, or closed as an orphan, the one that started closest to it).
    Each session is any launch's at most once, and the same list always
    gives the same answer.
  - Headless runs bypass nothing and are only counted: `claude -p`
    (`entrypoint` `sdk-…`), `codex exec` (`originator` naming exec), and
    intake's own drafter with them. A pi transcript does not say how pi was
    started, so a `pi -p` run counts as a person's session.
  - A session vessa could not date or place is listed apart, not judged.
  - A vessa too old for `--headers` (before vessa#5: exit 2, nothing
    listed) leaves the list not judged, and the report says to update it.
    The slow full listing is never read instead.

Every warning vessa gives about what it read, a vessa exit other than 0 or
4, an undated session, a capped window, and a root with no repositories is
a gap. A report with gaps says them, and exits 4: it is never a clean week.

Once a week, at an `ohjaa run` a person starts at a terminal, the report is
read while the agent works, and three lines of it are said after the
session's record, never in the agent's briefing. The week is marked only
when that report was complete. One with a gap is said again at most once a
day, and its last line says what the gap is, so it can be fixed (for
example ``incomplete: vessa is too old for `sessions --headers`; update
it``). Only those two times are kept (`$XDG_STATE_HOME/ohjaa/report-at` and
`report-shown-at`), like intake's cursor. `ohjaa report` records them too,
but only when it printed to a terminal: never for `--json`, a script or an
agent.

```
ohjaa: weekly report, 2026-10-03 to 2026-10-10: 2 session(s) without read-back or handover · 1 commit(s) on a default branch without passing evidence · 1 agent session(s) started without ohjaa
ohjaa:   first: 3dbaafd on origin/main in selko: FAILED sykli verify: refused: outcome …
ohjaa:   `ohjaa report` lists them all; the next summary is in 7 days.
```

`ohjaa intake` takes the report as its fourth source (`--source report`).
These become `observed` candidates:

- a session that never read back;
- a refused or unchecked commit on a default branch;
- a session a person started without ohjaa.

What could not be checked or judged never does.

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
| 3 working | small phases, each a commit and, where the repo has a sykli contract, a `sykli run` on it | observed from git, `.sykli/receipts` in every checkout `git worktree list` names, and `<git common dir>/sykli/receipts` (the receipt sykli keeps of each push it accepted, which outlives a removed worktree): the commits a reflog (a checkout's HEAD, or a branch) shows created since entering and still reachable, never ones a pull or merge brought in, nor one an amend or rebase replaced (a rebase's replayed commits are made anew, and count). A commit is the session's own only where that can be shown (see below); every other one made meanwhile is recorded as observed, not attributed |
| 4 finished | the pull request of the branch this session's newest commit on a branch was made on (by name, then by commit once a squash merge deleted the branch; when the branch, its worktree and its remote-tracking branch are all gone, even after `git fetch --prune`, its commits made here are found in the object store and the pull request is searched for by the newest: when one holds it they are shown `on no branch now`, otherwise (a stash, say) they are left out; none asked when it made no commit on a branch), and what `sykli verify --commit` says about its head; every commit that reached the default branch during the session, checked the same way | observed through `gh`, git and sykli |
| 5 leaving | the record: observed part, then the handover (`ohjaa handover`) | ohjaa, and the agent's testimony |

States are **observed, not declared**: the agent writes only its read-back and
its handover, and the record shows them as its words. Leaving always writes the
record, crash or not; a missing handover is recorded as none. Only losing ohjaa
itself (a SIGKILL, a crash, a restart) skips leaving. The session runs in a
process group of its own (`toimija run` leads it, the agent stays in it), and
its entering record names that group (`pgid`, with when it was made) before
toimija runs at all: the child says its pid, ohjaa records it, and only then
does the child exec. So a later launch tells the session apart from its ohjaa:

- ohjaa gone, the group's leader still running: **still running** (`STILL
  RUNNING:` at the top of the briefing, and `ohjaa_gone` in `report --json`).
  Its agent outlived its ohjaa and may still be changing things; the session
  is left open, and its leaving will not be recorded.
- ohjaa and the leader both gone (or an older record with no group): the next
  launch closes it as `orphaned (observed at next launch)`, queues it for
  `ohjaa intake`, and opens its briefing with it when it ran here.

The leader decides, not every member: toimija waits for its agent, so a
process the agent left behind in the group (a background shell, a server)
does not keep a dead session running. A leader that exited and was not reaped
yet is gone, and a process with that pid in another group, or one that
started after the group was made, is another process, the id reused. Under a
wrapper without job control (`sh -c 'ohjaa run …'`) at a terminal, the session
shares ohjaa's process group instead (see the signals below), and has no
group of its own to be told apart by: a later launch judges it by its ohjaa
alone, as before. Only the absence is observed, never a cause, and an orphan
ranks by when it started: a session that really ended after that stays the
last session here.

**Intent.** An Intent has an id of its own (`in-<ms>-<pid>`, minted in real
milliseconds, so never a session's `oh-<ms>-<pid>`): `ohjaa run --intent
"…"` mints one, and `ohjaa run <agent> --intent-id <id>` continues it in a new
session, its text coming from the newest session that carries it unless
`--intent` gives another. An id no session in the log carries is refused
(exit 3); a log that cannot be read never refuses a launch, and the briefing
says the continuation is unchecked. An Intent may span repositories: one last
worked on in another repository is continued, and that is said on the
terminal and in the briefing ("continued from session oh-…, which worked on
it in <repo>"). The briefing names both ids ("This session is oh-…; its
intent is in-…, new"), the record and the next briefing show it (`intent
fix the parser (intent in-…)`), and `--task` is recorded as given. Older
records have no intent id.

**Validations.** `ohjaa gate` inside a session writes a validation reference
into the session's record before it asks sykli (see `ohjaa gate` below): a
reference, never an outcome. The record and the next briefing list each one
in the observed part, before the handover: its request, kind, commit and
tree (and the tree sykli ran, when it is not exactly the commit's, or was
dirty), then sykli's run id and receipt (path and sha256) as recorded. One
with a run id and no receipt yet is pending: at leaving, when an orphan is
closed, in `ohjaa session` and in every briefing, sykli is asked about it
(`sykli status --run <id> --json`, once per run) and its answer is shown as
sykli's (`sykli says run … is running: 1 running, 1 pending`), never stored.
sykli is asked from the checkout the run was requested in, else from any
checkout of its clone that is left (sykli keeps its runs in the clone's
common dir, which outlives a removed worktree), handed `--repo-root` and
`--git-common-dir` when it takes them. A run that ended in any state but
`completed` (interrupted, lost, indeterminate) is said as not proven and not
failed. A pending validation outlives its session: every briefing in the
clone names those earlier sessions left pending ("Validations earlier
sessions here left pending:"), until sykli says the run ended; one sykli
cannot be asked about is named for a week from its request, then counted. A
sykli without `sykli status --run` is said to be one that cannot be asked.

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
(`receipt: passed`). A task sykli reused from its cache instead of running is
said apart, never folded into passed: `receipt: passed (1 of 3 tasks cached)`,
`receipt: cached (all 2 tasks cached)`, in the record, the briefing's work
line and the marks; a receipt is filed under the tree that ran (a run sykli's
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
the receipt is. What sykli can do is read from the flags `sykli verify --help`
lists, never from its sentences: one that lists `--commit` judges the commit
alone, nothing from the checkout (sykli#67 as merged), and one that also lists
`--json` is asked for its `sykli-verify.v1` verdict, whose trust tiers say what
it established. A pass stands on the receipt's identity against the commit:
its schema, contract, source (the commit's tree and inputs) and the records'
consistency. The outcome is the receipt's claim, as sykli's own trust
statement says (receipts are unsigned), read from the verdict's
`claimed_outcome` (an older sykli without it: its `outcome` check) whether
sykli lists it as established or not:

| sykli says | the record says | flag |
|---|---|---|
| exit 0; with `--json`, a verdict that judged the commit, verified it, established its schema, contract, source and consistency, and that says the receipt claims `passed` or `cached` | `verified: passed` | no |
| exit 0 with `--json`, but a verdict that does not prove it (another schema, another subject, an identity tier not established, no claimed pass) | `unverified: …` | no |
| exit 1 or 3: refused the commit (another tree, a dirty run, an input the commit does not have, a failed outcome, contradictory records) | `FAILED sykli verify: …` | **yes** |
| exit 4: it passed under another contract or lock than the one it is judged by now | `unproven: it passed under another contract than the gate's (…)` | **yes** where it is `origin/<default>`'s tip; otherwise no |
| exit 2: it could not verify (a bad revision, arguments, not a receipt) | `unverified: sykli verify could not verify: …` | no |
| any other end: a crash, a kill, no answer in time | `unverified: …` | no |
| no `--commit` (`sykli verify --help` lists no such flag) | `unverified: sykli has no --commit` | no |
| — the commit has no sykli contract | `unverified: no sykli contract …` | no |
| — no receipt names the commit's tree (the commit has a contract) | `unchecked` | **yes** where it is `origin/<default>`'s tip; otherwise no |
| — no receipt, and the checkout it was made in is gone with its `.sykli/` | `unchecked: … the checkout it was made in is gone` | no: it cannot be observed |

A refusal of the session's own work **flags the record**, and so does the tip
`origin/<default>` was left at when sykli refused it or it was never run where
there is a contract (and the checkout it was made in, where a receipt would be,
still exists): a broken main is the repository's state, whoever pushed it
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
facts, not anyone's words, and the drafter is told so. A session is marked
drafted from once the drafter has answered for each of its candidates, in
however many runs that takes, and only by a run that read every transcript;
until then it stays queued (one set aside keeps it queued, so that a retry
finds it).

Signals and the terminal, while a session runs (unix). The session's process
group is apart from ohjaa's, so ohjaa does with the terminal what a
job-control shell does with a job's, at every moment, not only at the start:

- **While ohjaa's job is in the foreground**, the session's group holds the
  terminal: the agent reads it, and **Ctrl-C** (and Ctrl-backslash) reaches it
  directly. ohjaa does not pass it on again: a second Ctrl-C makes agents like
  `claude` quit instead of interrupt.
- **Ctrl-Z** (or the agent suspending itself) stops the session, then ohjaa's
  job with the same signal, once ohjaa has the terminal back, so the shell
  sees the job stopped. The same goes for a session that reads or writes the
  terminal from the background (`ohjaa run … &`): the shell shows the job
  `suspended (tty input)`. `fg` gives the session the terminal again and
  continues it; `bg` continues it without. A stop ohjaa sees only after `fg`
  has brought its job back (`bg`, then `fg` at once; or ohjaa stopped alone)
  stops nothing: the session gets the terminal, and what is typed next
  reaches the agent, not the shell.
- **SIGINT and SIGQUIT sent to ohjaa** while the session does not hold the
  terminal (no terminal at all, or ohjaa's job not in the foreground) are
  passed to the session's group, each time: nothing else can bring it one.
- **SIGTERM, SIGHUP, SIGUSR1, SIGUSR2** sent to ohjaa are survived and passed
  to the session's group once each, with a SIGCONT, so a stopped session can
  act on them: Ctrl-Z, then `kill %1`, ends it at once. To stop a session from
  outside, send SIGTERM to ohjaa, or SIGINT to the session's process group
  (its record's `pgid`).
- **Under a wrapper without job control** (`sh -c 'ohjaa run …'`) at a
  terminal, nobody would give the terminal back should ohjaa be killed while
  the session held it, so the session shares ohjaa's process group, as it
  always did, and the terminal is not handed over.
- **After the session ends**, the terminal is ohjaa's group's again, and any
  of these signals cuts leaving short (the tools still running are killed)
  and the record is still written, with what was known by then; the normal
  dispositions come back once it is saved.
- **Before printing the exit record**, ohjaa restores the terminal settings
  saved before the session first got the foreground, and disables mouse,
  focus and extended-keyboard reporting left behind by an agent that did not
  clean up. This also runs when the session was killed but ohjaa survived.
  Recovery uses the controlling terminal, not redirected output, and never
  clears scrollback, discards input or changes a terminal another foreground
  job now owns.
- **SIGKILL** cannot be caught: it loses the leaving. The session's group
  outlives it, and the next launch says whether it still runs (above).

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
Each session is its record as the log holds it; what later versions added is
additive and absent from older records: `intent_id` and `task`, `pgid` and
`group_started_ms` (the session's process group), and `validations` (see
`ohjaa gate`, and the integrated validations of "The integrated State",
with `not_dispatched`). A log an ohjaa with `ohjaa judge` wrote (removed
2026-10-26) still holds that command's tables, `judgements` and
`judgement_responses`; it opens and is written as any other, and those
tables are never read and left as they were. Unreadable records produce exit 4. `--session ID` selects one record. Existing
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
- The briefing's first line is always `<!-- ohjaa-briefing v1 -->`. It says
  where ohjaa's words begin, so a reader of transcripts can tell a launch's
  task from the briefing toimija appends after it (vessa cuts there). It is
  matched byte for byte: a change is a new version, never an edit.
- `--dry-run` prints the exact command it would exec, without launching.
- If toimija cannot fully read the sessions, or does not report its coverage,
  ohjaa refuses to launch blind.
- Sessions are counted across **every worktree of the clone**, not only the
  checkout a session is launched in: toimija keeps each worktree's apart, so
  ohjaa asks it in each one the clone's layout names (side by side; never in
  ohjaa's own validation checkouts), says them on stderr (`ohjaa: 1 more
  session(s) in other worktrees of this clone at launch: 1790-42 (codex,
  widget-lexer)`), and names them in the briefing ("Sessions in the other
  worktrees of this clone at launch"). One that cannot be read is said, never
  shown as none; it does not refuse the launch. At leaving, what was made in
  another worktree while one of them ran there is not attributed.

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

**Observed, not asserted (law 3).** One rule: in a repository the session was
launched in or named with `--scope`, a commit made during the session is its
own unless another session was there at that second — in the checkout it was
launched in, a session launched in that checkout; in any other checkout or
branch of the clone, old or new, any session in the clone (sessions in the log,
and live ones toimija saw at launch). Only its own commits are its phases, its
pull request and its lessons. Everything else made during the session is
recorded as `observed … not attributed`, with the session it shared the place
with (`shared with oh-… in work, …`). Known limit: a person's own commit in a
place the session holds alone looks exactly like the agent's, and is credited
to the session.

A repository nobody named is observed when a commit was made in it: at leaving,
ohjaa looks at the repositories directly in the launch folder (and in any folder
`--scope` named that is not itself a repository), and records each where a
commit was made, or pushed from, during the session, in any of its worktrees,
as **found, not named**. Nothing in a found repository is attributed to the
session: it is not its phases or lessons, no pull request is looked up, and the
session never becomes that repository's "last session here" (a broken main it
saw there is still said, as the repository's state). Name repositories with
`--scope` wherever you can. A `--scope` path that is neither in a repository
nor a folder is named as unobserved. Scope can be named from inside a repo too, for
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

## The workflow home: applied from outside the repos

The agent workflow (the gate, the hooks, the launcher) is a workflow thing,
not something to merge into product repositories. Ohjaa keeps it in its home
and applies it to repositories from outside; nothing is installed on a
repository or written into one. Only repositories whose remote names an entry
run ohjaa's hooks, and only the hooks they need: `git status`, `commit` and
`fetch` start no ohjaa at all unless the repository has a hook of its own
there, and a Homebrew tap, Cargo's git cache or a personal project never
starts it.

```
~/.config/ohjaa/                    ($XDG_CONFIG_HOME/ohjaa)
  repos/<owner>/<repo>/             lower case
    repo.toml     branch = "main"   the branch ohjaa observes, and [cargo_env]: what each
                                    cargo task of its gate is given (KATANA D10)
    sykli.json    a stored gate, only for a repository with no sykli.json of its own
    sykli.lock
    hooks/pre-push, hooks/<name>    exec '<ohjaa>' hook <name> "$@": pre-push, and each
                                    hook this entry's clones have of their own
    gated.config                    core.hooksPath = that hooks/ folder
  git/config                        includeIf "hasconfig:remote.*.url:<pattern>" per entry
  roots                             where apply and doctor look (from the first apply's --root)
  applied.toml                      what apply changed outside the home, for --undo
  mine/ machine/ ours drafter       (knowledge and settings, as before)
~/.gitconfig                        [include] path = ~/.config/ohjaa/git/config (once)
~/.config/git/ignore                git's global excludes (or core.excludesFile):
                                    a block marked "ohjaa apply" holds .sykli/ and .teko/
```

```bash
ohjaa apply --root ~/projects --dry-run # the first time: name where the repositories are
ohjaa apply --root ~/projects          # the home's files, the include, the ignore block, ~/.zshrc
ohjaa apply                            # later runs read the roots it wrote down
ohjaa apply --undo                     # put back what apply changed; entries and roots stay
ohjaa apply --prune                    # also remove dispatchers no clone needs any more
ohjaa adopt ~/projects/taso            # entry: origin's default branch, and [cargo_env]
ohjaa gate                             # prove HEAD: run this repo's gate, as a push is judged
ohjaa gate --print                     # the contract and the commands, run nothing
ohjaa doctor                           # read-only; exits 4 on drift
```

**Scoped by remote.** Git's global config gets one line, an `include` of
`git/config`. That file has, for each entry, `includeIf
"hasconfig:remote.*.url:…"` sections (git 2.36+) that include the entry's own
`gated.config`, which sets `core.hooksPath` to the entry's own `hooks/`. The
patterns are host-agnostic and case-insensitive (`**/o/r`, `**/o/r.git`,
`**/o/r/`, `**/o/r.git/`, and the same after `*:` for scp-like URLs, plus a
short form for each global `insteadOf` rule), because a pattern that misses
leaves a push unobserved, while one that matches too much only costs a
process that finds no entry. Any remote of a clone counts. A `pushurl`, a
repository-local `insteadOf`, or the repository's own `core.hooksPath`
(perusta, selko and vartio set one) is not seen by the patterns: `ohjaa doctor`
checks, clone by clone, that git really resolves `core.hooksPath` to the
entry's hooks, and `ohjaa run` warns at launch in such a repository. The
generated files are written atomically and checked with `git config --file
… --list` first, since an unreadable one would break every git command.

**The dispatcher** (`ohjaa hook <name>`) finds the repository by the URL git
pushes to. On pre-push, with an entry, a push that moves the entry's branch
is observed: it is handed, quietly, to `sykli hook pre-push --contract
<the pushed commit's gate> --branch <entry's branch> <remote> <url>`. Nothing is
refused for it (see "Observe, mark, flag"). Then, for every hook, the
dispatcher runs the repository's own hook, `<common dir>/hooks/<name>`, with
the same arguments and the same stdin, and exits with its status: a
repository's own hook still refuses what it refuses. `push-to-checkout`, `proc-receive` and
`fsmonitor-watchman` never get a dispatcher, because a file of that name
changes what git does. The dispatchers exec ohjaa, and the pre-push one hands
the push to sykli, by the paths `apply` found on PATH, so a git client started
without your PATH (a GUI) runs them too.

**Few git processes.** Facts are worked out once and passed down. A hook that
only chains starts no git: git runs it from the work tree's root, and the
common directory comes from the repository's layout. What ohjaa reads of a
clone's commits (a commit, its tree, its own `sykli.json` and `sykli.lock`)
comes from one `git cat-file --batch` per clone, asked one object at a time
and ended when the command is done with it (a launch's, before the session
runs), never fetching. pre-push starts that one: the pushed commit's gate,
and, when sykli does not accept the push and the clone holds receipts, its
tree, for one `sykli verify`; it hands sykli `--repo-root` and
`--git-common-dir` when sykli takes them.
`ohjaa gate` starts one (the repository's config; its gate is derived from the
checkout's files), and inside a session that one more (HEAD, its tree and the
branch tip, for the request); asking sykli about a pending run starts none;
`doctor` two for the machine (`git --version`, the global config with
includes) and at most two per clone (its config, with `insteadOf` applied in
Rust, and for an entry's clone the tip's gate);
the `sykli validate` it asks of a stored gate's lock, and the `sykli validate`
and `sykli lock` of adopt, are handed the clone's facts and start none.
The briefing's flag costs at most three (the clone's config, which the drift
warnings share, and the clone's `git cat-file --batch`, which walks the branch
and reads the gate of each commit sykli is asked about) and one `sykli
verify`; the report's marks, and `report --json --marks`, two per entry (its
clone's config and the walk), and `sykli verify` only for a commit with a
receipt of its tree that says it passed. The briefing's validation lines start
no git (HEAD is read from git's files, the gates of the tip and HEAD through
the walk's process) and ask `sykli status --commit` about the tip
and HEAD; a launch that asks for an integrated validation starts at most three
more for the clone's validation checkout (made: `worktree add` and `status`;
reused: `checkout -f`, `clean` and `status`; two more where the commit has
submodules, `submodule deinit` and `submodule foreach`; and it all again, made
from nothing, when the reset or its check fails). A pre-push of any other branch than the
observed one reads the session log and the clone's ref as files, for a base
that moved: no process. `tests/config_home.rs` holds these budgets.

**`ohjaa gate`** proves HEAD: it runs the gate a push is judged by, from the
repository root: `mkdir -p .sykli && sykli run <contract> --json >
.sykli/receipt.json && sykli verify .sykli/receipt.json --contract <contract>
--commit HEAD`, where the contract is the gate of what the checkout runs (see
"The gate of a commit"): derived from the checkout's own `sykli.json` and
`sykli.lock`, read as files, else the entry's stored gate; or the
repository's own `sykli.json` when origin names no entry. When those two
files are HEAD's, that is exactly HEAD's gate; when they are edited (an input
added, as one must before committing it), it is the gate the commit they
will be in has, so the receipt proves that commit (a receipt names the
contract it ran under, and every commit is judged by the gate derived from
it). Where git converts line endings on checkout, the checkout's bytes can
differ from the commit's, and such a receipt reads as another contract's:
unproven, never a false pass. It prints the contract, where it comes from, and every command, and
exits with sykli's status; `--print` runs nothing and writes nothing (it names
where the run keeps the gate, and says when it is not written yet). The
receipt stays in that worktree
(`.sykli/receipt.json`, and the copy `sykli run` writes under
`.sykli/receipts/`), where the push and the marks find it; sykli has no
command that keeps a receipt anywhere else, but `sykli hook pre-push` copies
the one that proved a push into `<git common dir>/sykli/receipts/`, which
outlives the worktree. Agents are taught this one command.

Inside an `ohjaa run` session (`OHJAA_SESSION`), the gate first writes the
request into the session's record (`validations`, in `ohjaa-sessions.v1`),
before sykli is asked, and says so; outside one it says the request is not
recorded. A `ValidationRef` holds:

| field | what | from |
|---|---|---|
| `request` | ohjaa's id for the request, `rq-<ms>-<pid>` | ohjaa, at once |
| `repo`, `commit`, `tree` | the checkout, HEAD and its tree | git, at once |
| `common` | the clone's git common dir, where sykli keeps its runs | the layout, at once |
| `observed_branch` | `{name, tip, updated_ms}`: the entry's branch, where `origin/<branch>` was in this clone, and when it last moved here (its reflog): never a statement about the remote now | git and the reflog, at once |
| `contract`, `contract_digest` | the gate, and the hash its `sykli.lock` pins (else the one sykli reports) | the lock, at once |
| `requested_ms` | when | ohjaa, at once |
| `run` | sykli's run id, once sykli gives one | sykli |
| `ran` | `{tree, dirty}`: what sykli says ran (a run of uncommitted changes runs another tree, dirty) | sykli |
| `kind` | `candidate`; `integrated` only once sykli says the run was of exactly the commit's tree, nothing dirty beside it, and the commit was `observed_branch.tip`; a validation ohjaa asks for of a landing is `integrated` from the start (a checkout of exactly that commit; see "The integrated State") | from the above |
| `not_dispatched` | why a recorded request was never handed to sykli (its checkout could not be made or verified clean, a sykli without `--detach`, or the ohjaa that recorded it stopped first: "requested, never dispatched (ohjaa stopped before asking sykli); asked again as request …"); absent once it was. An integrated request with neither `run` nor `not_dispatched` is one whose ohjaa stopped between recording it and handing it over, and is said so | ohjaa |
| `lock_digest` | the lock's sha256, as sykli reports it | sykli |
| `receipt` | `{path, digest}`: the receipt sykli keeps of the run (`.sykli/receipts/rcpt_<sha256>.json`, digest as sykli names it), else `.sykli/receipt.json` without a digest; for an integrated validation, ohjaa's copy of it in its state (`integrated/<clone>/receipts/<request>.json`), made before the validation checkout is reset for another run, with sykli's digest | sykli |

Never an outcome: no state, verdict or check is stored. With a sykli that has
durable runs (KATANA D3, detected from `sykli run --help` naming `--detach`
and `sykli status --help` naming `--run`), the gate hands the run over
detached, `sykli run <contract> --detach --json`, which answers `{schema:
"sykli-run.v1", run}` once sykli has recorded the run; the run id goes into the
record at once, then the gate asks `sykli status --run <id> --json`
(`sykli-run-status.v1`, handed `--repo-root` and `--git-common-dir` when sykli
takes them) until the run ends, says sykli's answer, records what ran and the
receipt it names (a relative path is taken from the checkout), saves it as
`.sykli/receipt.json` and verifies it as above. It waits at most `--wait`
seconds (30 minutes by default). A run still going then, or one sykli says
ended in any state but `completed` (interrupted, lost, indeterminate), is not
proven and not failed: the gate says so and exits 75, and the run stays
recorded for whoever asks next. Only a completed run whose receipt does not
pass is a failure. Should the gate or its session be stopped meanwhile, the
run goes on in sykli. With any other sykli the gate runs in the foreground,
as always, and fills in the run id only if the receipt names one. The D3
shapes are read as KATANA's DECISIONS.md writes them; the tests drive them
with a stub, and a real sykli with durable runs is not released yet.

### The gate of a commit

A copy of a repository's contract drifts the moment the repository changes it.
The audit of 2026-10-26 found sykli's copy, made on 10-14, making `sykli run`
refuse on main, and 18 of sykli's last 50 commits, passed under its own
`sykli.json` (CI included), marked as another contract's. So an entry keeps no
copy of a contract its repository has. The gate of commit C is derived from C:

- the repository's `sykli.json` at C, and the `sykli.lock` beside it, read
  from the commit itself through the clone's one `git cat-file --batch`
  (never a work tree, which another session may be editing; only `ohjaa
  gate`, which runs its checkout, derives from the checkout's files);
- with the entry's `[cargo_env]` given to each cargo task that does not say
  where it builds (KATANA D10, below). When no task is given a line, the gate
  is that contract and lock as they are, byte for byte;
- written as files, outside the repository and never in the config home, for
  sykli reads a contract and the `sykli.lock` beside it from files. A gate a
  run that outlives the ohjaa that started it names (`ohjaa gate`, an
  integrated validation) is kept in ohjaa's state, keyed by the SHA-256 of
  the gate and its lock: `$XDG_STATE_HOME/ohjaa/derived/<digest>/sykli.json`
  and `sykli.lock`, beside the clones' validation folders. Every other gate
  sykli only reads (a receipt to verify, a push to judge, what applies to a
  commit) is read from there when a run kept it, else written into a folder
  of that ohjaa's own under `$TMPDIR`, removed when it is done: the marks, the
  report and `ohjaa gate --print` write nothing that stays, and work with a
  config home that cannot be written. A gate given lines is written as sykli
  canonicalises a contract (keys sorted, no blanks), so its file's SHA-256 is
  the hash sykli pins;
- locked (a `sykli.lock` beside it pinning it) only when the repository's own
  lock pins its contract at C. A lock that does not (the contract changed
  without `sykli lock`) never pins the gate: a gate given lines gets a lock
  pinning the repository's own contract, which the gate is not, and a gate
  given none keeps the repository's lock as it is, so sykli refuses the gate
  as it refuses the repository's own. A repository with no lock gets none.
  What the repository did not lock, ohjaa does not lock.
- read by the entries of C's top tree: a `sykli.json` (or `sykli.lock`) that
  is a symbolic link or no file, or one a partial clone lacks (ohjaa never
  fetches), is said, never read as no contract.

It is a function of C and the entry alone: the same C always gives the same
gate, and a contract changed at C gives another, in another folder, with no
adopt in between. `ohjaa gate`, the pre-push hook, the marks, the flags a
session's record raises at leaving and the integrated validation all judge C
by it, and it is the same bytes wherever and whenever it is written.

A commit with no `sykli.json` of its own is judged by the entry's stored gate,
`sykli.json` beside `repo.toml`, which adopt has `sykli init --contract` write
for a repository without one (ohjaa, vessa and faija today), and which gets
its lines when it is written. An entry adopted before keeps working: the copy
it holds is ignored for every commit that has its own contract, and `ohjaa
adopt --force` removes it. A commit from before its repository had a
`sykli.json` (one with only `sykli.rs`, as toimija's first ones) is then
judged by nothing: it is marked unproven (`could_not_say`, "no sykli contract
(sykli.json) in …"), never failed.

**A pass under the repository's own contract proves C too.** A run of C's own
`sykli.json` and lock, as they are (CI's `sykli run sykli.json`, or an
agent's), ran the same tasks and inputs as the gate, without the `[cargo_env]`
lines. Its receipt names that contract's hash (and that lock's digest; a
receipt from an older sykli names none), so ohjaa verifies it against them,
read from the commit: `sykli verify <receipt> --contract sykli.json --commit
<C>`, run in the clone, which checks the lock. A lock digest a receipt does
name has to be C's lock's. Every other receipt is
verified against the gate. One under any other contract (a copy from before,
an older gate) stays `other_contract`. The mark says which contract proved it
(below).

### Observe, mark, flag

Nothing is refused for the workflow's sake. A push is observed, each commit on
an entry's branch is marked, and what is not proven is flagged to the people
and agents who build on it. Only a repository's own hooks refuse.

**Observe.** A push that moves the entry's branch is handed to `sykli hook
pre-push` against the pushed commit's gate, with its output kept back, for at most
15 seconds. When sykli accepts it, nothing is said. sykli looks for a receipt
only in the worktree that pushes, so when it does not accept the push, the
commit is asked about as the marks ask (every worktree's receipts, its tree
from the git process that read its gate, one `sykli verify`, by the sykli the
dispatcher names):
a commit proven in another worktree is a quiet push too. Otherwise (no
receipt of the commit's tree, a failed one, one under another contract), or
when sykli cannot judge it (no sykli, one without `--contract` and
`--branch`, an entry without a gate, sykli out of time), the push goes on,
and one line says so, after the repository's own hook:

```
ohjaa: UNPROVEN push to main (no receipt in this worktree is a run of its tree). It is pushed and flagged; prove it with: ohjaa gate
```

When ohjaa cannot observe the push at all (no workflow home, an entry it
cannot read), the push goes on with `ohjaa: UNOBSERVED push to <owner/repo>:
<why>`.

**Mark.** Each commit on the entry's branch is, derived now and stored never:
- ✓ **proven**: sykli accepted a receipt of its tree against the commit's
  gate (`sykli verify <receipt> --contract <its gate> --commit <sha>` exits
  0), or, for a run of the commit's own `sykli.json`, against that (`--contract
  sykli.json --commit <sha>`);
- ✗ **failed**: the newest run of its tree here failed (its receipt says so,
  or sykli refused it for its outcome or evidence);
- ? **unproven**: no receipt of its tree here; or it passed under another
  contract or lock than the commit's gate and its own (a copy kept from
  before, a stored gate edited and locked again since); or sykli could not
  say, or no gate judges it (no `sykli.json` of its own, and no stored gate).

✓ means exactly that sykli accepted a receipt for this tree against the
commit's gate or its own contract, and says which: `(own sykli.json)` after
the commit in `ohjaa report`, `contract` in `ohjaa-marks.v1`. sykli verified
the receipt's identity against this commit;
the outcome is the receipt's claim. Receipts are unsigned files that any
local session can write, and `sykli verify` checks a receipt against the
commit's tree, its contract and its lock, not by running the tree again: a
forged receipt can make a commit look proven (one cheap forgery, editing
`tree_oid`, is filed on sykli). A mark is evidence from this machine's
sessions, not a CI result.

The walk starts where the clone last saw origin (`refs/remotes/origin/
<branch>`: no fetch, no network), and goes back along first parents to the
first proven commit, at most 50, tip first. The commits, and the gate of each
one sykli is asked about, come from the clone's one `git cat-file --batch`
(first parent by first parent; a shallow clone ends where its history does);
receipts are read as files, from the clone's layout:
`<common>/sykli/receipts/` (what sykli kept for accepted pushes) and, in
every worktree, `.sykli/receipts/` and `.sykli/receipt.json`. A forge's merge
of an up-to-date pull request has the tree of its head, so a receipt of the
head proves the merge. The marks are as of the last update of `origin/<branch>`
here, which its reflog dates and says the kind of (a fetch, or a push from
this clone). `ohjaa report` shows the marks of every entry, and `ohjaa report
--marks` only them:

```
Marks on each entry's branch, tip first (✓ proven · ✗ failed · ? unproven):
- false-systems/taso main (origin/main as of a fetch at 2026-10-13 09:12 UTC): ? 4be01c2 ✗ 77a1f0e ✓ 0c9d3b1
```

**Flag.** When the entry's branch is not proven at its tip, the briefing of
`ohjaa run` (after its sentinel line) and of `ohjaa brief` opens with it,
before anything else, and the launch says it on stderr:

```
FLAG: main of false-systems/taso is unproven since 77a1f0e (2 commits: ? 4be01c2, ✗ 77a1f0e; origin/main as of a fetch at 2026-10-13 09:12 UTC). Start by running `ohjaa gate` on main and fix forward what fails.
```

When some of those commits passed under another contract, it says the branch
is "not proven against its current gate" (or "has never been proven against
its current gate"), and how many passed under another one.

The flags a session's record raises at leaving (a broken `origin/main`, its
own refused work) judge a repository with an entry by its gate too.

### The integrated State, candidates, and a base that moved

A candidate's pass proves the State that lands only when the trees are the
same: sykli binds a receipt to a tree. A fast-forward lands the candidate's
own tree; a merge onto a branch that moved on lands a State no run has seen.
ohjaa never carries a result across trees. It asks sykli, with a sykli that
has `sykli status --commit` (KATANA D3; detected from `sykli status --help`),
and says five states apart, each as sykli's word:

| state | what sykli says |
|---|---|
| candidate passed / failed | a current receipt of HEAD's tree (HEAD not the observed tip) passed / failed |
| integrated passed / failed | a current receipt of the observed branch's tip's tree passed / failed |
| integrated unverified | no current receipt of the tip's tree (none at all, a run pending, results that disagree, or sykli could not say) |

(A candidate with no current receipt is `candidate unverified`.)

**Asking for it.** When a landing reaches the observed branch, ohjaa asks
sykli to validate the integrated State, once sykli has no current receipt of
its tree under the gate (`sykli status --commit <tip> --contract <gate>
--json` says `none`) and no request of it stands:

- **at leaving**, for each commit the session saw land on `origin/<branch>`
  that the branch still sits at;
- **at launch**, in the briefing, for the tip the marks walk found (`ohjaa
  run` only: `ohjaa brief` asks for nothing, and `--dry-run` says it would).

One ohjaa at a time decides and asks, per clone: it takes the clone's lock
(`$XDG_STATE_HOME/ohjaa/integrated/<clone>/lock`, flock(2), so a crash never
leaves it held; a launch waits at most 20 s for another, then asks for
nothing and says so) and reads the log again under it. Two launches at once
in two worktrees of one clone ask sykli once; the other says "ohjaa asked
sykli to validate it at …". A request that **stands** is not repeated: one
handed to sykli (a run id) or one that could not be (`not_dispatched`), in
the last ten minutes. A request recorded and **never dispatched** (its ohjaa
stopped between recording it and handing it over: neither a run id nor
`not_dispatched`) is said as "requested, never dispatched (ohjaa stopped
before asking sykli)", never as asked, and is asked again at the next launch
at once, however old; the old request says it was replaced.

The request is a `ValidationRef` of kind `integrated`, written into the
session's record **before** sykli is asked (invariant A; at launch, with the
entering record). Then ohjaa hands sykli the run in the clone's **one
rolling validation checkout**, `$XDG_STATE_HOME/ohjaa/integrated/<clone>/
checkout`, a linked worktree of the clone (git keeps its admin folder in
`<common>/worktrees/`; no file, hook or config of the repository is
written). Before every run it is:

1. **left alone while a run there may still be going**: the newest run
   handed over in it is asked about (`sykli status --run`). The checkout is
   **busy**, nothing is reset under it, no request is recorded, and the line
   says "the validation checkout is busy with sykli run … so none is asked
   for now" (a later launch asks), whenever sykli says the run is requested
   or running; or any of its tasks is still running, or any attempt has not
   finished (a run sykli calls `lost`, its coordinator gone, may still have
   one); or any attempt's process group still has a member (a task that left
   a process behind); or sykli cannot say what became of the run, or cannot
   be asked. A run that is gone for good keeps it busy until a person removes
   the checkout folder (the line says which); the next launch then makes it
   afresh;
2. **its last run's receipt kept**: when that run has ended, the receipt it
   names (in the checkout's `.sykli/receipts/`) is copied into
   `integrated/<clone>/receipts/<request>.json` and recorded in that
   request's `receipt`, with sykli's digest, so no evidence lives only in a
   checkout;
3. **reset to exactly the landed commit**: `git -c core.hooksPath=/dev/null
   checkout -q -f --detach <commit>`; where the commit has submodules,
   `submodule deinit -q -f --all`, so each is as a fresh checkout has it (not
   populated: its work tree, ignored files and changes go; nothing is
   fetched, and the gate initialises what it needs); then `clean -q -ffdx -e
   /.sykli` (sykli's own folder at the top stays, everything else another
   run left goes: build folders, untracked and changed files, and a `.sykli`
   folder deeper down);
4. **verified**: HEAD is the commit; `git status --porcelain=v1
   --untracked-files=all --ignored --ignore-submodules=none -- .
   ':(exclude).sykli'` says nothing; and, with submodules, `submodule foreach
   --recursive` finds none populated. When the reset or the check fails, the
   checkout is made again from nothing (`worktree remove --force`, then
   `worktree add --detach --force`) and verified again: a leftover never
   blocks validation for good. Only when that fails too is the request
   recorded as not dispatched, and why.

Then `sykli run <its gate> --detach --json` there (the gate derived from the
landed commit, recorded as the request's `contract`). Its run id is filled in, or,
when the run could not be handed over, why (`not_dispatched`). Because the
checkout is the clone's, sykli's run and receipt are the clone's: `sykli
status --commit` finds them from any checkout, and so do the marks. One
checkout per clone, however many landings: nothing piles up.

**Cargo's target dir.** `~/.cargo/config.toml` may point every checkout at
one shared `build.target-dir`, and sykli hands a task only PATH, HOME and
TMPDIR, so cargo would build there, and take another tree's newer artifacts
for fresh. ohjaa writes `integrated/<clone>/.cargo/config.toml` (`[build]
target-dir = "<that folder>/target"`) above the checkout: cargo reads the
config of every folder above the one it runs in, and a deeper one wins over
`$CARGO_HOME/config.toml`, so this checkout's builds, and only its, go to
`integrated/<clone>/target`. Nothing is written into the repository or the
checkout. A repository's own `.cargo/config.toml` still wins, as it would
anywhere.

(sykli keeps a detached run's journal in the clone's common dir, and its
receipt in the checkout it ran in; keeping that receipt beside the journal
is a sykli change, not made here.) Leaving never reads the validation
checkout's reflog (nothing is made in it), and it is not counted as a
sibling checkout. A sykli without `status --commit` or `run --detach` is
asked for nothing, and the line says so.

The briefing says it after the flag (and the launch on stderr):

```
Validation here (sykli's word, asked at launch; integrated: what is on the observed branch, candidate: HEAD on its way there):
- integrated unverified: main of acme/widget at 4be01c2: sykli has no current receipt of its tree; ohjaa asks sykli to validate it: request rq-…, recorded in this session before sykli is asked
- candidate passed: HEAD 9c0d1e2 (feat/parser): a current sykli receipt of its tree passed
- base moved since validation: HEAD 9c0d1e2 was validated when origin/main was at 0c9d3b1; it is at 4be01c2 now: its merge is a State no run has seen (flagged, never refused)
```

**A base that moved.** A `ValidationRef` records where `origin/<branch>` was
when the candidate was validated. When HEAD's newest validation saw it
elsewhere than it is now, the briefing says "base moved since validation",
and so does a push of that commit to any other branch than the observed one
(read from files only: the log, read-only and never waited on, and the
clone's ref; a log a writer holds at that moment is not read, and nothing is
said):

```
ohjaa: base moved since validation: 9c0d1e2 (feat/parser) was validated when origin/main was at 0c9d3b1; it is at 4be01c2 now: its merge is a State no run has seen (flagged, never refused). It is pushed; validate the integrated State once it lands.
```

Flagged, never refused (observe-mark-flag). A push to the observed branch
itself is a fast-forward of the commit sykli's hook judges, so its own tree
is the State that lands.

### `ohjaa report --json`: `ohjaa-marks.v1`

One machine-readable view for other tools. Computing it writes nothing (a
gate sykli verifies against goes into a folder of the report's own under
`$TMPDIR`, removed before it exits): not
the session log (opened read-only), not the report's bookmarks
(`report-at`, `report-shown-at`), even at a terminal. Exits 4 when the
report has gaps, as the text does.

The full report reads every repository's default-branch window and asks vessa
for its header listing (about half a second; at most 15 s): on a root of
about 50 repositories, some 500 git processes.
`ohjaa report --json --marks` is the view to read often: the same document
with `report` null, from two git processes per entry (its clone's config and
the walk) and a `sykli verify` only for a commit whose receipt says it passed;
no window, no vessa.

```
{
  "schema": "ohjaa-marks.v1",
  "generated": "<RFC 3339>",
  "complete": true,                      // false: something could not be read
  "entries": [{                          // every entry in the home
    "repo": "owner/name",
    "branch": "main", "ref": "origin/main",
    "tip": "<sha>" | null,               // where origin/main is here
    "clone": "<the clone walked>",       // the one whose origin/main moved last
    "updated": "<RFC 3339>" | null,      // when origin/main last moved here
    "updated_by": "fetch" | "push" | null,
    "ended": "proven" | "root" | "cap" | "not_asked",
    "proven_at_tip": false,
    "commits": [{                        // tip first, down to the first proven one, at most 50
      "sha": "…", "tree": "…",
      "mark": "proven" | "failed" | "unproven",
      "code": "proven" | "failed" | "other_contract" | "no_receipt"
            | "no_receipt_checkout_gone" | "could_not_say" | "stale",
      "why": "…" | null,                 // for a person; null when proven
      "author": "…", "authored": "<RFC 3339>", "subject": "…",
      "contract": "derived" | "own" | "stored" | null, // a proven commit: the contract its
                                         // receipt ran under: the commit's gate, its own
                                         // sykli.json, or the entry's stored gate
      "tasks": 3 | null, "cached": 1 | null // a proven commit: the accepted receipt's tasks, and
                                         // how many it reused from sykli's cache
    }],
    "error": null | "why it has no marks" // then the rest are null and commits is []
  }],
  "proposals": {                         // waiting for a person's decision
    "listed": true,                      // false, with "why", when they could not be read
    "waiting": [{ "scope": "ours", "name": "…", "title": "<first line>",
                  "since": "YYYY-MM-DD", "repo": "<path>" | null }]
  },
  "sessions": [{                         // still open, or closed as orphaned in the window
    "id": "…", "agent": "…", "repo": "…", "intent": "…",
    "started": "<RFC 3339>",
    "state": "open"                      // its ohjaa runs, or that cannot be told
           | "ohjaa_gone"                // its ohjaa is gone, its process group runs on
           | "orphaned"                  // closed as orphaned, or ohjaa and group both gone now
  }],
  "report": { "schema": "ohjaa-report.v1", … } | null  // the window's lists; null with --marks
}
```

The keys `ohjaa-marks.v1` had when viemäri was built on it (2026-10-13)
never go; `integrated` (per entry and per commit) and, on 2026-10-26, the
commits' `contract` were added since, so a
reader of the earlier shape keeps working. `judgements` was added on
2026-10-14 (#46) and removed with `ohjaa judge` on 2026-10-26: nothing ever
read it and no Judgement was ever recorded, so the schema kept its name.
From now on, `ohjaa-marks.v1` only gains keys.

`ended` says why the walk stopped: at a proven commit, at the branch's first
commit, after 50, or before a commit only sykli could judge with no `sykli
verify` left to ask. `src/report.rs` pins this shape in a test.

**`apply`** runs as the `ohjaa` on PATH (anything else is refused unless
`--bin` names it, so a `cargo run` cannot re-point every hook). It refuses,
before changing anything, when git's global `core.hooksPath` is set (in the
global file or a file it includes): it would run in every repository and
override the scoped one; `--force` removes it from the global file. It does
not guess where the repositories are: with no `roots` file it refuses until
`--root <dir>` names the folder that holds them, writes that down in `roots`,
so what it finds never depends on where it runs, and it only ever adds
dispatchers (a spare
costs a process, a missing one silently skips a check); `--prune` removes
spares. Each change is recorded before it is made, so `--undo` can put back
exactly what was there even after an apply that failed. The launcher line it
puts in `~/.zshrc`, in place of the old `source ~/projects/ohjaa-launch.zsh`,
is guarded like that one was: `[[ -x <ohjaa> ]] && eval "$(<ohjaa>
shell-init zsh)"`.

**`adopt`** writes the entry: `repo.toml`, with the branch (origin's default
one, else `--branch`) and `[cargo_env]` (below). It keeps no copy of a
contract the repository has at that branch (not the working tree, which
another session may be editing): every commit's gate is derived from the
commit, and a copy an earlier adopt kept is removed. For a repository with no
`sykli.json` of its own, it asks `sykli init --contract <entry>/sykli.json`
for a stored gate. Then it regenerates `git/` and writes the entry's
dispatchers. It never touches git's global config, and takes no lock in the
repository.

**One build directory per worktree (KATANA D10).** A shared Cargo target dir
(`~/.cargo/config.toml`'s `build.target-dir`, or a build-dir) lets a gate in
one worktree run a binary another worktree built, and a receipt then claims a
pass for code that never ran. So each task of the gate whose command runs
cargo (a word of it is `cargo`) is given these, in its `env`, as sykli's
README recommends, one for each of the two places cargo builds in (the target
dir, and the build dir that holds its intermediate artifacts, test binaries
among them). They are the entry's `[cargo_env]`, which adopt writes into
`repo.toml`, and a derived gate is given them when it is derived; an entry
written before has none, and gets these:

```toml
[cargo_env]
CARGO_TARGET_DIR = "${worktree_cache}/cargo-target"
CARGO_BUILD_BUILD_DIR = "${worktree_cache}/cargo-build"
```

A task that explicitly chooses its target directory gets **neither** line:
Cargo's build directory defaults under that target, so its layout stays intact.
This includes `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` in `env` or
`inherit`, a certain assignment on every direct Cargo invocation, or
`--target-dir` / `--config build.target-dir=…` on every invocation.
A task that chooses only `CARGO_BUILD_BUILD_DIR` (including
`--config build.build-dir=…`) gets only the target-directory line.

Command detection is deliberately conservative, not a shell interpreter.
A leading unconditional `export NAME=value`, separated by semicolons or
newlines and followed by direct Cargo commands, counts. Conditional exports,
subshells, comments, heredocs, unsets, wrappers and dynamically evaluated
commands do not establish that every Cargo invocation receives the setting.
Uncertain cases get the defaults; a command's own assignment still overrides
them when it actually runs. An assignment before one Cargo invocation says
nothing about the next. Explicit `env` and `inherit` remain the way to declare
settings for complex commands. Tasks with no Cargo word are untouched.
Only the gate changes, never the repository's contract or an agent's own
Cargo commands. A derived gate is locked as "The gate of a commit" says.

A stored gate that came with a `sykli.lock` is locked again (`sykli lock
<entry>/sykli.json`), and only when sykli validates the contract and lock written, which
adopt asks first (`sykli validate <entry>/sykli.json`). Both run in the
repository, handed `--repo-root` and `--git-common-dir` when sykli takes them,
so they start no git. If validation fails, adopt keeps the gate as written
and prints sykli's diagnostic
without guessing the cause from its wording. Validate and lock the contract
in the repository, land both on its observed branch, then adopt again.
A successful validation is reported as `sykli validate accepts its sykli.lock`,
not as an independent claim that Ohjaa verified every lock entry.

The lines are given only when every sykli that runs the gate expands the
placeholder: the one on PATH and the one the pre-push dispatcher runs (as
`apply` recorded it). A sykli that does names `${worktree_cache}` in `sykli
run --help`, matched as that token, never as a sentence. An older one passes
it on as written, and cargo would build in a `${worktree_cache}/` folder
inside the work tree, so then adopt writes an empty `[cargo_env]`, saying why
in `repo.toml`, and says so. When `sykli lock` fails, a stored gate is kept as
written, with a warning.

`ohjaa doctor` says, for each entry with a clone under the roots, which
contract judges its branch's tip: the gate derived from the tip's own
`sykli.json` (and the tasks given lines), or the stored gate. For a derived
one it flags (drift) a tip whose `sykli.lock` does not pin its `sykli.json`
(sykli refuses the gate, as it refuses the repository's own); an empty
`[cargo_env]` while every sykli knows the placeholder and the gate has cargo
tasks ("gate runs may share a target dir across worktrees"; `ohjaa adopt
--force` writes the lines; a note while one does not); and lines, or a task of
the repository's own, naming `${worktree_cache}` while a sykli that runs the
gate does not expand it, by its path: a sykli downgraded after adopt builds in
the work tree. A copy an entry still holds beside a derived gate is noted.
For a stored gate it checks, as before, its cargo tasks' lines and whether its
`sykli.lock` pins it (`sykli validate`, from the entry's clone and handed its
facts when one is under the roots, else from the entry, where sykli asks git
once), which an adopt cut short between writing the two leaves behind. Adding
the lines changes the gate's digest, so receipts from before read as another
contract's; a run of the repository's own contract is still its own.

## What it will never do

- **No messaging between agents, no request/response, no presence.** A message
  from an agent that has since died is a silent lie. Knowledge is not a
  message: it carries its time, its why and whose it is, and a person reviewed
  it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests. A handover is a record for whoever comes next here, shown in
  the next briefing, never a message addressed to a particular agent.
