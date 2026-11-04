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
ohjaa brief --agent claude             # the briefing an agent gets here
ohjaa brief --scope ../other-repo       # retrieve context after expanding scope
ohjaa propose --scope ours --id rust-only --kind decision \
      --origin "2026-09-16: 'we dont do python'" "Code is Rust or Elixir/Erlang."
ohjaa review                           # a person decides what becomes current
ohjaa lessons publish --dry-run        # repo knowledge into each repository's own AGENTS.md
ohjaa intake                           # draft proposals from what people said
ohjaa report                           # what went around the workflow this week
ohjaa line                             # for a terminal's opening: what is new since you looked
ohjaa digest                           # what happened, for you: all of it
ohjaa check                            # validate, and flag what may no longer hold
ohjaa session                          # the record of the last ended session here, or of one by id
ohjaa graph rust-only                  # a node, its neighbours, who was briefed with it
ohjaa fleet                            # who is live, across every repo
ohjaa system                           # what every tool in the fleet is, derived now
ohjaa apply                            # apply the workflow from ~/.config/ohjaa, outside every repo
ohjaa adopt ~/projects/sykli           # give a repo an entry: the branch ohjaa observes, and the build settings its gate gives cargo
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
| repo | `<repo>/AGENTS.md`, its `## Lessons` section; ohjaa keeps each node, with its why, in `~/.local/state/ohjaa/lessons/` | a pull request ohjaa opens |
| mine | `~/.config/ohjaa/mine/` | you |
| machine | `~/.config/ohjaa/machine/` | you |

The launch repository's briefing is ordered **ours → repo → mine → machine**, then **decision →
fact → taste → lesson**. A personal node can never supersede a shared one, and
shared Knowledge never builds on personal Knowledge. Those rules come from
faija's contract, and ohjaa carries its reference implementation in
`vendor/faija-knowledge`.

The launch repository's own knowledge that its AGENTS.md holds, as checked
out, is left to that file, which the agent reads (see "Repo knowledge: the
repository's own AGENTS.md"). Every explicitly scoped repository also
contributes its repo knowledge, all of it, labelled with its path, within a
budget of its own (every decision in full; see "Keeping it true"): the agent
is not working there, and need not read that AGENTS.md. Global knowledge is
delivered once, not repeated for each repo. No keyword matcher drops a
decision. Session records preserve the repository identity of delivered repo
nodes; older records without that identity still read.

## Agents propose, a person decides

- `ohjaa propose` writes a proposal to `~/.local/state/ohjaa/proposals/`,
  **outside every repository**. An agent's draft never lands in a shared repo
  by itself.
- `ohjaa review`, `accept`, `reject`, `retire` and `confirm` refuse to run
  without an interactive terminal, and agents run without one.
- Interactive `ohjaa review` shows one numbered proposal at a time: a readable
  title, its scope, the complete rule and its sources, and what accepting it
  does. Original spacing and code indentation are preserved; the terminal
  handles visual wrapping without reformatting quoted commands or literals.
  Replacements, refinements and possible duplicates stay visible before
  the choice. Type `a` to accept, `r` to dismiss, `s` or Enter to leave it for
  later, `d` for the original identifier and proposal file, or `q` to stop.
  Full action names and uppercase work too. Invalid input (including `aq`)
  changes nothing and asks again; end-of-input stops without skipping the rest.
  Each decision is saved immediately, with brief confirmation rather than a
  file-operation log. History failures, instruction-update failures and partial
  instruction coverage remain visible. The final summary counts decisions and
  proposals still waiting; `ohjaa list` shows saved knowledge. `review --list`
  remains read-only. Unreadable proposals or evidence make review incomplete
  (exit 4), not an empty or complete queue.
- Accepting a node that supersedes another marks the old node `superseded` in
  the same step, and refuses outright if the result would not hold together.
  Nothing is deleted: retired and superseded Knowledge keeps its time and its
  why.
- Shared Knowledge is then reviewed in a pull request, like code. For shared
  scopes that review is the real gate. Ours is committed in its checkout by
  the person. Repo knowledge goes into the repository's own AGENTS.md
  through a pull request ohjaa opens, and the person merges it like any
  change: see "Repo knowledge: the repository's own AGENTS.md".
- Personal Knowledge (mine, machine) is versioned in the home itself:
  `~/.config/ohjaa` is a local git repository, with no remote and never
  pushed, made by its first commit (or the repository the home already is).
  Each decision on it (`accept`, `reject`, `retire`, `confirm`, and each one
  `review` makes) is one commit, with your git identity, saying what and why:
  `accept code-is-rust: Code is Rust or Elixir.`, then the kind, the scope and
  each `why:` of its origin; a retirement carries its `--why`. `ohjaa apply`,
  `apply --undo` and `adopt` commit what they change in the home the same way.
  A commit that fails (another git holding the index, no identity) is said,
  and the decision stands; the next commit takes it in. `git -C
  ~/.config/ohjaa log` is the history of what you decided.
  - One change at a time: each decision (its write, its commit and the
    agents' block below), apply and adopt holds the home's lock
    (`.git/ohjaa.lock`), so decisions made at once are a commit each, in turn.
    The home's git holds it too, for as long as it runs (the lock is its
    stdin): an ohjaa killed while it commits leaves its git to finish (it
    runs in a process group of its own), and the next change waits for that
    git, not only for the ohjaa. Nothing that git could start beside itself
    is left running to hold the lock after it: the home's git runs no hook
    (`core.hooksPath=/dev/null`, `hook.<event>.enabled=false` where git has
    it, and the home is made with no template), no fsmonitor
    (`core.fsmonitor=false`), no automatic gc or maintenance, and no
    signing. A hook git's config defines by name (`hook.<name>.command`)
    still runs under git 2.54, which has no switch for all of them, but git
    gives it /dev/null for its stdin, so nothing it leaves holds the lock.
  - Only what ohjaa keeps there is committed: `mine/`, `machine/`, `repos/`,
    `git/`, `roots`, `applied.toml`, `ours` and `drafter`, by an allowlist in
    the repository's own `.git/info/exclude` (the home holds no `.gitignore`).
    A `.DS_Store`, an editor's swap or backup file, a half-done write's
    temporary file, or a file of your own beside them never is.
  - The `drafter` file, a command line, is in the history too. Nothing ohjaa
    writes there is secret, but keep any key out of that line (give the
    drafter its key from your environment).

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
- **Reverts:** reverted commits on each repository's current branch.
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

**Secret-looking tokens are redacted before the drafter: a best effort over
known shapes, not a guarantee.** People paste credentials to agents. As each
prompt is made, everything in it but ohjaa's own instructions is scrubbed,
from every source, with the filter on or off: the knowledge it lists (each
line whole, before it is cut to fit), and each candidate's time, source,
place and author, its words and the agent's words before them. A token in one
of these shapes becomes `[redacted]`, and is counted:
- a known key prefix: `sk-`, `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`,
  `github_pat_`, `xoxb-`/`xoxp-`/`xoxa-`/`xoxs-`, `AKIA`/`ASIA`, `glpat-`,
  `AIza`;
- a JWT, or a PEM private key block;
- an id and a secret joined by a dot (`<hex, 24+>.<letters and digits,
  12+>`), as some providers issue keys;
- a token of 32 or more characters with two or more each of capitals, lower
  case letters and digits, unless it is hex (a hash or a UUID, either case);
  a path is judged by the pieces of its names;
- a long mixed token right after a word that names a secret (`api_key=…`,
  `GITHUB_TOKEN=…`, `password: …`, `Authorization: Bearer …`). That
  includes a sha or a UUID after `key:` or `token:`.

Anything else gets through: a short password, a key with no digits, or one in
a shape not listed. Long random ids that are no secret (document ids, a
base64 alphabet in code) are redacted too, at little cost to the drafter. On
the 14 days of 2026-10-26, with `--no-prefilter`, 4 tokens were redacted in
1,383 candidates: the credential the review of #56 found (no token of its
shape reached the drafter) and, by a count per kind of token, document ids. The original is never said, logged or kept; each run says only how many
were redacted, and a dry run shows the scrubbed prompt. Whether the drafter
is a local model or `claude -p` is Yair's decision, and still open. Until it
is made, the filter's default (on) also keeps most messages from leaving the
machine at all.

It sends them in batches to a **drafter**, an agent CLI that reads a prompt on
stdin and prints its answer as one JSON object: its drafts, and what each
candidate re-teaches (see *Does knowledge work?* below). The default is
`claude -p`; override it with
`--drafter` or `~/.config/ohjaa/drafter`. The drafter sees what is already
present where the batch comes from: the existing Knowledge, then the
proposals waiting for a person, of ours, mine and machine, and of each
repository the batch's candidates come from. A candidate comes from a
repository when its place (or, for a review, the repository it was read
from) lies in it, or names a folder after it, as a pull request's URL and a
worktree named `<repo>-<topic>` do. Each node is shown by its id, scope, kind
and status (a repo one with its repository) and the first 100 characters of
its body, in at most 24,000 characters; what does not fit is counted. On the
131 nodes present after the first real run, every batch carried all of them
(about 21,000 characters); now a batch from no repository carries 75 (12,011),
most repositories add 1 to 3 (to about 12,600), habitat 14 (14,821) and
valvo 24 (16,799). The drafter is told to return nothing for a lesson already
present. When a candidate adds to a present node, it names that node in
`refines` and says what it adds. When a person corrects an agent on what a
current node already says, it names that node in `reteaches` and drafts
nothing, and that is how a node that does not reach agents gets noticed.

