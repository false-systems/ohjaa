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
ohjaa line                             # for a terminal's opening: what is new since you looked
ohjaa digest                           # what happened, for you: all of it
ohjaa system                           # what every tool in the fleet is, derived now
ohjaa doctor                           # what differs from what apply would make it
```

The checks a commit must pass are the repository's own, in its `sykli.json`:
`sykli run` proves HEAD, and `sykli hook install --pre-push` makes the
repository's pre-push hook refuse a push to its default branch without a passing receipt.
Ohjaa runs no check and installs no hook.

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
decision.

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
  each `why:` of its origin; a retirement carries its `--why`. `ohjaa apply` and
  `apply --undo` commit what they change in the home the same way.
  A commit that fails (another git holding the index, no identity) is said,
  and the decision stands; the next commit takes it in. `git -C
  ~/.config/ohjaa log` is the history of what you decided.
  - One change at a time: each decision (its write, its commit and the
    agents' block below) and apply holds the home's lock
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
  - Only what ohjaa keeps there is committed: `mine/`, `machine/`, `roots`,
    `applied.toml`, `ours` and `drafter`, by an allowlist in
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

`ohjaa intake` reads three sources:

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
- PR comments and reverts are not filtered.

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
until the drafter answers for it.
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
that read all three sources, none with a warning, and then only past
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
    every run and holds nothing: not the cursor, not the exit code. Waiting cannot read it, and a hold would keep the window before
    every later write, so it would never end. Once vessa reads the store,
    `ohjaa intake --source transcripts --since <its first day>` reads it back.
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
  message in an agent session or a review comment, never a revert;
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

**Kept as history** in
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

## The human view: what needs you, and what happened

```bash
ohjaa line            # at terminal startup: attention only, otherwise silent
ohjaa digest          # attention details and unread pull requests; marks them seen at a terminal
ohjaa digest --since 7d   # a window instead; the cursor stays
ohjaa collect         # gather the snapshot `line` reads (it starts this itself when it is old)
```

It records only: nothing is sent to anyone, decided or refused. Ohjaa runs
no sessions and keeps no record of any: what an agent did in a working copy
is in `toimija history`, and what it used is vessa's.

- **`ohjaa line`** shows only what needs attention or a decision: a source
  that could not be read, possible secrets in agent sessions, and proposed
  agent instructions waiting for you. The first terminal of a local day
  shows up to 15 lines; later terminals show a short notice only when
  attention changes. With none it is silent, even when pull requests were
  merged. Silence is not a claim that every system is healthy. Terminals
  opened together say a notice once: the one that claims `seen.lock`.
- **`ohjaa digest`** shows the attention in full, the pull requests merged,
  opened and closed since you last read one, one waiting proposal in
  rotation, and "Now": who toimija says is live on this machine, then the
  pull requests GitHub last said were open in the owners of the fleet's
  clones, the last updated first, at most 20 listed and the rest counted.
  That is where teammates' work shows. A pull request opened and merged in
  the same stretch is said once, as merged. Reading a digest at a person's
  terminal marks activity seen; piping it, `--since`, or running it inside
  an agent session does not.
- **Only a person sees it.** `line` prints, reads, writes and starts nothing
  unless stdout is a terminal and no agent's marker is set
  (`ohjaa line --help` lists them): an agent's shell reads `~/.zshrc` too.
- **`line` starts no git and touches no network.** It reads the proposals
  itself and everything else from the snapshot `ohjaa collect` writes, said
  with its age. When the snapshot is older than 15 minutes, `line` starts
  one collect in the background. `ohjaa digest` collects first when it is
  more than 5 minutes old, and waits at most 10 s.
- **`ohjaa collect`** asks two sources once, at the same time. A source
  that fails keeps what it held before, and is said to be as of then.
  - GitHub: one `gh api graphql` search, at most 2 calls, for the pull
    requests updated in the owners of the fleet's clones under the roots
    (those with `.toimija/repo.yaml`). It reads from where the next digest
    begins, 2 hours earlier for what GitHub indexes late, at least a day
    back and at most a week.
  - Secrets: one `vessa secrets --json` scan from the last complete scan,
    with a five-minute overlap. Only locations, rules, counts, where a held
    credential is kept, and SHA-256 fingerprint prefixes are kept, never a
    value. One of this machine's own credentials is an alarm; broad pattern
    matches are evidence in the digest, not startup attention. It needs a
    vessa with `vessa-secrets.v1`.
- **What could not be read is said, never an all-clear.** A source that did
  not answer, or could not read all it asked, is said with how old what it
  holds is, and `ohjaa collect` and `ohjaa digest` exit 4. The line says
  each failure of a source once, and a different failure of it again.

Lines wrap at 100 characters, except URLs. Text from outside (a title, a
tool's error) has every control character and bidi control replaced with
U+FFFD before it reaches the terminal.

**State**, in `$XDG_STATE_HOME/ohjaa/human/`: `seen.json` (what the person
last saw, and where unread activity begins), `collected.json` (the
snapshot), and the two `flock(2)` locks beside them. Deleting them loses no
knowledge; the view then starts from the last day.

## Keeping it true

`ohjaa check` validates every node and the rules across them. It also flags
what may no longer hold:

- a `holds_while` predicate that is false right now;
- a **fact** not re-confirmed for 90 days (`ohjaa confirm <id>`);
- a node whose text names, in backticks, an `ohjaa` command this ohjaa does not
  have (`ohjaa run`, once it was removed): the rule asks for what no agent can
  do;
- a briefing longer than an agent will reliably read (8,000 characters). The
  fix is to merge or retire nodes.

A flag never hides a node. The briefing shows it with the flag beside it.

It also says, per current node, how often a person had to teach it again, and
suggests rewording what keeps being re-taught and re-confirming what nothing
touches: suggestions, never flags (see *Does knowledge work?* above). Before
those counts it says up to when intake read what people said, or that it never
ran here: a count from an intake that has not read is unknown, not zero.

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

A line the checked-out AGENTS.md still holds of a node
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

Ohjaa keeps what it writes for you in its home and applies it from outside:
nothing is installed on a repository or written into one.

```
~/.config/ohjaa/                    ($XDG_CONFIG_HOME/ohjaa)
  mine/ machine/ ours drafter       knowledge and settings
  roots                             where ohjaa looks for repositories (apply --root writes it)
  applied.toml                      what apply changed outside the home, for --undo
  .git/                             the home's history: one commit per decision on mine
                                    or machine, and per apply; never pushed
