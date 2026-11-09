# ohjaa

**Steering knowledge for agents, in the files they already read.** *Ohjaa* is
Finnish for *steer*.

Toimija reports facts and refuses to decide. Ohjaa holds the other half: what
the people an agent works for **know and have decided**. That covers the
decisions, facts, taste and lessons that
[`faija-ontology.v2`](https://github.com/false-systems/faija) calls
**Knowledge**. `ohjaa apply` writes the team's Knowledge and the person's into
each agent's own instruction file (Claude Code, Codex, pi, opencode), and a
repository's own lessons go into its `AGENTS.md`: every agent reads them
wherever it starts, however it was started. Ohjaa runs no sessions; the note
for the next agent is `toimija handover`.

```bash
ohjaa apply                            # write the Knowledge into each agent's own instruction file
ohjaa propose --scope ours --id rust-only --kind decision \
      --origin "2026-09-16: 'we dont do python'" "Code is Rust or Elixir/Erlang."
ohjaa review                           # a person decides what becomes current
ohjaa list                             # the Knowledge that applies here
ohjaa graph rust-only                  # a node and its neighbours
ohjaa check                            # validate, and flag what may no longer hold
ohjaa lessons publish --dry-run        # repo knowledge into each repository's own AGENTS.md
ohjaa intake                           # draft proposals from what people said
ohjaa brief --scope ../other-repo      # the Knowledge and context for a repository
ohjaa report                           # commits on a default branch without passing evidence
ohjaa line                             # for a terminal's opening: what is new since you looked
ohjaa digest                           # what happened, for you: all of it
ohjaa system                           # what every tool in the fleet is, derived now
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

## The report: commits without passing evidence

```bash
ohjaa report                    # the last 7 days
ohjaa report --since 30d --json # ohjaa-report.v1
```

`ohjaa report` derives, now, the marks on every entry's branch (see "Observe,
mark, flag" below) and, for the window, the **commits on a default branch
without passing evidence**:

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
- The rest are counted by why: verified (and how many of those reused tasks
  from sykli's cache, `verified_reusing_cache`); no sykli contract; made in
  a checkout that is gone, with no receipt kept (merge commits among them);
  sykli could not say. A window over the cap is said.

Ohjaa launches no agents, so no session is judged against a launch. The
JSON keeps its keys, as `ohjaa-marks.v1` only gains them:
`sessions_without_testimony` is empty, and `bypasses` is `{"judged": false,
"why": …}`.

A capped window and a root with no repositories are gaps. A report with
gaps says them, and exits 4: it is never a clean week. `ohjaa report`
records when a complete report was last shown (`$XDG_STATE_HOME/ohjaa/report-at`
and `report-shown-at`), but only when it printed to a terminal: never for
`--json`, a script or an agent.

`ohjaa intake` takes the report as its fourth source (`--source report`): a
refused or unchecked commit on a default branch becomes an `observed`
candidate. What could not be checked never does.

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
- **Your own credentials are an alarm.** A Vessa that compares with the
  credentials this machine holds (its report says what it compared with,
  `compared_with`) lists only those as findings, each naming where the
  credential is kept (`held`: `environment variable …`, `~/.aws/credentials`,
  gh's login). Each one is attention whatever rule the detector gave it:
  startup says "Your credentials reached agent sessions: N occurrences;
  rotate them", and the digest names each place it is kept and each session.
  Such a report replaces what an earlier Vessa listed of every other
  secret-shaped string, so that noise leaves the snapshot at the first collect;
  Vessa counts those strings itself (`candidates`) and ohjaa shows none of
  them. The paragraphs below describe an earlier Vessa's findings.
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
  in the log has its recorded intent, end time and elapsed time. A pull request has its URL. Its "Now" says
  who is live on this machine, then the pull requests GitHub last said were
  open in the entries' owners (updated in the window the snapshot read),
  the last updated first: the repository, the number, the author, whether it
  is a draft, its title, how long ago it last changed and its URL. That is
  where teammates' work shows, from any machine: a session that pushes its
  branch and opens a draft pull request as it starts is listed there. At
  most 20 are listed, the rest counted; GitHub never read is said under
  "Sources", never as none open. Reading a digest
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
    with a five-minute overlap. Only locations, rules, counts, where a held
    credential is kept, and optional SHA-256 fingerprint prefixes are kept,
    never secret values. New findings
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
   prints `[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line`, and the line
   `ohjaa apply` wrote already evals it.
2. Run `ohjaa collect` once. Startup then shows attention only; use
   `ohjaa digest` to read activity and details.
3. Archive viemari. Its state, `~/.local/state/viemari/`, can then be removed.

Not carried over: viemari's lifecycle view (Intent, session, change,
validation, each sykli run asked by id), its rework and lead-time measures,
pull request reviews, and direct pushes that are proven. `ohjaa report` and
`ohjaa graph` show the depth.

In particular, viemari's day listed each failed validation a session asked
sykli for (`FAILED: test`). This view does not ask sykli about validation
runs, so their absence here is **not a pass**: a failed run shows here only
as the mark of a commit on an entry's branch (`main: 1 commit needs passing
checks (1 failed)`).

## Session usage

Ohjaa runs no sessions now: the session log holds the sessions it ran
before, and only the gate's records are added to it. Agent usage is vessa's.
What is in the log can still be read:

```bash
ohjaa usage export > /tmp/ohjaa-sessions.json
ohjaa usage import /tmp/session-usage.json --session oh-EXISTING-ID
```

`usage export` emits `ohjaa-sessions.v1`, with `sessions` and `unreadable`.
Each session is its record as the log holds it; what later versions added is
additive and absent from older records. A log an ohjaa with `ohjaa judge`
wrote (removed 2026-10-26) still holds that command's tables, `judgements`
and `judgement_responses`; it opens and is written as any other, and those
tables are never read and left as they were. Unreadable records produce exit
4. `--session ID` selects one record. `usage import` attaches an
`agent-usage.v1` receipt to a session in the log; a refused receipt is
recorded as `refused: <reason>`, never as not collected. Exports contain
private session context; choose their destination accordingly.

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

The team's knowledge and yours (ours, mine, machine) reach every agent's own
global instruction file the same way, from one source, the nodes. Every agent
reads its own file wherever it starts; a workplace file in the folder above
the repositories is not read by Codex or opencode, which stop at a
repository's root (tested with each agent, 2026-11-09).
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
Knowledge (ohjaa): what your team and the person you work for have decided and know. …
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

`ohjaa brief --agent <agent>` does not repeat this knowledge. It reads that
one agent's own file (no git), found where the agent
itself finds it (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`
and `OPENCODE_CONFIG_DIR` move it), and compares its block with the block
apply would write now, byte for byte, never by a time. When the two are the
same, the briefing leaves out what the block holds and says where it is, in
one line, with the file's full path:

```
The team's, mine and machine (41 nodes) are not repeated here: Claude Code has them from its own instructions, /Users/you/.claude/CLAUDE.md, whose ohjaa block is current.
```

A block over its own budget holds every decision and the rest by priority,
as above. The nodes it names only by count are then carried by the briefing,
within the briefing's budget, and the line says so (`The team's, mine and
machine: Claude Code has 41 of their 42 nodes from its own instructions, …, whose
ohjaa block is current; the other 1 is above.`). A flagged node is still
repeated, with its flag, since the block carries no flags (`Flagged, so
repeated above: <id>.`).

The briefing carries the team's, mine and machine, within its budget, and its
one line says why (`The team's, mine and machine are carried here, not left to Codex's own
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
`ohjaa brief --agent <command>` knows the environment, not the arguments the
agent is started with.

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

## The perimeter

`ohjaa system` derives what every tool in the fleet is, from each repo's own
`.toimija/repo.yaml`: **derived now, stored never**, so it cannot be staler
than the moment it was computed. Who is live in a repository is `toimija
sessions`; what teammates work on elsewhere is their open pull requests, which
`ohjaa digest` lists under "Now" (claiming work stays in GitHub; see "What it
will never do"). `ohjaa brief` includes only the repository it runs in and
repositories named by `--scope`, after the Knowledge: `brief --scope PATH`
retrieves context when a task grows. Missing scope repositories produce exit 4
from `brief`, never an empty success.

The root list is derived as the sibling directories of your git root (outside
a repo, the folder you are in), unless `~/.config/ohjaa/roots` or `--root`
overrides it. A root that does not exist is an error, never an empty list.

## Coverage is a fact

A scan that failed is not a scan that found nothing:

```
coverage: scanned 17/18 repo(s)
  incomplete — absence here is not evidence of absence:
    somerepo: not a Git repository
```

- `ohjaa system` exits `4` when coverage is incomplete.
- A session toimija reports as `stale` stays visible, with its age, because
  "quiet for ninety minutes" and "gone" look the same from outside.

## The workflow home: applied from outside the repos

The agent workflow (the gate, the hooks, the terminal's line) is a workflow thing,
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
                                    holds ours, mine and machine (see "Keeping it true")
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
entry's hooks. The
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

Inside a session the log holds (`OHJAA_SESSION`, which only the removed
`ohjaa run` set), the gate first writes the
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

**Flag.** When the entry's branch is not proven at its tip, `ohjaa brief`
opens with it, before anything else:

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
    "state": "open"                      // never left (ohjaa runs no sessions now: these are older)
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
exactly what was there even after an apply that failed. The line it puts in
`~/.zshrc` (it prints `ohjaa line` as a terminal opens), in place of the old `source ~/projects/ohjaa-launch.zsh`,
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
  marked block of the team's, mine and machine knowledge (`ohjaa:knowledge`) in each
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
- **Ohjaa runs no sessions.** Agents start as they are typed and read the
  Knowledge from their own instruction files; the note for the next agent is
  `toimija handover`. Imported usage is separately labelled source testimony;
  it never becomes observed work or authoritative billing.
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
and `cargo clippy --all-targets -- -D warnings`, it is what CI runs. The same
three are the repository's `sykli.json` (with every input it reads, pinned in
`sykli.lock`), the contract `ohjaa gate` runs on a person's machine; a second
CI job installs sykli and runs that contract too, advisory until the plain
cargo job is removed on purpose. A new source or test file goes into its
inputs (`sykli init --force` rewrites them), or `sykli run` refuses.
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
  Vessa implementing `vessa-secrets.v1` that compares with the machine's
  credentials (`compared_with`); the test gives it its key as a secret-named
  variable. Older versions fail visibly:

  ```bash
  OHJAA_TEST_VESSA=/absolute/path/to/vessa cargo test --test cli real_vessa_producer -- --ignored
  ```

- `tests/config_home.rs`, `real_sykli_*`: what a flag says and what a mark
  is, in the real sykli's judgement
  (`OHJAA_TEST_SYKLI=<path> cargo test --test config_home -- --ignored`).

## What it will never do

- **No messaging between agents, no request/response, no presence.** A message
  from an agent that has since died is a silent lie. Knowledge is not a
  message: it carries its time, its why and whose it is, and a person reviewed
  it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests. A handover is a record for whoever comes next here, shown in
  the next briefing, never a message addressed to a particular agent.