**A draft that may repeat what is present is written, and said to a
person.** Every draft costs a person a review, and only a person decides
what is Knowledge, so the drafter's answer is checked too, and nothing in it
is dropped. A draft whose title says mostly what the title of a present node
of its own scope says (for a repo one, of its own repository: the one it
names, else the one intake runs in) is written like any other proposal, and
said to be a *possible duplicate* of that node, the nearest one. Present
means current, or proposed and waiting; a superseded or retired node is not.
The node a draft refines, current or waiting, and those it supersedes, are
not compared: a refinement says its base again by design, and is kept as
refining it (one refining a proposal still waiting can be accepted once that
one is).
- A node's title is its id: its words, lowercased, without filler (`a`,
  `the`, `and`, `with`, …), each with one common ending cut (`-ing`, `-ed`,
  `-es`, `-s` but not `-ss`, `-e`).
- Two titles are near when they share at least two words, and the shared
  words are at least 60% of both: Dice's coefficient, 2·shared / (a + b) ≥
  0.6.

Nothing of it is stored: it is derived each time it is said. A run says
`proposed <id>, a possible duplicate of <present id>`, and its summary counts
them by what they may repeat; each draft written is present for every draft
after it, in its answer and in the batches after. `ohjaa review` works it out
again for every waiting proposal, against current knowledge and the
proposals that came before it (older, or as old and stored first), so of two
near ones only the later is said, and shows it right after the one it may
repeat when that one waits too, with the first line of it when it is
current. Among the 131 ids present after the first real run, 8 pairs are
near, each one lesson; the review of #61 made up distinct lessons that are
near too (`tapio-linux-only` and `rauha-linux-only`, two valvo LSM lessons, a
correction that reuses the words of what it corrects), which is why a near
draft is said, never refused. The same lesson in other words
(`gate-before-pushing-main`, `prove-with-ohjaa-gate-before-merging`: 0.44)
is the drafter's to see, shown both.

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
- A repository whose current branch was never committed to (one with no
  commits yet, or a new orphan branch) has nothing to read: it is empty, not
  unread, and holds nothing. git refuses it in words it also uses for a
  branch whose ref was lost (a loose ref deleted, `packed-refs` cut short);
  a lost ref keeps its reflog, so a branch with one is said as lost, warns
  and holds the cursor. With reflogs off (`core.logAllRefUpdates`), a lost
  ref reads as empty. git is asked in the C locale,
  `LANGUAGE` removed, and no git process more is spent on this. One git
  cannot read at all (its `.git` names nothing, a garbage ref, a missing
  object) warns, and holds the cursor.
- vessa's exit 4 (a partial list) names two kinds of reason, which intake
  treats apart. What it lists is drafted from either way.
  - A source it could not read (`warnings`), what a transcript leaves unknown
    (`limitations`), or a run it ended with an `error` is a warning: said,
    and it holds the cursor. So is a vessa missing from PATH, one that cannot
    list messages (older than `messages`: update it), or output intake cannot
    read.
  - A store vessa knows of and never reads (`coverage.gaps`; today
    opencode's SQLite, named only when written in the window) is said on
    every run and holds nothing: not the cursor, not a session, not the exit
    code. Waiting cannot read it, and a hold would keep the window before
    every later write, so it would never end. Once vessa reads the store,
    `ohjaa intake --source transcripts --since <its first day>` reads it back.
- The report's own gaps are said, and do not hold the cursor.
- A first run, with no cursor yet, keeps the start of its window (fourteen
  days back) as the cursor before it drafts.
- A `--since` before the cursor reads that stretch again, and sends once more
  what is no longer kept as handled.

## Does knowledge work? What a person had to teach again

The briefing gives agents knowledge; this is how ohjaa sees whether it changes
what they do. A node the person keeps re-teaching ("no Python", "use Lima",
"I asked you twice") is not working: it needs rewording, or making a
decision. A node nothing touches may no longer hold.

**The drafter says what each candidate re-teaches.** Its answer is one JSON
object:

```json
{"drafts": [{"scope": "mine", "repo": null, "id": "…", "kind": "lesson",
             "body": "…", "origin": ["…"], "refines": null, "supersedes": []}],
 "reteaches": [{"candidate": 2, "ids": ["rust-elixir-only"]}]}
```

`candidate` is the number the prompt gives a candidate (`[2]`), and `ids` the
current nodes it corrects an agent against. A message that only re-teaches
makes no draft. The drafter is already shown the current nodes' ids and first
lines, scrubbed of secrets like the rest of the prompt, within its 24,000
characters, current knowledge first so that it is the last to be cut. An
answer in the shape before, a bare array of drafts, is still read, as
re-teaching nothing.

**Its answer is checked.** A re-teaching is kept only when:
- it names a candidate of the batch, and that candidate is a person's words: a
  message in an agent session or a review comment, never an observed failure,
  a handover or a revert;
- its id is one current node the drafter was shown with that candidate: a
  repo node only with a candidate from its repository, and an id defined in
  two scopes is not one node.

Everything else is dropped and counted by why on every run (`5
re-teaching(s) the drafter named were dropped: 3 naming no current node shown
with its candidate, 1 of no candidate in its batch, 1 unreadable`). One entry
written wrong costs only itself; an answer that is neither drafts nor
re-teachings is refused, and its batch split, as before. A run says each
re-teaching it records by node, time and source, never the words
(`re-taught rust-elixir-only (2026-09-21T10:00:00Z claude)`), and counts them
by node at its end.

**Kept as history**, like the session log, in
`$XDG_STATE_HOME/ohjaa/knowledge-evidence.json` (`ohjaa-knowledge-evidence.v1`).
Per node (its scope and id, and for a repo node its clone, so every worktree
of a clone shares its count and another repository's node of the same id has
its own) it holds:
- each message that re-taught it: its time, the digest intake keys it by, and
  its session's id (the agent's own, as vessa names it), never its words;
- the newest time a draft cited it, refining or superseding it, as of the
  newest message in that draft's batch.

It is written whole, after each batch the drafter answers for and before that
batch is kept as handled. A message counts once per node however often the
drafter answers for it: a run killed before its progress was kept, a deleted
`intake-handled`, a `--since` back over it, or `--retry-failed` sends it again
under the same key. Deleting the file loses no knowledge, only the counts. A
damaged one stops intake before anything is sent, and is never replaced;
`ohjaa list`, `check` and `review` say so, and show no counts rather than
zeros.

**Shown, and only suggested.** A count is since the node's `as_of`, when a
person last accepted or confirmed it: a message said before that (its own
origin, say) did not re-teach it.
- `ohjaa list`, under each current node: `re-taught 3 times since 2026-09-20,
  last 2026-09-23`, where last is the newest re-teaching or citation since.
- `ohjaa check`: the same for each current node (`evidence <id>: …`), then a
  suggestion where one applies (`suggest  <id>: …`), counted in its summary:
  - re-taught 3 or more times in the last 30 days: `reword it, or make it a
    decision`;
  - nothing re-taught or cited it, and no person accepted or confirmed it, in
    60 days: `still true?` (a fact already flagged as not re-confirmed is
    asked by its flag).

  A suggestion is never a flag: `check --strict` does not fail on one.
- `ohjaa review`, when a proposal may repeat a current node, or refines one:
  that node's count, under it.