~/.config/git/ignore                git's global excludes (or core.excludesFile):
                                    a block marked "ohjaa apply" holds .sykli/, .teko/ and .ohjaa/
~/.zshrc                            the terminal's line: [[ -x <ohjaa> ]] && eval "$(<ohjaa> shell-init zsh)"
~/.claude/CLAUDE.md, ~/.codex/AGENTS.md, ~/.pi/agent/AGENTS.md,
~/.config/opencode/AGENTS.md        each that exists: a block marked "ohjaa:knowledge"
                                    holds ours, mine and machine (see "Keeping it true")
```

```bash
ohjaa apply --dry-run                  # every change, none made
ohjaa apply                            # the ignore block, ~/.zshrc, the agents' knowledge
                                       # blocks; then a commit in the home
ohjaa apply --undo                     # put back what apply changed; the history stays
ohjaa doctor                           # read-only; exits 4 on drift
```

`apply` runs the ohjaa on PATH (or `--bin`), by its absolute path, in the
terminal's line; another binary running it is refused (exit 3), so a `cargo
run` never points the line at itself. Every change is planned before any is
made, and each is recorded in `applied.toml` before it is made, so `--undo`
puts back exactly what was there, even after an apply that failed midway.
What the record holds that `--undo` does not take back stays in it, and
undo exits 4: it is never dropped unread. `apply --root <dir>` writes down
where the repositories are (`roots`), which `collect` and `lessons` read
wherever they run from; undo keeps it.

**The checks a commit must pass are sykli's**, from the repository's own
`sykli.json` and `sykli.lock`: `sykli run` proves HEAD, `sykli verify
--commit` says what proves a commit, and `sykli hook install --pre-push`
makes the repository's pre-push hook refuse a push to its default branch without a
passing receipt. Ohjaa runs no check, keeps no gate and installs no hook.

**Few git processes.** `apply` and `doctor` read git's global config in one
process (`git config --global --includes --list`) and no clone. The home's
history costs two per commit (`add -A` and `commit`, with git's automatic
maintenance off) and one more once, the home's `git init`: a decision on mine
or machine starts those beside the one that finds the repository it runs in,
and apply starts them when it changed something. The agents' instruction
files are read and written without git. `ohjaa collect` reads the config of
each of the fleet's clones under the roots once, at the same time, to find
the owners whose pull requests GitHub is asked about. `tests/config_home.rs` holds these budgets, and
`tests/terminal.rs` a decision's.

## Boundaries in full

AGENTS.md and `.toimija/repo.yaml` carry these boundaries in short; this is
each in full, with what ohjaa keeps where.

- One Rust crate producing a single `ohjaa` binary. Dependencies stay minimal —
  `anyhow`, `clap`, `serde`, `serde_json`, the vendored `faija-knowledge`
  (knowledge-node.v1), and `libc` (`flock(2)`, process groups for what ohjaa starts, the local time
  zone) — and every addition needs a reason. Tests
  alone also use `sha2` and `serde_yaml` (the parser toimija reads
  `.toimija/repo.yaml` with, so a config toimija cannot parse fails CI here).
- **Ohjaa holds Knowledge; toimija reports facts.** Never reimplement anything
  toimija derives — shell out to its CLI and consume its `--json`.
- **Ohjaa stores Knowledge.** Knowledge, as knowledge-node.v1 files in the
  scope directories plus proposals waiting for a person, is the truth and lives
  in git: ours in its repository, repo in the repository's own AGENTS.md (its
  `## Lessons`, one line per node, through a pull request ohjaa opens), mine
  and machine in the home's own history. Each accepted repo node is kept with
  its why, and where its line went, in `$XDG_STATE_HOME/ohjaa/lessons/<clone>/`
  (the nodes and `delivery.json`; `<clone>` the clone's common git directory),
  beside the worktrees a delivery writes in while it runs (`lessons/.work/`)
  and its lock (`lessons.lock`): knowledge, not a cache. Beside it,
  `$XDG_STATE_HOME/ohjaa` holds intake's cursor
  (`intake-since`), intake's progress (`intake-handled`: a version line; per
  candidate the drafter answered for or failed on, its time and a digest of
  its words, never the words, and for a failure how often and why in ohjaa's
  words, kept until the drafter answers for it; an end line with the count),
  the lock that lets one intake run at a time (`intake.lock`), what intake
  saw of each current node (`knowledge-evidence.json`: per node, each
  message a person re-taught it with, by its time, a digest of its words and
  its session's id, never the words, and when a draft last cited it; history,
  read by `ohjaa list`, `check` and `review`, never by
  a briefing), and the human view's
  folder (`human/`: `seen.json`, what the person last saw and when, the keys
  of what was said in the last two hours, where the next digest begins and
  where the earliest of what a day's digest left out begins, each with the
  keys of what GitHub said in the two hours before it; `collected.json`,
  the snapshot `ohjaa collect` writes of what `ohjaa line` cannot derive
  without git or the network, namely GitHub's pull requests and where and how often vessa's secrets scan found
  secret-shaped tokens in agent sessions (places and counts, never a
  value), each with when it was read and what it could not read; and two
  empty lock files, neither a cache: `seen.lock`, held while the cursor is
  read and moved, by the terminal saying the line and by `ohjaa digest`
  shown to a person, and `collect.lock`, held by the one collect that
  runs). Deleting them loses
  no knowledge: intake then reads its default window again, and sends once
  more what is still in it, the counts of re-teaching start again, and the
  human view starts from the last day.
  Everything else — fleet, perimeter, coverage — is derived now and stored
  never, but in the human view's snapshot: a copy that is said with its age
  wherever it is shown, read only by `ohjaa line`, `ohjaa digest` and `ohjaa
  collect` (which keeps from it, as of then, what a source held when that
  cannot be read again), never by a briefing. No knowledge index and no daemon; that snapshot is the one
  cache.