ohjaa never changes a node itself. Rewording, deciding, confirming and
retiring stay a person's.

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
  - A session belongs to a launch that recorded its id: what leaving found
    by its launch prompt (`left.native`; see "The agent's own sessions"),
    the launch's usage receipt, or what its agent's environment claimed from
    inside it (for a launch that left, as its leaving kept it; for one
    still open, `native_env`; an id claimed after a launch ended never
    counts). A launch with an id of its own claims no other session. A
    session named by several launches (resumed in each) is each one's.
  - A claim never outranks an observation: an id another launch's prompt
    or receipt names is not the claiming launch's, and counts for nothing
    there.
  - Only a launch with no id of its own falls back to time and folder: it
    claims at most one session, the same agent, in the launch's repository
    or below, started while the launch ran (from 5 s before it was logged
    until it left, or now while it runs). An agent can write its first
    record minutes into its launch.
  - When more than one could be a launch's, the one whose transcript was
    last written closest to the launch's end is its own (for a launch still
    running, or closed as an orphan, the one that started closest to it).
    Each session is any launch's at most once, and the same list always
    gives the same answer.
  - Each session a launch claimed is said with how: counted by way in the
    text (an environment's id as claimed by the agent), where each one
    taken by time and folder alone is named; in `--json`,
    `bypasses.matched`, each `{agent, id, session, by, testimony}`, `by` one
    of `launch_prompt`, `receipt`, `env` and `time_and_folder`, and
    `testimony` true for a receipt's or the agent's word. A session named
    several ways is said by the surest: launch prompt, then receipt, then
    the agent's claim.
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

## The human view: what happened, for the person

```bash
ohjaa line            # at terminal startup: attention only, otherwise silent
ohjaa digest          # attention details and unread activity; marks activity seen at a terminal
ohjaa digest --since 7d   # a window instead; the cursor stays
ohjaa collect         # gather the snapshot `line` reads (it starts this itself when it is old)
```

An agent learns the state of the work from its briefing. A person learns it
here. It records only: nothing is sent to anyone, decided or refused.

- **`ohjaa line`** shows only matters needing attention or a decision:
  failed or missing verification, observations that could not be read,
  sessions without an observed final state, long-open sessions, and proposed
  agent instructions waiting for you. The first terminal of a local day
  shows up to 15 lines of attention; later terminals show a short notice
  only when attention changes. With no attention it is silent, even when
  sessions ended or pull requests were merged. Silence is not a claim that
  every system is healthy. Terminals opened together say a notice once:
  the one that claims `seen.lock` says it.
- **Opening a terminal never marks activity read.** The first startup
  anchors the unread window, even when quiet; subsequent days do not move
  that window. Existing unread positions from older versions are retained.
- **Saved checks and open records mean different things.** Startup identifies
  an old background update and gives `ohjaa collect` to refresh saved checks;
  session records are read now. An unknown or incomplete secrets scan is not
  a finding and cannot establish an all-clear. An old, unclosed session record
  may still be active: age alone does not mean it is stuck. The digest separates
  these records from failures and shows their recorded task, or the last
  recorded goal when the launch label was only `interactive`. Goals are context,
  not proof of current progress; absent descriptions are stated explicitly.
  Proposed instructions are labelled as not active until you decide.
- **Startup is a summary, not a diagnostic dump.** Unreadable saved sources are
  grouped by what is missing, with one refresh command; paths and detailed
  reasons stay in `ohjaa digest`. Multiple old open records are grouped by
  repository, without claiming that their agents are stuck. Possible secrets
  are labelled as unconfirmed pattern matches, not confirmed credential leaks
  or a count of unique credentials. Full findings, individual inspection
  commands, source dates, and incomplete coverage remain in the digest.
- **A match count is not a leaked-key count.** Startup separates recognizable
  key-format matches from secret-labelled values and unclassified rules. Generic
  random-string matches alone stay in the digest, not in startup attention. When Vessa
  reports context, it says how many occurrences are in signature/encrypted
  fields, without suppressing those matches. Older reports leave context
  unknown; partial context is a lower bound, never a complete total. Timestamp
  gaps are counted separately from other coverage issues when available.
  Counts are occurrences, not distinct credentials; no validity is inferred.
- **Redaction is not credential detection.** Generic random-string
  (`mixed_token`) occurrences remain scan evidence in the digest but do not
  trigger startup credential alerts, regardless of context. The heuristic
  checks string length and character mixtures; that alone cannot distinguish
  keys from identifiers, encoded data or examples. Unknown context stays unknown,
  never known-safe, and all counts remain inspectable. Known
  key formats, secret-labelled values, unknown rules, and incomplete
  coverage still need attention. No detector or redaction rule is disabled,
  and opening a quiet terminal does not consume the underlying evidence.
  A changed reviewable subset has its own attention identity and observed
  scan time: reading metadata earlier does not acknowledge later candidates,
  and an unchanged re-scan does not replay the same alert.
- **`ohjaa digest`** shows attention details, unread agent sessions and
  pull requests, proposed instructions and who is live. Each ended session
  has its full recorded intent, end time, elapsed time and an inspection
  command, `ohjaa session <id>`. A pull request has its URL. Reading a digest
  at a person's terminal marks activity seen under `seen.lock`; piping it,
  using `--since`, or running it inside an agent session does not. A digest
  that cannot claim the lock within 2 s shows the same information but
  leaves the cursor unchanged.
- **Only a person sees it.** `line` prints, reads, writes and starts nothing
  unless stdout is a terminal and no agent session's marker is set:
  `OHJAA_SESSION`, `TOIMIJA_SESSION`, `TOIMIJA_RUN`, `CLAUDECODE`,
  `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`,
  `CODEX_SESSION_ID`, `CODEX_CI`, `CODEX_SANDBOX`,
  `CODEX_SANDBOX_NETWORK_DISABLED`, `CODEX_MANAGED_BY_NPM`, `PI_SESSION_ID`,
  `PI_CODING_AGENT`, `OPENCODE`, `OPENCODE_SESSION_ID` and `AI_AGENT`. These
  are the variables the agents here set in the shells they run tools in, and
  an agent's shell reads `~/.zshrc` too. A variable a person sets
  themselves, such as `CODEX_HOME`, is not a marker. `ohjaa digest` moves
  the cursor by the same rule, and never with `--since`.
- **`line` starts no git and touches no network.** It reads the session log
  and the proposals itself. Everything else comes from the snapshot `ohjaa
  collect` writes. When the snapshot is older than 15 minutes, `line` starts
  one collect in the background, detached, unless one is running, and says
  what cached observations it has with their dates. Routine background
  refresh alone is not an attention item. Cached observations are not the
  age of live session records. A refresh that cannot start is reported.
  `ohjaa digest` collects first when the snapshot is more than
  5 minutes old, and waits for it at most 10 s.
- **`ohjaa collect`** asks five sources once, at the same time. A source
  that fails keeps what it held before, and is said to be as of then.
  - The marks and the open sessions, as `ohjaa report --marks` finds them. A
    branch whose marks cannot be read keeps what it was last read as, and so
    does every branch when the entries themselves cannot be listed. An entry
    whose clone git could not read is said as that, never as one with no
    clone.
  - GitHub: one `gh api graphql` search for the pull requests updated in the
    owners of the workflow home's entries, at most 2 calls. It reads from
    where the next digest begins, by GitHub's clock (2 hours earlier, for
    what is indexed late), at least a day back and at most a week. A digest
    whose window starts earlier says `pull requests only since …`. A
    digest's pull requests begin those 2 hours earlier too, without what the
    last digest said of them, so one GitHub indexed late is said once.
  - vessa: `vessa usage --json --input <transcript>…` over the transcripts of
    the sessions that ended since then, found at leaving (`left.native`). It
    never reads every transcript.
  - Message excerpts: one `vessa messages --json` call for sessions ended
    lately, matched by observed native agent/session IDs and launch/end times.
    The first non-empty message leads the session row; the full recorded
    launch intent and inspection action remain available. Sessions without
    an excerpt keep their intent. Excerpts are scrubbed by Vessa (never
    requested with `--raw`), cut at a word to about 70 characters and retained
    only within the digest's window.
  - Secrets: one `vessa secrets --json` scan from the last complete scan,
    with a five-minute overlap. Only locations, rules, counts and optional
    SHA-256 fingerprint prefixes are kept, never secret values. New findings
    are attention notices; opening a terminal does not consume their digest
    details. The digest's **Secrets seen** section names the native session,
    whose words contained the finding, its time, rule and fingerprint.
    Rotate findings that are real keys. Malformed, unavailable or partial
    scan coverage stays unknown/incomplete and does not advance the last
    complete-scan boundary. An older snapshot with no scan says so too.

The secrets scan requires a Vessa that implements `vessa-secrets.v1`. Without
it, collection and the digest report incomplete coverage (exit 4), not an
empty successful scan. Default consumer tests use a stand-in for controlled
failure cases. The opt-in real-producer test below runs an independently built
Vessa against synthetic native records, checking excerpts, private findings,
partial coverage, preserved retry boundaries and recovery. No repository is
built or fetched implicitly.
- **What could not be read is said, never an all-clear.** A source that did
  not answer, or could not read all it asked (an entry that cannot be
  marked, a page of pull requests not asked for, vessa's partial usage, the
  fleet's failed scans), is said where the view is read, with how old what
  it holds is: `! marks  1 could not be read, as of 08:12 UTC: …`. `ohjaa
  collect` and `ohjaa digest` then exit 4, as `ohjaa report` does. The line
  says each failure of a source once, and a different failure of it again.

What stays until it is resolved comes first, in this order:

1. a branch with a failed commit;
2. what is unknown: a source that could not be read, a session still running
   with its ohjaa gone, an orphan, a commit sykli could say nothing about;
3. a branch whose recent commits have no recorded verified passing result;
4. what waits for a person: a session open more than a day.

Missing verification does not by itself mean tests failed. Failures are
counted explicitly, and an unavailable source remains unknown. Local
verified receipts and remote CI results are different observations. Branch
details identify the repository, latest observed commit, oldest commit
without a passing result, and when those facts were checked. Inspect them
with `ohjaa report --marks`. Before running `ohjaa gate`, use a checkout of
that repository at the recorded commit: the command proves the current
checkout's `HEAD`, not whichever branch a notice names. Ohjaa does not switch
checkouts on your behalf.

An agent's process exit describes the process, not task completion or test
results. Code 0 is successful process exit; another code has no inferred
cause. Elapsed time includes idle time. A task confirmation records that the
agent understood the task and rules; a completion note records what changed
and what remains. Missing notes are missing documentation, not failed code.
An `interactive` or empty launch intent is labelled **task not recorded**,
not assumed idle. A launch outside a repository says so and names its folder
and declared repository scopes, when recorded.

A branch is said as it was last read: when that was not just now, with its
age (`, as of 05:00 UTC (unread since)`), and it stays until a read resolves
it. A branch is said again when its tip moves. What happened is said once:
- each session that ended, with how it ended, how long it ran, whether it
  read back and handed over, what landed, its pull request and its token usage;
- the pull requests merged into a default branch;
- those opened or closed.

A pull request opened and merged in the same stretch is said once, as merged.

**Token usage** comes from Vessa. It is joined by the agent's own
session ids ohjaa found in the transcripts at leaving. It is unknown, never
zero, unless every one of the session's own sessions was read. Usage is given
in tokens, with cache reads apart (`115k tokens + 2.9M cache reads`). The
collector reads Vessa's token-only `vessa-usage.v2` exports and also accepts
the token fields of older v1 exports. It ignores legacy spending fields and
never shows currency estimates, including from a previously saved snapshot.

**Proposed agent instructions:** startup shows the waiting count and the
`ohjaa review` command, which lets a person read complete proposals and
decide what becomes an instruction. This is instruction review, not code
review. The digest shows one waiting proposal in rotation, with its complete
first-line summary before its id. It explains the scope (personal, shared,
repository or machine) and labels the instruction's date without claiming
it was created today or has waited since that date.

Lines wrap at 100 characters, except URLs kept intact for opening or copying.
Details wrap rather than disappear; the
short startup notice always retains `ohjaa digest` as its inspection action.
Digest source dates are separate for repository verification, GitHub pull
requests and Vessa token usage. These are human-readable changes only: no
machine schema, observation, receipt or authority is changed.
Lines use plain words with no colour, and
ids appear only where a person needs one: a session to look at with `ohjaa
session <id>`, a commit, a pull request, a proposal. Text from a source (a
title, an intent, a tool's error) has every control character and bidi
control replaced with U+FFFD before it reaches the terminal.

**State**, in `$XDG_STATE_HOME/ohjaa/human/`:
- `seen.json`, the cursor: when the person last saw the view, the keys of
  attention they saw and activity read in full (within the overlap), where
  unread activity begins and any older unread position retained from a
  previous version (each with the keys of what GitHub said in the 2 hours
  before it and sessions shown at the boundary second), and the day it was
  shown;
  new cursors also keep the secrets scan's own boundary, so a background
  result cannot be skipped because a digest read an older snapshot;
- `seen.lock`, which the terminal saying the line, and `ohjaa digest`
  moving the cursor, hold with `flock(2)`;
- `collected.json`, the snapshot: the marks and open sessions, GitHub's pull
  requests and vessa's usage, each with when it was read and what it could
  not read;
- `collect.lock`, which the one running collect holds with `flock(2)`.

Deleting them loses no knowledge. The view then starts from the last day.

### From viemari

`ohjaa line` replaces `viemari line`; its facts were ohjaa's already. To
switch over, once this ohjaa is installed:

1. Remove the two lines `viemari shell-init` added to `~/.zshrc`, the comment
   and `command -v viemari >/dev/null && viemari line`. `ohjaa shell-init zsh`
   now ends with `[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line`, and the line
   `ohjaa apply` wrote already evals it.
2. Run `ohjaa collect` once. Startup then shows attention only; use
   `ohjaa digest` to read activity and details.
3. Archive viemari. Its state, `~/.local/state/viemari/`, can then be removed.

Not carried over: viemari's lifecycle view (Intent, session, change,
validation, each sykli run asked by id), its rework and lead-time measures,
pull request reviews, and direct pushes that are proven. `ohjaa session`,
`ohjaa report` and `ohjaa graph` show the depth.

In particular, viemari's day listed each failed validation a session asked
sykli for (`FAILED: test`). This view does not ask sykli about validation
runs, so their absence here is **not a pass**: a failed run shows here only
as the mark of a commit on an entry's branch (`main: 1 commit needs passing
checks (1 failed)`). `ohjaa session <id>` shows the validations a session asked for.

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

The briefing includes the current intent's full text, including on continuation,
so a concrete goal is assigned work even without `--task`. An explicit `--task`
specifies work within that goal and stays separate; a placeholder intent such
as `interactive` without a task leaves the agent waiting for an assignment.
To start the turn, a concrete intent uses Toimija's task transport when no
explicit task is supplied. The Ohjaa session still records only the explicit
`--task` as its task; the goal remains its intent. Empty, `unknown` and
`interactive` intents do not start a turn on their own.

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
| exit 4: it passed under another contract or lock than the one it is judged by now | `unproven: it ran under another contract than the gate's (…)` | **yes** where it is `origin/<default>`'s tip; otherwise no |
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
blocks a launch; a broken one is reported and left alone, never replaced. A new
log is made whole beside its path and only then put in place, by a hard link or
else an exclusive rename (FAT has no hard links), so two launches making it at
once never take the other's half-made log for a broken one. Where the
filesystem has neither (exFAT on macOS), the log is made in place, never over
one, and the launch says why once. What a launch stopped while making the log
left beside it is removed by a later one.
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

### The agent's own sessions

Ohjaa records which of the agent's own sessions a session ran, so a
consumer such as vessa can join its accounting without an imported receipt
(D7). It has two ways to know, and records what each says: what it
observes, and what the agent claims (law 3: a claim is testimony, never
evidence).

The agent's claim is its own environment. Each agent sets its session's id
in every shell it starts for its tools: Claude Code `CLAUDE_CODE_SESSION_ID`
(the new id after a `/clear`; in a subagent's shell, its parent's), Codex
`CODEX_THREAD_ID`, pi `PI_SESSION_ID`. `ohjaa oriented` and `ohjaa
handover`, run in such a shell while the session is open, record the id
the launched agent's variable holds (only that agent's: a codex started
from a Claude shell inherits Claude's) at `native_env`, each `{agent,
session_id, at_ms}`, once, first named first. After the session has left,
the id is said and not recorded: a shell that outlived the session (a tmux
server started inside it) may be another session's. A value that is no
session id is said and not recorded. An agent that never reads back or
hands over claims nothing, and any shell can set the variable, so a claim
never outranks what ohjaa observed.

What ohjaa observes is the launch prompt. At leaving, and at the launch
that closes a session as orphaned, it looks for the launch prompt in the
agent's transcripts. It reads the
agent's transcripts written since the launch, never writing them: Claude
Code's `projects/<folder>/<id>.jsonl` (`$CLAUDE_CONFIG_DIR`, else
`~/.claude`), Codex's `sessions/…/rollout-….jsonl` (`$CODEX_HOME`, else
`~/.codex`) and pi's `sessions/<folder>/….jsonl` (`$PI_CODING_AGENT_DIR`,
else `~/.pi/agent`). A transcript whose header names another folder (Codex's
and pi's first record, Claude's first record with a folder and a time) is
read no further.

A native session is named only when its transcript was given the launch
prompt that names this session: the briefing's `This session is <id>;`,
which toimija gives the agent as its launch prompt. That is the first
message naming a session before the agent's first reply: a user message
(for Codex, a developer message too), behind whatever the agent's own
records put first. It is never a later message, a compaction summary, a
command's or a shell's output (`<local-command-…>`, `<bash-…>`), a tool's
output or a paste. Time and folder alone name nothing.

- A resumed native session (its transcript began before the launch) is
  named, with `resumed: true`, when its launch prompt naming this session
  was said within a minute of the launch. A message into an older session
  later resumes nothing. Every session a native session ran in names it, so
  its usage must not be added once per session. "Before the launch" is the
  launch's own instant, to the millisecond (`launched_ms` in the record), as
  the agents time their records: a session begun at 12:00:00.300 and resumed
  at 12:00:00.900 is resumed, though both fall in the launch's second, the
  one the session's id and its time windows are built on.
- A fork is named, with `forked_from`, when its parent is named for this
  session and it began before the session was last seen. That is now at
  leaving, and at an orphan close the latest time the orphan's record
  shows. A fork another launch made is that launch's: Claude marks each copy
  in a fork `forkedFrom`, so a fork's own launch prompt names its launch.
  Codex copies its parent's records into a fork restamped and unmarked, so
  a Codex (or pi) fork in which any prompt names another session is never
  named here.
- A subagent's work is its parent's. Claude's subagent transcripts are never
  read, and a Codex subagent's rollout names its root thread.
- Two native sessions each given the launch prompt name neither: the record
  says `ambiguous`, with their ids.
- Otherwise the record says why none is named: `not looked for` (opencode
  keeps its sessions in SQLite; other commands keep no transcripts ohjaa
  knows), or `none found`, with how many transcripts in the folder it read.
  `--no-briefing` gives no launch prompt to look for. Neither does pi unless
  toimija's `agents:` config gives it a prompt.

Both are recorded together at `left.native`: `sessions`, each `{agent,
session_id, source, resumed?, forked_from?, env?, testimony?}` (the
receipts' `agent` and `session_id`), `why` and `disagreement`. Each id is
there once. `source` is the transcript holding its launch prompt; `env:
true` says the environment claimed it too. An id only the environment
claimed has `source: "env"` and `testimony: true`: the agent's word, not
ohjaa's observation. `why` says why no transcript named one, or, beside
named ones, what could not be read. When the launch prompt named sessions
and the environment claimed only others, the observation wins: the claimed
ids are left out of `sessions`, and `disagreement` names both (a `/clear`
before the first read-back looks the same, so the claim stays in
`native_env`). `ohjaa session` shows them, each claim said as the agent's,
and for a session still open what its environment claimed so far.

`usage export` emits `ohjaa-sessions.v1`, with `sessions` and `unreadable`.
Each session is its record as the log holds it; what later versions added is
additive and absent from older records: `intent_id` and `task`, `pgid` and
`group_started_ms` (the session's process group), `launched_ms` (the launch's
own instant; `started_ms` is its second), `native_env` and `left.native` (the agent's own sessions, above), `validations` (see
`ohjaa gate`, and the integrated validations of "The integrated State",
with `not_dispatched`), and a delivered node's `via`: the agent's own
instruction file that held it at launch, so the briefing left it out (see
"Keeping it true"). A node the budget left out is not in `delivered`. A log an ohjaa with `ohjaa judge` wrote (removed
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
  fix is to merge or retire nodes.

A flag never hides a node. The briefing shows it with the flag beside it.

It also says, per current node, how often a person had to teach it again, and
suggests rewording what keeps being re-taught and re-confirming what nothing
touches: suggestions, never flags (see *Does knowledge work?* above).

Until the knowledge is merged or retired back under the budget, the briefing
degrades rather than falls off a cliff, and says so on stderr. It keeps every
decision in full, then the rest by priority while it fits: taste (how you
work, in every session), then lesson (what a correction taught), then fact
(the nearest to what can be found out again, and the first to go stale),
each the one last confirmed (`as_of`) first. The filling stops at the first
node that does not fit, and the briefing ends with the count of the rest,
``3 more: `ohjaa list` ``. No node is ever cut short. `ohjaa check` still
measures all of it.

For agents not launched through ohjaa, `ohjaa brief --write <file>` writes the
Knowledge as a generated block into an instruction file such as `CLAUDE.md` or
`AGENTS.md`, and leaves everything else in the file alone.

Your general knowledge (mine, machine) reaches every agent's own global
instruction file the same way, from one source, the nodes in the home.
`ohjaa apply` writes the block into each of these that exists, where the
agent itself finds it, and creates none:

| agent | its global instruction file (the first that exists) |
|---|---|
| Claude Code | `$CLAUDE_CONFIG_DIR` (else `~/.claude`)`/CLAUDE.md` |
| Codex | `$CODEX_HOME` (else `~/.codex`)`/AGENTS.override.md`, `AGENTS.md` |
| pi | `$PI_CODING_AGENT_DIR` (else `~/.pi/agent`)`/AGENTS.override.md`, `AGENTS.md`, `CLAUDE.md` |
| opencode | `$OPENCODE_CONFIG_DIR` (else `$XDG_CONFIG_HOME/opencode`, else `~/.config/opencode`)`/AGENTS.md` |

An agent that is set up but has no such file is said so (create the file,
even empty, and apply again), and a file inside a git repository gets no
block: one person's knowledge never goes into a file a repository shares.
The block is one line per current node, `kind: text`, in briefing order,
without origins:

```
<!-- ohjaa:knowledge begin: mine and machine, written by `ohjaa apply` and after each decision, … -->
Knowledge (ohjaa, mine and machine): what the person you work for has decided and knows. …
- decision: Code is Rust, or Elixir/Erlang.
- fact: Linux and eBPF builds and tests run in the Lima VM.
<!-- ohjaa:knowledge end -->
```

Past the briefing's budget (8,000 characters) it degrades as the briefing
does: every decision in full, then taste, lesson and fact while they fit,
then the rest by count, ``30 more: `ohjaa list` ``, and apply says so.
After each decision the block is rewritten wherever it is (moved, or
between your own lines), and only where it is: a block you take out stays
out until the next apply. `apply --undo` takes it out and puts back exactly
what was there, a final newline that was missing included. Every file apply
writes a block into is recorded, so undo takes back each, also one written
while a variable (`CLAUDE_CONFIG_DIR`, `OPENCODE_CONFIG_DIR`, …) moved the
agent's folder, whatever the variables say when undo runs. It is the same kind
of block `brief --write` writes, so a file holds one; a block apply did not
write (`brief --write`'s, or one you wrote) is yours: apply, each decision and
undo leave it as it is and say so. An agent's file apply cannot read as a
whole (two blocks, a marker without its partner) is said and skipped, and the
rest is applied, but apply is not done: it ends with `applied, except 1
agent file not written: <file> (<why>)` and exits 4, its commit names the
file, and every later apply and dry run, and `ohjaa doctor`, say so again
until you fix the file by hand. A home whose `.git` is a file (a worktree)
keeps the lock in the folder that file names. A linked worktree keeps the
allowlist there too, as `ohjaa.exclude`, which the home's `git add` is given
as its excludes file: git reads the common folder's `info/exclude` for every
worktree of the repository, and the allowlist begins by leaving out
everything, which would hide the other worktrees' new files. That file
takes the global excludes file's place for that one command, so it holds
your own excludes file's lines (`core.excludesFile`, else
`~/.config/git/ignore`), read for each commit, and then the allowlist: the
allowlist decides wherever it matches, and your patterns (a merge tool's
`*.orig`) everywhere else, as they do below `info/exclude` in a plain home.
The repository's shared `info/exclude` is still read above both. A bare
dotfiles repository (`git --git-dir=~/.dotfiles --work-tree=~`) has no `.git`
above the file, so ohjaa cannot see that a file is in it: give such an
agent's file no block by putting one of your own there.

An agent launched through ohjaa is not given this knowledge twice. `ohjaa run
<agent>` reads that one agent's own file (no git), found where the agent
itself finds it (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`
and `OPENCODE_CONFIG_DIR` move it), and compares its block with the block
apply would write now, byte for byte, never by a time. When the two are the
same, the briefing leaves out what the block holds and says where it is, in
one line, with the file's full path:

```
Mine and machine (41 nodes) are not repeated here: Claude Code has them from its own instructions, /Users/you/.claude/CLAUDE.md, whose ohjaa block is current.
```

A block over its own budget holds every decision and the rest by priority,
as above. The nodes it names only by count are then carried by the briefing,
within the briefing's budget, and the line says so (`Mine and machine:
Claude Code has 41 of their 42 nodes from its own instructions, …, whose
ohjaa block is current; the other 1 is above.`). A flagged node is still
repeated, with its flag, since the block carries no flags (`Flagged, so
repeated above: <id>.`).

The briefing carries mine and machine, within its budget, and its one line
says why (`Mine and machine are carried here, not left to Codex's own
instructions: /Users/you/.codex/AGENTS.md does not exist.`), when:

- the file is missing, holds no block, holds a block apply did not write, holds
  a stale one, or cannot be read;
- the agent will not read the file, by its own options (before its own `--`)
  or variables:
  - Claude Code 2.1.311 given `--bare` or `--safe-mode`, or with
    `CLAUDE_CODE_SIMPLE` or `CLAUDE_CODE_SAFE_MODE` set truthy, or
    `CLAUDE_CODE_DISABLE_CLAUDE_MDS` set at all, reads no CLAUDE.md; given
    `--setting-sources` without `user`, or an empty `CLAUDE_CONFIG_DIR`, it
    does not read that one;
  - pi 1.2.0 given `--no-context-files` or `-nc` reads no AGENTS.md or
    CLAUDE.md;
  - opencode 2.1.4 given `--server <url>` leaves its instructions to that
    server; without `--standalone` it leaves them to its background
    service, which reads them with the environment it was started with, so
    when `OPENCODE_CONFIG_DIR` or `XDG_CONFIG_HOME` is set (they move the
    file) the service may read another file than the one ohjaa checked;
  - Codex 0.164.0 has no option that skips its own `AGENTS.md`;
- the folder the file is in is not an absolute path (a relative
  `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR` or
  `OPENCODE_CONFIG_DIR`): the agent resolves it where it starts, so which file
  it reads cannot be told.

For any command but `claude`, `codex`, `pi` and `opencode` nothing is said.
The line is part of the knowledge section, and counts toward its budget.
`ohjaa brief --agent <command>` prints the briefing that agent gets; it
knows the environment, not the arguments `ohjaa run … -- <args>` would pass.

## Repo knowledge: the repository's own AGENTS.md

Every agent reads the repository's AGENTS.md, so that is where repo
knowledge goes. When you accept a repo proposal (`ohjaa accept`, or `a` in
`ohjaa review`), ohjaa writes its line into the `## Lessons` section of
that repository's AGENTS.md, through a pull request it opens, and you merge
it like any change. Nothing is written into your checkout:

- **The node** is kept in ohjaa's state, with its why:
  `~/.local/state/ohjaa/lessons/<clone>/<id>.md`, where `<clone>` is the
  clone's common git directory (read from git's layout), so every worktree
  of it shares them. Beside them, `delivery.json` (`ohjaa-lessons.v1`) says
  where each line went: `pr` (its URL and branch), `merged` (on the
  default branch when ohjaa last looked), `undelivered` or `held`, each
  with why. It is knowledge, not a cache: deleting it loses what is not
  delivered yet, and the why behind what is.
- **The line**, one per node, its body on one line, marked with its id:

  ```
  ## Lessons

  <!-- This repository's accepted knowledge: one line per node, each marked with its id, … -->

  - <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-27)
  ```

  A line is replaced by its id wherever it is (a confirmation moves its
  date, a second line of the same id goes), a superseded or retired one is
  taken out, and a new one goes after the last of the section's, else
  after what the section holds; the section is added at the end of the
  file when there is none, and AGENTS.md is made when there is none. Your
  own lines, and a line of an id ohjaa does not know, stay as they are.
  Writing the same nodes again changes nothing. What ohjaa writes into a
  repository names no tool: not its marks, its words, its branch, its
  commit or its pull request (a private repository's says why each was
  accepted, in your words). AGENTS.md is read whole, from the branch as
  fetched: one git cannot read, or that is not UTF-8, is never written
  over, and the delivery says why.