- **The workflow lives in the home, outside every repository.**
  `~/.config/ohjaa` holds the knowledge scopes, `roots`, and apply's record.
  The home is also a local git repository, its history: ohjaa makes it with
  its first commit (or uses the one the home already is), never gives it a
  remote and never pushes it, and commits there, with the person's git
  identity, each decision on mine or machine knowledge and each `apply` and
  `apply --undo` (`git add -A`, then `git commit`), each under the home's
  lock (`.git/ohjaa.lock`) and holding only what ohjaa keeps there (an
  allowlist in the repository's own `.git/info/exclude`); a commit that
  fails is said, and the change stands. Outside the home, `ohjaa apply`
  changes only a marked block in git's global excludes file, one line of
  `~/.zshrc`, and a marked block of the team's, mine and machine knowledge
  (`ohjaa:knowledge`) in each agent's own global instruction file that
  already exists outside every repository (Claude Code's
  `~/.claude/CLAUDE.md`, Codex's `~/.codex/AGENTS.md`, pi's
  `~/.pi/agent/AGENTS.md`, opencode's `~/.config/opencode/AGENTS.md`, each
  where the agent finds it; none is created, no text around the block is
  touched, and a block apply did not write is the person's and stays),
  records each before making it, and `apply --undo` puts them back; each
  decision rewrites that block where it is, and nowhere else. Nothing is
  installed on or written into a repository but, while repo knowledge is
  delivered, a worktree off the remote's branch
  (`$XDG_STATE_HOME/ohjaa/lessons/.work/`), removed once its branch is
  pushed. No file, hook or config of a repository's checkout is written;
  `ohjaa lessons publish` takes out of it the `.ohjaa/knowledge/` files a
  person accepted before, once they are in a pull request.
- **Ohjaa checks nothing and refuses nothing for the workflow.** The checks
  a commit must pass, and the refusal of a push without them, are sykli's,
  from the repository's own contract and hooks.
- **Ohjaa runs no sessions.** Agents start as they are typed and read the
  Knowledge from their own instruction files; the note for the next agent is
  `toimija handover`. Agent token usage is vessa's.
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
  Knowledge is not a message — it carries its time, its why and whose it is. The human view (`ohjaa
  line`, `ohjaa digest`) is a record a person reads at their own terminal,
  never a notification.
- **Network only through `gh`, the drafter and, to deliver repo knowledge,
  git to GitHub with gh's credentials** — in `ohjaa intake` (review
  comments, drafting), in `ohjaa collect` (one
  search for the pull requests of the owners of the fleet's clones under the
  roots,
  at most two calls;
  its one vessa call, for secrets, is local), and after a person's decision on repo knowledge and
  in `ohjaa lessons publish` (gh: the repository's visibility, the person's
  private repositories and name once a run, its open lessons pull request,
  `pr create`; git: one fetch and one push to the repository on GitHub, by
  HTTPS with gh's credentials; a dry run fetches and pushes nothing). Every other
  command works from the filesystem and local CLIs, and `ohjaa line` starts
  no git process either.
- Failed coverage is never rendered as an empty result. If a scan could not
  look, it says so and exits non-zero.


## Tests

`cargo test` runs against stand-ins for toimija, vessa, gh and the agents, in a sandboxed home, never the person's own. With `cargo fmt --check`
and `cargo clippy --all-targets -- -D warnings`, it is what CI runs. The same
three are the repository's `sykli.json` (with every input it reads, pinned in
`sykli.lock`), the contract `sykli run` runs on a person's machine; a second
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

## What it will never do

- **No messaging between agents, no request/response, no presence.** A message
  from an agent that has since died is a silent lie. Knowledge is not a
  message: it carries its time, its why and whose it is, and a person reviewed
  it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests.