- **The pull request.** From the clone, never in your checkout: `gh repo
  view` (its visibility and default branch), `gh pr list --author `
  (your open pull request of the lessons, from a branch
  `agents-md-lessons/…` of that repository, gets the next commit, on its
  tip, never a force), `git fetch` of that branch, a
  worktree off it in `~/.local/state/ohjaa/lessons/.work/`, `git commit`
  (with your identity, and the repository's own hooks), `git push` to
  `refs/heads/agents-md-lessons/<date>-<n>`, `gh pr create`, and the
  worktree removed however it ends. git reaches the repository on GitHub,
  `https://github.com/<owner>/<repo>.git`, whatever origin pushes to, with
  gh's credentials (`gh auth git-credential`): no SSH key's prompt can hold
  it up, and your `insteadOf` rules still apply. The origin names the
  repository; it must be on GitHub. One delivery at a time
  (`lessons/lessons.lock`), which first takes out what a delivery that did
  not end (killed mid-push) left in `lessons/.work/`. Before the push, the
  commit must change AGENTS.md alone, and its blob be the text ohjaa wrote
  and checked: a hook that changed either stops the delivery. A hook that
  refuses is said in its own words. A decision lets the home's lock go
  before it delivers, so nothing else waits on the repository's hooks.
  `retire` and `confirm` of a node ohjaa holds deliver the same way.
- **When it cannot be delivered** (no remote named origin, an origin not on
  GitHub, no gh, gh signed out, a fetch or push that fails), the decision
  stands: it is said, `delivery.json` keeps why, `ohjaa list` shows it under
  the node, the briefing marks it, and `ohjaa lessons publish` delivers it
  once that is fixed. It is never dropped.

**A repository that is not private** (public, or internal) gets nothing
that names what is. Everything it would be sent is checked first against a
deny list built from your private context, asked of gh once a run, and
never from gh alone:

- the names of every repository you can see that is not public (GitHub's
  REST `user/repos` as owner, collaborator and organisation member, every
  page, keeping the private and internal ones). A list that holds no
  private repository sends nothing, and so does one that lacks a
  repository under your roots that `gh repo view` does not show as public:
  it is not complete (a fine-grained token, another account);
- the name of every repository under your roots, as its folder and its
  remote name it, that gh does not show as public (no GitHub remote, not
  listed);
- your home path, your home folder's name, and your GitHub login and name;
- always `ohjaa`, this tool's own name, whatever gh lists.

A line is read before it is matched: entities decoded, invisible
characters (zero-width spaces, soft hyphens, joiners) taken out, fullwidth
forms and the Latin letters with diacritics folded to ASCII (`viemäri`
reads `viemari`), combining marks dropped, and, a second time, HTML comments
and Markdown's `*`, `_`, `` ` ``, `~` taken out, so a split name joins
(`hab<!-- -->itat`, `**hab**itat`). A name matches at the start of a word,
whatever follows (`selkod`, `habitats`, `SelkoClient`), letter case not
told apart; the home path and `ohjaa` match anywhere (`.ohjaa/`). A letter
outside ASCII left after that (a Cyrillic а, a Greek ο), or an entity not
read, holds the line too, since it could stand for a letter of a name. A
false match only holds a line back, to be reworded. What that repository's
own URL says is no secret in it: its name, and its owner. A line that names
something on the list is held back and said, with what it named, and its
text goes nowhere: at `accept`, the accept is refused and the proposal stays
waiting; at `publish`, the node waits where it is. The rest of what would
be sent (the section's heading and intro when they are new, the commit
message, and for a new pull request its branch, title and body) is checked
too, and a match there stops the delivery whole. No why is ever sent to a
repository that is not private.

**The briefing** reads the launch repository's AGENTS.md as checked out
(the file, no git; a line inside a fence or an HTML comment does not
count). The nodes whose lines it holds, as they are now, are left to it,
but a flagged one, **only for an agent shown to read that file**, and one
line says so (a line of a node's id in other words, older or edited by
hand, does not count, and the node is carried, marked `AGENTS.md holds
another line of it`):

```
This repository's lessons (14 nodes) are not repeated here: Codex reads its AGENTS.md (/Users/you/projects/widget/AGENTS.md), which holds them in its ## Lessons section (its lessons end within the 32768 bytes of project instructions Codex reads).
```

Any other agent gets them carried, and the line says why:

```
This repository's lessons are carried here, though its AGENTS.md holds 14 of them: /Users/you/projects/widget/CLAUDE.md does not import AGENTS.md (an `.md` line), and Claude Code reads it then only where no CLAUDE.md is found.
```

| agent | left to AGENTS.md when | carried when |
|---|---|---|
| Claude Code 2.1.311 | a `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md` of the root, or of a folder between it and the launch folder, imports the root's AGENTS.md (`@AGENTS.md` in its text: not in frontmatter, fenced or indented code, code spans, HTML comments or HTML blocks), **and** it starts at the root, or the project's `hasClaudeMdExternalIncludesApproved` is true in `~/.claude.json` (it follows an import outside the folder it starts in only then); or `pluginConfigs` of your or your organisation's settings set the built-in `cc-plugin-agents-md` plugin's `instructionFiles` to `claude-md-and-agents-md` | otherwise: no CLAUDE.md (its fallback reads AGENTS.md only where no CLAUDE.md is found at the root or above, and one above every checkout defeats it), a prose mention, a launch in a subfolder without that approval, `--bare`, `--safe-mode`, `CLAUDE_CODE_SIMPLE` or `CLAUDE_CODE_SAFE_MODE`, `CLAUDE_CODE_DISABLE_CLAUDE_MDS`, `--setting-sources` without `project`, `--settings`, `claudeMdExcludes`, `managed-only` |
| Codex 0.164.0 | always, within its cap | an `AGENTS.override.md` at the root; the last lesson's line ending past `project_doc_max_bytes` (a `-c`, else `/config.toml`, else 32768) |
| pi 1.2.0 | always | `-nc`, `--no-context-files`; an `AGENTS.override.md` at the root |
| opencode 2.1.4 | always | `OPENCODE_DISABLE_PROJECT_CONFIG` or `OPENCODE_CONFIG_PROJECT_DISABLE` set; `--server` |
| any other, or `brief` without `--agent` | never | always |

The session log records a node as delivered through AGENTS.md only when it
was left to it. A line the checked-out AGENTS.md still holds of a node
retired or superseded, while its removal waits, is said, whatever the
agent, and for a scoped repository too:

```
AGENTS.md still says `X`, but it was retired on 2026-10-28 (the widget is gone); its removal is pending in https://github.com/acme/widget/pull/41: do not follow it.
```

A copy of a node left untracked in the checkout beside the one ohjaa's
state holds (a move cut short) is not read, and is said.

Until a line is on the checked-out commit, the briefing carries the node,
marked beside its id: `pending PR <url>`, `not delivered: <why>`, `held
back from AGENTS.md: <why>`, or, for one still untracked in the checkout,
`not yet in AGENTS.md`. A scoped repository's knowledge is carried as before.
`ohjaa list` says, under each current repo node, where it is.

**Nodes accepted before** sit untracked in `<repo>/.ohjaa/knowledge/`.
`ohjaa lessons publish [<repo>…]` delivers them, and every accepted node not
delivered: per repository, one pull request; once it is open, each file
moves out of the checkout into ohjaa's state, recorded in `delivery.json`
with where it came from and the pull request's URL, and an emptied
`.ohjaa/` is removed. A held one stays where it is. Without a repository
named, it takes each repository under the roots with a `.ohjaa/knowledge/`,
and each with accepted lessons not delivered. `--dry-run` says, per
repository, its visibility, each line it would add and what a line would
name, from AGENTS.md as last fetched there, and fetches, writes and sends
nothing. It exits 4 while anything is not delivered.

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
completion or session exit, not after every conversational reply. No node is
cut short to save tokens: past the budget the briefing names what it leaves
out by count, and no new index, database or model call is needed.

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
- The briefing `ohjaa run` hands toimija opens with the line
  `<!-- ohjaa-briefing v1 -->`. It says where ohjaa's words begin, so a
  reader of transcripts can tell a launch's task from the briefing toimija
  appends after it (vessa cuts there). It is matched byte for byte: a change
  is a new version, never an edit. `ohjaa brief --agent <command>` prints
  the same briefing without it, and `brief --write` never writes it into a
  file.
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

Launch agents from the folder that holds your repositories. You can give a goal
without choosing the repositories first:

```bash
cd ~/projects
ohjaa run codex --intent "reduce Toimija's Git pressure"
```

The agent receives that goal, identifies the repositories it needs, and declares
each with `toimija intent "<why>" --scope <repo>` before working there. This makes
its scope visible to other sessions; Ohjaa's launch-time attribution remains as
described below. When repositories are known at launch, name them with `--scope`:

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
    hooks/pre-push, hooks/<name>    exec '<ohjaa>' hook <name> "$@": pre-push, and each
                                    hook this entry's clones have of their own
    gated.config                    core.hooksPath = that hooks/ folder
  git/config                        includeIf "hasconfig:remote.*.url:<pattern>" per entry
  roots                             where apply and doctor look (from the first apply's --root)
  applied.toml                      what apply changed outside the home, for --undo
  mine/ machine/ ours drafter       (knowledge and settings, as before)
  .git/                             the home's history: one commit per decision on mine
                                    or machine, per apply and per adopt; never pushed
~/.gitconfig                        [include] path = ~/.config/ohjaa/git/config (once)
~/.config/git/ignore                git's global excludes (or core.excludesFile):
                                    a block marked "ohjaa apply" holds .sykli/, .teko/ and .ohjaa/
~/.claude/CLAUDE.md, ~/.codex/AGENTS.md, ~/.pi/agent/AGENTS.md,
~/.config/opencode/AGENTS.md        each that exists: a block marked "ohjaa:knowledge"
                                    holds mine and machine (see "Keeping it true")
```

```bash
ohjaa apply --root ~/projects --dry-run # the first time: name where the repositories are
ohjaa apply --root ~/projects          # the home's files, the include, the ignore block, ~/.zshrc,
                                       # the agents' knowledge blocks; then a commit in the home
ohjaa apply                            # later runs read the roots it wrote down
ohjaa apply --undo                     # put back what apply changed; entries, roots, history stay
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
checkout's files), and inside a session, or in a checkout with no
`sykli.json`, one more (HEAD, its tree and the branch tip); asking sykli about
a pending run starts none;
`doctor` two for the machine (`git --version`, the global config with
includes) and at most two per clone (its config, with `insteadOf` applied in
Rust, and for an entry's clone the tip's gate);
adopt asks sykli only for its `--help`. A gate `sykli init` writes for a tree (a commit with no
contract of its own, see "The gate of a commit") costs, every time a commit
is judged by it (once per tree in one ohjaa process, never kept), two more
(its scratch checkout's `git init` and `git read-tree -u`) and one `sykli
init`, which asks git twice more (`rev-parse` and `ls-files`, in the scratch
checkout): about 0.2 s for ohjaa's tree. `report --json --marks` on the
home of 2026-10-27 (three such entries, ohjaa, vessa and faija, each asked
for its tip) starts about 12 git processes more than it would without them
(43 against 31), and takes about 0.7 s more (1.8–2.0 s against 1.1–1.25 s),
each time it runs; so does each briefing in such a repository, for its tip
and HEAD.
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
that moved: no process. The home's history costs two per commit (`add -A`
and `commit`, with git's automatic maintenance off, which would start one
more, detached) and one more once, the home's `git init`: a decision on mine
or machine starts those beside the one that finds the repository it runs in,
and apply starts them when it changed something, adopt each time; the
agents' instruction files are read and written without git. `tests/config_home.rs`
holds these budgets, and `tests/terminal.rs` a decision's.

**`ohjaa gate`** proves HEAD: it runs the gate a push is judged by, from the
repository root: `mkdir -p .sykli && sykli run <contract> --json >
.sykli/receipt.json && sykli verify .sykli/receipt.json --contract <contract>
--commit HEAD`, where the contract is the gate of what the checkout runs (see
"The gate of a commit"): derived from the checkout's own `sykli.json` and
`sykli.lock`, read as files; in a checkout with no `sykli.json`, HEAD's gate,
derived as every commit's is (what `sykli init` writes for HEAD's tree, when
HEAD has no contract of its own: it declares what HEAD tracks, so it proves
HEAD as committed, and the gate says to commit first); or the repository's
own `sykli.json` when origin names no entry. When those two
files are HEAD's, that is exactly HEAD's gate; when they are edited (an input
added, as one must before committing it), it is the gate the commit they
will be in has, so the receipt proves that commit (a receipt names the
contract it ran under, and every commit is judged by the gate derived from
it). Where git converts line endings on checkout, the checkout's bytes can
differ from the commit's, and such a receipt reads as another contract's:
unproven, never a false pass. It prints the contract, where it comes from, and every command, and
exits with sykli's status; `--print` runs nothing and writes nothing (it names
where the run keeps the gate, and says when it is not written yet). The
receipt is in that worktree (`.sykli/receipt.json`, and the copy `sykli run`
writes under `.sykli/receipts/`), and sykli keeps it, too, in the run's own
folder under the clone's common dir (`<git common dir>/sykli/runs/<run>/
rcpt_<sha256>.json`; `sykli status --run` names it, and `verify --commit`
judges it from any worktree of the clone), and `sykli hook pre-push` copies
the one that proved a push into `<git common dir>/sykli/receipts/`. Those two
outlive the worktree: the push and the marks find the proof there once the
worktree is removed, as it is after its pull request merges (sykli keeps at
most `SYKLI_RUN_BUDGET` runs per clone, 100 by default, the oldest pruned
first). Agents are taught this one command.

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

**A commit with no contract of its own** (no `sykli.json`, and no
`sykli.rs`: ohjaa, vessa and faija today) is judged by what `sykli init`
writes for its tree, given the entry's lines as a repository's own contract
is, and locked as sykli init locked it. sykli init declares every file the
tree tracks (sykli#89), so a file added is declared by the gate of the commit
that adds it, with no adopt in between: a copy kept beside `repo.toml`, as
adopt once wrote with `sykli init --contract`, drifted the moment a file was
added. sykli is asked, by the sykli that judges the commit, in a **scratch
checkout** of exactly that tree: a repository of its own in a folder of the
ohjaa process's own under `$TMPDIR`, borrowing the clone's objects
(`objects/info/alternates`, read only), the tree read into its index and
work tree (`git init`, then `git read-tree -u`; no hook runs, nothing is
fetched), then `sykli init --contract <folder>/sykli.json`, handed that
checkout's facts; the folder goes when the process ends. Nothing is written
into the clone (no linked worktree), the config home or the state, so the
marks and the report can ask it. sykli init is asked again every time the
commit is judged (once per tree in one ohjaa process, in memory), and what it
writes is never kept to be read back: a run that outlives ohjaa (`ohjaa
gate`, an integrated validation) names its gate in `derived/<digest>/` as
any other, and that file is read only where it is, byte for byte, the gate
derived now. A sykli whose init writes another contract for a tree gives the
tree another gate, and a run under the one before reads as another
contract's. No receipt proves such a commit but one of a run under its
gate: there is no contract of its own to verify one against. What sykli init
cannot write a gate for (no `Cargo.toml`, `package.json` or `go.mod`) has
none, in sykli's words. An ohjaa killed while sykli init runs (a report, a
hook or a gate keep the default disposition) leaves its private folder,
scratch checkout and all; the next ohjaa that makes one first removes those
of every ohjaa that is gone (`ohjaa-gates-<pid>-<n>`, this user's, its pid
unknown to kill(2)).

An entry adopted before keeps working with no adopt: the copy it holds
(`sykli.json` and `sykli.lock` beside `repo.toml`) is never read, doctor
notes it, and `ohjaa adopt --force` removes it. A receipt of a run under
that copy proves a commit only where the copy is the gate derived now (as
when it was written by `sykli init` for that commit's files); else it reads
as another contract's: unproven, never failed. A commit with only a
`sykli.rs` (as toimija's first ones) has a contract of its own that no gate
is derived from: it is judged by nothing, and marked unproven
(`could_not_say`, "no sykli contract (sykli.json) in …"), never failed.

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
`--branch`, a commit with no gate: only a `sykli.rs`, or a tree sykli init
writes none for; sykli out of time), the push goes on,
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
  before, the gate of an older contract); or sykli could not say, or no gate
  judges it (only a `sykli.rs`, or a tree sykli init writes no gate for).

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
receipts are read as files, from the clone's layout: what sykli keeps under
the common dir, which outlives every worktree (`<common>/sykli/receipts/`,
what its hook kept for accepted pushes, and `<common>/sykli/runs/<run>/`, the
receipt each run concluded with), and, in every worktree, `.sykli/receipts/`
and `.sykli/receipt.json`; a receipt kept in two places under its digest's
name is read once, the same bytes being the same receipt (another file under
that name never hides it, and one sykli named that cannot be read is a gap
in the report). A forge's merge of an up-to-date pull request has the tree
of its head, so a receipt of the head proves the merge, also once the
worktree the head was proven in is removed. The marks are as of the last update of `origin/<branch>`
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

When some of those commits ran under another contract, it says the branch
is "not proven against its current gate" (or "has never been proven against
its current gate"), and how many ran under another one. A failed run counts
as the commit's failure only under its gate (or its own contract), as a
pass does: one under any other contract is `other_contract`, never ✗.

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

(sykli keeps a run's journal in the clone's common dir, and its receipt both
in the checkout it ran in and beside the journal, which the marks read: the
copy in ohjaa's state is kept all the same.) Leaving never reads the validation
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
  "unlisted": null | "<why>",            // why no entry could be listed (the workflow home
                                         // cannot be found, or its entries cannot be read)
  "entries": [{                          // every entry in the home; none, with complete
                                         // false and unlisted saying why, when they cannot
                                         // be listed
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
      "contract": "derived" | "own" | null, // a proven commit: the contract its receipt
                                         // ran under: the commit's gate (from its own
                                         // sykli.json, or what sykli init writes for its
                                         // tree), or its own sykli.json; "stored", the
                                         // entry's copy, is no longer given (2026-10-27)
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
commits' `contract` were added since, and on 2026-10-27 the top-level
`unlisted`, so a
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
commit, and a copy an earlier adopt kept is removed. That holds for a
repository with no `sykli.json` of its own as well: each commit's gate is
what `sykli init` writes for its tree, asked when it is needed, so adopt only
checks that the sykli on PATH can write a contract outside a repository
(`sykli init --help` names `--contract`) and refuses when it cannot. Then it regenerates `git/` and writes the entry's
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

The lines are given only when every sykli that runs the gate expands the
placeholder: the one on PATH and the one the pre-push dispatcher runs (as
`apply` recorded it). A sykli that does names `${worktree_cache}` in `sykli
run --help`, matched as that token, never as a sentence. An older one passes
it on as written, and cargo would build in a `${worktree_cache}/` folder
inside the work tree, so then adopt writes an empty `[cargo_env]`, saying why
in `repo.toml`, and says so.

`ohjaa doctor` says, for each entry with a clone under the roots, which
contract judges its branch's tip: the gate derived from the tip's own
`sykli.json`, or from what `sykli init` writes for its tree (asked in a
scratch checkout; doctor keeps nothing), and the tasks
given lines. It flags (drift) a tip whose `sykli.lock` does not pin its `sykli.json`
(sykli refuses the gate, as it refuses the repository's own); an empty
`[cargo_env]` while every sykli knows the placeholder and the gate has cargo
tasks ("gate runs may share a target dir across worktrees"; `ohjaa adopt
--force` writes the lines; a note while one does not); and lines, or a task of
the repository's own, naming `${worktree_cache}` while a sykli that runs the
gate does not expand it, by its path: a sykli downgraded after adopt builds in
the work tree. A copy an entry still holds beside `repo.toml` is noted as
never read. Adding the lines changes the gate's digest, so receipts from before read as another
contract's; a run of the repository's own contract is still its own.

## Boundaries in full

AGENTS.md and `.toimija/repo.yaml` carry these boundaries in short; this is
each in full, with what ohjaa keeps where.

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
  the lock that lets one intake run at a time (`intake.lock`), what intake
  saw of each current node (`knowledge-evidence.json`: per node, each
  message a person re-taught it with, by its time, a digest of its words and
  its session's id, never the words, and when a draft last cited it; history
  like the session log, read by `ohjaa list`, `check` and `review`, never by
  a briefing, a gate or a report), the report's two times (`report-at`,
  `report-shown-at`), and the human view's
  folder (`human/`: `seen.json`, what the person last saw and when, the keys
  of what was said in the last two hours, where the next digest begins and
  where the earliest of what a day's digest left out begins, each with the
  keys of what GitHub said in the two hours before it; `collected.json`,
  the snapshot `ohjaa collect` writes of what `ohjaa line` cannot derive
  without git or the network, namely the marks and open sessions, GitHub's
  pull requests, vessa's usage of the sessions ended lately, the first
  words the person typed in each of them (the person's own words shown
  back to them, as vessa prints them with secrets scrubbed, cut to about
  70 characters and kept only for the window the digest shows, never
  longer), and where and how often vessa's secrets scan found
  secret-shaped tokens in agent sessions (places and counts, never a
  value), each with when it was read and what it could not read; and two
  empty lock files, neither a cache: `seen.lock`, held while the cursor is
  read and moved, by the terminal saying the line and by `ohjaa digest`
  shown to a person, and `collect.lock`, held by the one collect that
  runs). Deleting them loses
  no knowledge: intake then reads its default window again, and sends once
  more what is still in it, the counts of re-teaching start again, and the
  human view starts from the last day.
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
  search for the pull requests of the entries' owners, at most two calls;
  its vessa calls, at most three, one each for usage, messages and
  secrets, are local), and after a person's decision on repo knowledge and
  in `ohjaa lessons publish` (gh: the repository's visibility, the person's
  private repositories and name once a run, its open lessons pull request,
  `pr create`; git: one fetch and one push to the repository on GitHub, by
  HTTPS with gh's credentials; a dry run fetches and pushes nothing). Every other
  command works from the filesystem and local CLIs, and `ohjaa line` starts
  no git process either.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.


## Tests

`cargo test` runs against stand-ins for sykli, toimija, vessa, gh and the
agents, in a sandboxed home, never the person's own. With `cargo fmt --check`
and `cargo clippy --all-targets -- -D warnings`, it is what CI runs.
`tests/lessons.rs` delivers repo knowledge to a local bare repository as the
remote: git's `insteadOf` turns the GitHub URL the checkout names into it,
and `GIT_ALLOW_PROTOCOL=file` keeps git off the network should that fail; a
person's accept and retire run on a pty of their own.

A test writes a stand-in, or any file it then runs, only through its
crate's `write_executable` (`process::write_executable` in the unit tests),
which has `cp` write it. Linux refuses to run a file any process holds open
for writing (`Text file busy`), and a child another test thread forks
holds a copy of every descriptor the test process has open until that child
execs, so a file the test process wrote itself, closed and even renamed
can still be open for writing in such a child at the moment it runs.

Tests that need the real tools are ignored by default, since CI's runners
have none of them:

- `tests/cli.rs`, `real_vessa_producer_*`: the actual Vessa command reads only
  a synthetic HOME, while Ohjaa collects and renders its output. Requires a
  Vessa implementing `vessa-secrets.v1`; older versions fail visibly:

  ```bash
  OHJAA_TEST_VESSA=/absolute/path/to/vessa cargo test --test cli real_vessa_producer -- --ignored
  ```

- `tests/config_home.rs`, `real_sykli_*`: what a flag says and what a mark
  is, in the real sykli's judgement
  (`OHJAA_TEST_SYKLI=<path> cargo test --test config_home -- --ignored`).
- `tests/loop.rs`: the whole loop, walked once, in a sandbox of its own.
  `ohjaa run` briefs a stand-in claude through the real toimija. The agent
  reads back, commits in a worktree, proves it with `ohjaa gate` (the real
  sykli), pushes to a bare origin's main and hands over. sykli's pre-push
  hook accepts the push, and the marks say ✓. Once the worktree is removed
  and the hook's copy is set aside, they still say ✓, from the receipt sykli
  kept of the run. `ohjaa intake` drafts a proposal
  (a stand-in drafter) from a correction the real vessa finds in the agent's
  transcript. A person accepts it at a terminal, and the next briefing
  carries it with the handover. Then a commit whose test fails is pushed:
  it is marked ✗, and the next briefing opens with the flag. Measured on
  2026-10-27, it takes 8 to 16 seconds on macOS and 3 to 8 on Linux:

```bash
OHJAA_TEST_SYKLI=$(command -v sykli) OHJAA_TEST_TOIMIJA=$(command -v toimija) \
  OHJAA_TEST_VESSA=$(command -v vessa) cargo test --test loop -- --ignored
```

## What it will never do

- **No messaging between agents, no request/response, no presence.** A message
  from an agent that has since died is a silent lie. Knowledge is not a
  message: it carries its time, its why and whose it is, and a person reviewed
  it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests. A handover is a record for whoever comes next here, shown in
  the next briefing, never a message addressed to a particular agent.
