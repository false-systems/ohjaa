# ohjaa reference

This is the detail behind the [README](../README.md): what knowledge is, what
each command does, and where ohjaa keeps what. `ohjaa <command> --help` gives
the flags and exit codes of each command.

Contents:

- [Knowledge](#knowledge)
- [Commands](#commands)
- [Agents propose, a person decides](#agents-propose-a-person-decides)
- [Apply: the agents' own instruction files](#apply-the-agents-own-instruction-files)
- [The briefing](#the-briefing)
- [Repo knowledge: the repository's own AGENTS.md](#repo-knowledge-the-repositorys-own-agentsmd)
- [Check: keeping it true](#check-keeping-it-true)
- [Intake: corrections become proposals](#intake-corrections-become-proposals)
- [Does knowledge work? What a person had to teach again](#does-knowledge-work-what-a-person-had-to-teach-again)
- [The human view](#the-human-view)
- [The perimeter and coverage](#the-perimeter-and-coverage)
- [Boundaries in full](#boundaries-in-full)
- [Tests](#tests)
- [What it will never do](#what-it-will-never-do)

## Knowledge

Knowledge is what the people an agent works for know and have decided:
decisions, facts, taste and lessons. Each piece is a node, one per file, in
the [`knowledge-node.v1`](knowledge-node-v1.md)
format: TOML frontmatter, then the body an agent reads.

```
+++
id = "containers-run-in-podman"
kind = "fact"
status = "current"
as_of = 2026-11-10
origin = ["an agent tried docker twice and stalled"]
holds_while = ["command:podman"]
+++
Containers run in podman on this machine. There is no docker.
```

- `kind` is `decision`, `fact`, `taste` or `lesson`.
- `status` is `proposed`, `current`, `superseded` or `retired`.
- `origin` is why the node exists: the correction, decision or outcome
  behind it.
- `holds_while` names what must stay true for the node to hold:
  `command:<name>` (a command on PATH) or `path:<path>`.
- `refines` names a node this one narrows or builds on; `supersedes` names
  nodes of the same scope that it replaces.

A node's scope, meaning whose it is, is the directory it lives in:

| Scope | Lives in | Changes by |
|---|---|---|
| ours | `<shared checkout>/knowledge/`, set by `OHJAA_OURS` or the `ours` file in the home | pull request |
| repo | `<repo>/AGENTS.md`, its `## Lessons` section; ohjaa keeps each node, with its why, in its state folder | a pull request ohjaa opens |
| mine | `mine/` in the home | you |
| machine | `machine/` in the home | you |

The home is `$XDG_CONFIG_HOME/ohjaa`, by default `~/.config/ohjaa`. The state
folder is `$XDG_STATE_HOME/ohjaa`, by default `~/.local/state/ohjaa`.

Knowledge is ordered **ours → repo → mine → machine**, then **decision → fact
→ taste → lesson**. A personal node never supersedes a shared one, and shared
knowledge never builds on personal knowledge. Ohjaa defines the format and those rules in
[knowledge-node-v1.md](knowledge-node-v1.md). The reference implementation
is the crate `crates/knowledge-node`, and the golden fixtures are in
`fixtures/knowledge-node.v1/`. [ontology.md](ontology.md) says what the
concepts mean.

## Commands

| Command | What it does |
|---|---|
| `ohjaa apply` | Writes the knowledge into each agent's own instruction file, one line into `~/.zshrc`, and an ignore block into git's global excludes. `--dry-run`, `--undo`. |
| `ohjaa doctor` | Reports what differs from what `apply` would make it. Read-only. |
| `ohjaa propose` | Proposes a node. It waits until a person decides. |
| `ohjaa review` | Goes through the proposals, one at a time. A person only. `--list` only lists them. |
| `ohjaa accept <id>`, `reject <id>` | Decide one proposal. A person only. |
| `ohjaa retire <id> --why <why>` | Retires a node. It is kept, with its why. A person only. |
| `ohjaa confirm <id>` | Confirms that a node still holds, as of today. A person only. |
| `ohjaa list` | Lists the knowledge that applies here. `--all` includes superseded and retired nodes. |
| `ohjaa graph <id>` | Shows a node and its neighbours: what it builds on, what builds on it, what it replaced. |
| `ohjaa check` | Validates every node, flags what may no longer hold, and suggests what to reword or confirm. `--strict`, `--budget`. |
| `ohjaa lessons publish` | Delivers repo knowledge into each repository's own `AGENTS.md`, through a pull request. `--dry-run`. |
| `ohjaa intake` | Drafts proposals from what people said. |
| `ohjaa brief` | Prints the briefing for the repository it runs in. `--scope`, `--agent`, `--knowledge-only`, `--write`. |
| `ohjaa line` | For a terminal's opening: only what needs the person's attention. |
| `ohjaa digest` | The attention in full, and unread pull requests. `--since`, `--no-collect`. |
| `ohjaa collect` | Gathers the snapshot that `line` and `digest` read. |
| `ohjaa system` | Says what every prepared repository under the roots is. `--json`. |
| `ohjaa shell-init zsh` | Prints what a terminal runs as it opens. |

Every command takes `--root <path>`: where to look for repositories.

Exit codes mean the same everywhere: `0` done; `1` it could not run, or
`check` found errors; `2` wrong usage; `3` refused; `4` done in part, or
something could not be read. Ohjaa never shows a failed read as an empty
result.

## Agents propose, a person decides

```bash
ohjaa propose --scope ours --id code-is-rust --kind decision \
      --origin "said three times in one week: 'we do not use Python here'" \
      "Code is Rust."
ohjaa review
```

- `ohjaa propose` writes a proposal to `proposals/` in the state folder,
  **outside every repository**. An agent's draft never lands in a shared
  repository by itself. The body can come from stdin (`-`). `--repo <path>`
  names the repository of a repo proposal when it is not the current one.
- `ohjaa review`, `accept`, `reject`, `retire` and `confirm` refuse to run
  without an interactive terminal. Agents run without one.
- Accepting a node that supersedes another marks the old node `superseded`
  in the same step, and refuses outright if the result would not hold
  together. Nothing is deleted: retired and superseded knowledge keeps its
  time and its why.
- Shared knowledge is then reviewed in a pull request, like code. For shared
  scopes that review is the real gate. The person commits ours in its
  checkout. Repo knowledge goes into the repository's own `AGENTS.md` through
  a pull request ohjaa opens.

**How strong the person-only guard is.** It stops a careless agent. It does
not stop a determined one. A program can fake a terminal, so for personal
scopes the terminal check is a line an honest agent does not cross, not a
lock.

### Review

`ohjaa review` shows one numbered proposal at a time: a readable title, its
scope, the complete rule and its sources, and what accepting it does.
Original spacing and code indentation are kept. Replacements, refinements and
possible duplicates are shown before the choice.

- Type `a` to accept, `r` to dismiss, `s` or Enter to leave it for later,
  `d` for the original identifier and proposal file, or `q` to stop. Full
  action names and uppercase work too.
- Invalid input (`aq`, say) changes nothing and asks again. End of input
  stops without skipping the rest.
- Each decision is saved at once, with a short confirmation. A failure to
  write the history or an agent's instruction file is shown.
- The final summary counts the decisions and the proposals still waiting.
- `review --list` is read-only.
- Unreadable proposals or evidence make the review incomplete (exit 4),
  never an empty or complete queue.

### Personal knowledge has a history

Mine and machine are versioned in the home itself. The home is a local git
repository with no remote. Ohjaa makes it with its first commit, or uses the
repository the home already is, and never pushes it.

- Each decision (`accept`, `reject`, `retire`, `confirm`, and each one
  `review` makes) is one commit, with your git identity, saying what and
  why: `accept code-is-rust: Code is Rust.`, then the kind, the scope and
  each `why:` of its origin. A retirement carries its `--why`.
- `ohjaa apply` and `apply --undo` commit what they change in the home the
  same way.
- A commit that fails (another git holds the index, no identity) is said,
  and the decision stands. The next commit takes it in.
- `git -C ~/.config/ohjaa log` is the history of what you decided.

One change happens at a time. Each decision, with its write, its commit and
the rewrite of the agents' blocks, holds the home's lock (`.git/ohjaa.lock`),
and so does apply. Decisions made at once become one commit each, in turn.

- The home's git holds the lock for as long as it runs. An ohjaa killed while
  it commits leaves its git to finish, in a process group of its own, and the
  next change waits for that git.
- The home's git runs no hook, no fsmonitor, no automatic gc or maintenance,
  and no signing, so nothing it starts keeps the lock after it. A hook that
  git's config defines by name (`hook.<name>.command`) still runs under git
  2.54, which has no switch for all of them. Git gives it `/dev/null` for
  stdin, so it cannot hold the lock either.
- Only what ohjaa keeps there is committed: `mine/`, `machine/`, `roots`,
  `applied.toml`, `ours` and `drafter`, by an allowlist in the repository's
  own `.git/info/exclude`. The home holds no `.gitignore`. A `.DS_Store`, an
  editor's swap file or a file of your own beside them is never committed.
- The `drafter` file, a command line, is in the history too. Keep any key out
  of that line: give the drafter its key from your environment.

A home whose `.git` is a file (a worktree) keeps the lock in the folder that
file names. A linked worktree keeps the allowlist there too, as
`ohjaa.exclude`, which the home's `git add` is given as its excludes file.
That file takes the global excludes file's place for that one command, so it
holds your own excludes file's lines (`core.excludesFile`, else
`~/.config/git/ignore`), read for each commit, and then the allowlist. The
repository's shared `info/exclude` is still read above both.

## Apply: the agents' own instruction files

Ohjaa keeps what it writes for you in its home and applies it from outside.
Nothing is installed on a repository or written into a checkout.

```
~/.config/ohjaa/                    the home ($XDG_CONFIG_HOME/ohjaa)
  mine/ machine/ ours drafter       knowledge and settings
  roots                             where ohjaa looks for repositories (apply --root writes it)
  applied.toml                      what apply changed outside the home, for --undo
  .git/                             the home's history; never pushed
~/.config/git/ignore                git's global excludes (or core.excludesFile):
                                    a block marked "ohjaa apply" holding .sykli/ and .ohjaa/
~/.zshrc                            one line: [[ -x <ohjaa> ]] && eval "$(<ohjaa> shell-init zsh)"
each agent's own instruction file   a block marked "ohjaa:knowledge"
```

```bash
ohjaa apply --dry-run     # every change, none made
ohjaa apply               # make them, then commit in the home
ohjaa apply --undo        # put back what apply changed; the history stays
ohjaa doctor              # read-only; exits 4 on drift
```

`apply` makes three changes outside the home, and nothing else:

- **The knowledge block.** Ours, mine and machine go into each agent's own
  global instruction file, from one source, the nodes.
- **The terminal's line.** One line in `~/.zshrc` runs `ohjaa line` as a
  terminal opens. zsh is the only shell.
- **The ignore block.** A marked block in git's global excludes file ignores
  `.sykli/` (sykli's receipts) and `.ohjaa/` (lessons that sit in a
  checkout), in every repository.

Every change is planned before any is made, and each is recorded in
`applied.toml` before it is made. So `--undo` puts back exactly what was
there, a missing final newline included, even after an apply that failed
midway. What the record holds that `--undo` cannot take back stays in it,
and undo exits 4.

`apply` writes the absolute path of the ohjaa on PATH (or of `--bin`) into
the terminal's line. An ohjaa that is not the one on PATH refuses to apply
(exit 3), so a development build never points the line at itself.

`apply --root <dir>` writes down where the repositories are (`roots`), which
`collect` and `lessons` read wherever they run from. Undo keeps it.

### Where each agent's block goes

Every agent reads its own global instruction file wherever it starts. Codex
and opencode stop at a repository's root, so a file in the folder above your
repositories does not reach them. `ohjaa apply` writes the block into each of
these that exists, and creates none:

| Agent | Its global instruction file (the first that exists) |
|---|---|
| Claude Code | `$CLAUDE_CONFIG_DIR` (else `~/.claude`)`/CLAUDE.md` |
| Codex | `$CODEX_HOME` (else `~/.codex`)`/AGENTS.override.md`, `AGENTS.md` |
| pi | `$PI_CODING_AGENT_DIR` (else `~/.pi/agent`)`/AGENTS.override.md`, `AGENTS.md`, `CLAUDE.md` |
| opencode | `$OPENCODE_CONFIG_DIR` (else `$XDG_CONFIG_HOME/opencode`, else `~/.config/opencode`)`/AGENTS.md` |

An agent that is set up but has no such file is named: create the file, even
empty, and apply again. A file inside a git repository gets no block: one
person's knowledge never goes into a file a repository shares. A bare
dotfiles repository (`git --git-dir=<dir> --work-tree=~`) has no `.git` above
the file, so ohjaa cannot see that the file is in it. Put a block of your own
in such a file and ohjaa leaves it alone.

The block is one line per current node, `kind: text`, in knowledge order,
without origins:

```
<!-- ohjaa:knowledge begin: mine and machine, written by `ohjaa apply` and after each decision, … -->
Knowledge (ohjaa): what your team and the person you work for have decided and know. …
- decision: Code is Rust.
- fact: Containers run in podman on this machine. There is no docker.
<!-- ohjaa:knowledge end -->
```

- Past 8,000 characters the block keeps every decision in full, then taste,
  lesson and fact while they fit, then counts the rest
  (``30 more: `ohjaa list` ``). Apply says so.
- After each decision the block is rewritten where it is, and only there. A
  block you take out stays out until the next apply.
- Apply records every file it writes a block into. Undo takes back each one,
  whatever the agents' variables say when undo runs.
- A block apply did not write (one from `brief --write`, or your own) is
  yours. Apply, each decision and undo leave it as it is and say so. A file
  holds one block.
- A file apply cannot read as a whole (two blocks, a marker without its
  partner) is named and skipped. The rest is applied, and apply exits 4. Every
  later apply, dry run and `ohjaa doctor` names the file again until you fix
  it by hand.

## The briefing

`ohjaa brief` prints the knowledge and context for the repository it runs
in. It is what an agent gets when it asks, and what `ohjaa check` measures.

```bash
ohjaa brief                          # this repository
ohjaa brief --scope ../other-repo    # and the knowledge and context of another
ohjaa brief --agent codex            # leave out what Codex already reads
ohjaa brief --knowledge-only         # without the live perimeter
ohjaa brief --write notes/AGENTS.md  # write the knowledge as a block into a file
```

- Global knowledge is delivered once, not repeated for each repository.
- Every repository named by `--scope` adds its repo knowledge, all of it,
  labelled with its path, within a budget of its own. A scope repository
  that is missing makes `brief` exit 4, never an empty success.
- `--write <file>` writes the knowledge as a generated block into an
  instruction file such as `CLAUDE.md` or `AGENTS.md`, and leaves everything
  else in the file alone.
- No keyword matcher drops a decision.

### The budget

A briefing longer than an agent reliably reads (8,000 characters by default)
degrades, and says so on stderr. It keeps every decision in full, then the
rest by priority while it fits: taste, then lesson, then fact, each with the
most recently confirmed first. The filling stops at the first node that does
not fit, and the briefing ends with the count of the rest
(``3 more: `ohjaa list` ``). No node is cut short. `ohjaa check` still
measures all of it. The fix is to merge or retire nodes.

### What an agent already reads is not repeated

`ohjaa brief --agent <agent>` reads that agent's own global instruction file,
found where the agent finds it, and compares its block with the block apply
would write now, byte for byte. When the two are the same, the briefing
leaves out what the block holds and says where it is:

```
The team's, mine and machine (41 nodes) are not repeated here: Claude Code has them from its own instructions, /home/you/.claude/CLAUDE.md, whose ohjaa block is current.
```

A block over its own budget names some nodes only by count. The briefing
carries those, and the line says so. A flagged node is repeated with its
flag, since the block carries no flags.

The briefing carries all of ours, mine and machine, and its one line says
why, when:

- the file is missing, holds no block, holds a block apply did not write,
  holds a stale one, or cannot be read;
- the folder the file is in is not an absolute path (a relative
  `CLAUDE_CONFIG_DIR`, say): the agent resolves it where it starts, so ohjaa
  cannot tell which file it reads;
- the agent's own variables say it will not read the file:
  - Claude Code 2.1.311 with `CLAUDE_CODE_SIMPLE` or `CLAUDE_CODE_SAFE_MODE`
    set truthy, `CLAUDE_CODE_DISABLE_CLAUDE_MDS` set at all, or an empty
    `CLAUDE_CONFIG_DIR`;
  - opencode 2.1.4 leaves its instructions to its background service unless
    it runs `--standalone`. That service reads them with the environment it
    started with, so when `OPENCODE_CONFIG_DIR` or `XDG_CONFIG_HOME` is set
    it may read another file than the one ohjaa checked;
  - Codex 0.164.0 and pi 1.2.0 have no variable that skips the file.

`ohjaa brief --agent` knows the environment, not the arguments the agent is
started with. An agent started with an option that skips its instruction
files (`claude --bare`, `pi --no-context-files`) gets no block, and ohjaa
cannot see that. For any command but `claude`, `codex`, `pi` and `opencode`
nothing is said.

## Repo knowledge: the repository's own AGENTS.md

Every agent reads a repository's `AGENTS.md`, so that is where repo knowledge
goes. When you accept a repo proposal, ohjaa writes its line into the
`## Lessons` section of that repository's `AGENTS.md`, through a pull request
it opens. You merge it like any change. Nothing is written into your
checkout.

**The node** is kept in ohjaa's state, with its why:
`lessons/<clone>/<id>.md`, where `<clone>` is the clone's common git
directory, so every worktree of it shares them. Beside them, `delivery.json`
(`ohjaa-lessons.v1`) says where each line is: `pr` (its URL and branch),
`merged`, `undelivered` or `held`, each with why. This is knowledge, not a
cache. Deleting it loses what is not delivered yet, and the why behind what
is.

**The line**, one per node, holds its body on one line, marked with its id:

```
## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, … -->

- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-27)
```

- A line is replaced by its id wherever it is. A confirmation moves its
  date. A second line of the same id goes.
- A superseded or retired node's line is taken out.
- A new line goes after the last of the section's. The section is added at
  the end of the file when there is none, and `AGENTS.md` is made when there
  is none.
- Your own lines, and a line of an id ohjaa does not know, stay as they are.
- Writing the same nodes again changes nothing.
- What ohjaa writes into a repository names no tool: not in its marks, its
  words, its branch, its commit or its pull request.
- `AGENTS.md` is read whole, from the branch as fetched. A file git cannot
  read, or that is not UTF-8, is never written over, and the delivery says
  why.

**The pull request** is made from the clone, never in your checkout:

1. `gh repo view` reads the repository's visibility and default branch.
2. `gh pr list --author @me` finds your open pull request of the lessons, on
   a branch `agents-md-lessons/…`. It gets the next commit on its tip, never
   a force-push.
3. `git fetch` of that branch, then a worktree off it in `lessons/.work/` in
   the state folder.
4. `git commit`, with your identity and the repository's own hooks.
5. `git push` to `refs/heads/agents-md-lessons/<date>-<n>`, then
   `gh pr create`. The worktree is removed however it ends.

Git reaches the repository on GitHub by HTTPS, with gh's credentials, so no
SSH key's prompt holds it up. Your `insteadOf` rules still apply. The origin
must be on GitHub. One delivery runs at a time (`lessons/lessons.lock`), and
it first clears what a delivery that did not end left in `lessons/.work/`.

Before the push, the commit must change `AGENTS.md` alone, and hold exactly
the text ohjaa wrote and checked. A hook that changed either stops the
delivery. A hook that refuses is quoted in its own words. `retire` and
`confirm` of a repo node deliver the same way.

**When a line cannot be delivered** (no remote named origin, an origin not on
GitHub, no gh, gh signed out, a fetch or push that fails), the decision
stands. `delivery.json` keeps why, `ohjaa list` shows it under the node, the
briefing marks it, and `ohjaa lessons publish` delivers it once that is
fixed. It is never dropped.

### A repository that is not private

A public or internal repository gets nothing that names what is private.
Everything it would be sent is checked first against a deny list built from
your own context:

- the name of every repository you can see that is not public, asked of gh
  once a run. A list with no private repository in it, or one that lacks a
  repository under your roots that gh does not show as public, is not
  complete, and nothing is sent;
- the name of every repository under your roots that gh does not show as
  public, by its folder and by its remote;
- your home path, your home folder's name, and your GitHub login and name;
- always `ohjaa`, this tool's own name.

A line is read before it is matched. Entities are decoded, invisible
characters taken out, fullwidth forms and Latin letters with diacritics
folded to ASCII (`café` reads `cafe`), and HTML comments and Markdown's `*`,
`_`, `` ` `` and `~` taken out, so a split name joins (`wid<!-- -->get`,
`**wid**get`).

- A name matches at the start of a word, whatever follows (`widgetd`,
  `widgets`, `WidgetClient`), in any letter case. The home path and `ohjaa`
  match anywhere.
- A letter outside ASCII that is left after folding, or an entity that
  cannot be read, holds the line too: it could stand for a letter of a name.
- The repository's own name and owner are no secret in it.

A line that names something on the list is held back and named, and its text
goes nowhere. At `accept`, the accept is refused and the proposal stays
waiting. At `publish`, the node waits where it is. The rest of what would be
sent (the section's heading and intro, the commit message, the pull
request's branch, title and body) is checked too, and a match there stops the
whole delivery. A false match only holds a line back, to be reworded. No why
is ever sent to a repository that is not private.

### What the briefing does with repo knowledge

The briefing reads the repository's `AGENTS.md` as checked out. A line inside
a code fence or an HTML comment does not count. The nodes whose lines the
file holds, as they are now, are left to the file, **only for an agent shown
to read that file**, and one line says so:

```
This repository's lessons (14 nodes) are not repeated here: Codex reads its AGENTS.md (/home/you/projects/widget/AGENTS.md), which holds them in its ## Lessons section (its lessons end within the 32768 bytes of project instructions Codex reads).
```

Any other agent gets them carried, and the line says why. A flagged node is
always carried. A node whose line in the file differs from the node (older,
or edited by hand) is carried too, marked `AGENTS.md holds another line of
it`.

| Agent | Left to AGENTS.md when | Carried when |
|---|---|---|
| Claude Code 2.1.311 | a `CLAUDE.md`, `.claude/CLAUDE.md` or `CLAUDE.local.md` at the root, or between it and the launch folder, imports the root's `AGENTS.md` (`@AGENTS.md` in its text, outside frontmatter, code, HTML comments and HTML blocks), **and** it starts at the root or has approved imports from outside the launch folder; or its settings turn on the built-in plugin that reads `AGENTS.md` | otherwise: no `CLAUDE.md`, a prose mention only, a launch in a subfolder without that approval, or a variable or setting that skips project instructions |
| Codex 0.164.0 | always, within its cap | an `AGENTS.override.md` at the root; the last lesson's line ends past `project_doc_max_bytes` (32768 by default) |
| pi 1.2.0 | always | an `AGENTS.override.md` at the root |
| opencode 2.1.4 | always | `OPENCODE_DISABLE_PROJECT_CONFIG` or `OPENCODE_CONFIG_PROJECT_DISABLE` is set |
| any other, or `brief` without `--agent` | never | always |

Until a line is on the checked-out commit, the briefing carries the node,
marked beside its id: `pending PR <url>`, `not delivered: <why>`, `held back
from AGENTS.md: <why>` or `not yet in AGENTS.md`. `ohjaa list` says, under
each current repo node, where it is.

A line the checked-out `AGENTS.md` still holds of a retired or superseded
node, while its removal waits, is named for every agent:

```
AGENTS.md still says `X`, but it was retired on 2026-10-28 (the widget is gone); its removal is pending in https://github.com/acme/widget/pull/41: do not follow it.
```

### Publish

`ohjaa lessons publish [<repo>…]` delivers every accepted repo node that is
not delivered yet, one pull request per repository. It also takes node files
that sit untracked in `<repo>/.ohjaa/knowledge/`: once the pull request is
open, each file moves out of the checkout into ohjaa's state, and an emptied
`.ohjaa/` is removed. A held node stays where it is.

Without a repository named, it takes each repository under the roots that
has something to deliver. `--dry-run` says, per repository, its visibility,
each line it would add and what a line would name, and fetches, writes and
sends nothing. Publish exits 4 while anything is not delivered.

## Check: keeping it true

`ohjaa check` validates every node and the rules across them. It also flags
what may no longer hold:

- a `holds_while` predicate that is false right now;
- a **fact** not confirmed for 90 days (`ohjaa confirm <id>`);
- a node whose text names, in backticks, an `ohjaa` command that this ohjaa
  does not have: the rule asks for what no agent can do;
- a briefing over the budget (`--budget`, 8,000 characters by default).

A flag never hides a node. The briefing shows the node with the flag beside
it. `check --strict` exits non-zero on flags too, not only on errors.

Check also says, per current node, how often a person had to teach it again,
and suggests what to reword or confirm. Suggestions are never flags. Before
those counts it says up to when intake has read what people said, or that
intake never ran here: a count from an intake that has not read is unknown,
not zero.

## Intake: corrections become proposals

```bash
ohjaa intake                                  # from where the last run stopped
ohjaa intake --since 2026-04-01 --max-batches 50
ohjaa intake --source transcripts --dry-run   # show the prompt, send nothing
ohjaa intake --no-prefilter                   # every message, not only likely corrections
```

`ohjaa intake` reads three sources:

- **Transcripts:** what people wrote in agent sessions, through
  `vessa messages --since <cursor> --json` (`vessa-messages.v1`). Intake
  parses no transcript itself, and has no fallback. Each message travels
  with the end of the agent's reply before it (600 characters), how it was
  said (typed mid-turn, an answer to the agent's question, a reason for
  refusing a tool call, a launch's task) and its session. A message longer
  than 1,500 characters is a pasted brief: counted, never sent.
- **Reviews:** human comments on pull requests and issues, through `gh`.
- **Reverts:** reverted commits on each repository's current branch.

### The pre-filter

A local pre-filter comes first, with no network. Most of what people write
to agents is the work, not a correction. Of what vessa lists, only this is
sent: what a fixed set of phrase rules takes for a correction, the same
words said more than once among the messages a run reads, and every reason a
person gave for refusing a tool call.

The filter misses corrections, so it is never silent:

- Every run says how many messages vessa listed, how many were briefs, and
  how many the filter kept (by rule) and dropped (by agent). It gives
  counts, never the words.
- `--no-prefilter` sends every message.
- A dropped message is not handled, and the filter's verdict is not kept.
  The message comes back only when a run reads that stretch again with
  `--since`.
- Pull request comments and reverts are not filtered.

### Secrets are redacted, as a best effort

People paste credentials to agents. As each prompt is made, everything in it
but ohjaa's own instructions is scrubbed, from every source, with the filter
on or off. A token in one of these shapes becomes `[redacted]`, and is
counted:

- a known key prefix: `sk-`, `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`,
  `github_pat_`, `xoxb-`/`xoxp-`/`xoxa-`/`xoxs-`, `AKIA`/`ASIA`, `glpat-`,
  `AIza`;
- a JWT, or a PEM private key block;
- an id and a secret joined by a dot (`<hex, 24+>.<letters and digits, 12+>`);
- a token of 32 or more characters with two or more each of capitals, lower
  case letters and digits, unless it is hex (a hash or a UUID);
- a long mixed token right after a word that names a secret (`api_key=…`,
  `GITHUB_TOKEN=…`, `password: …`, `Authorization: Bearer …`).

This is not a guarantee. A short password, a key with no digits, or a key in
a shape not listed gets through. Long random ids that are no secret are
redacted too. The original is never shown, logged or kept. Each run says
only how many tokens were redacted, and a dry run shows the scrubbed prompt.

### The drafter

Intake sends candidates in batches to a **drafter**: an agent CLI that reads
a prompt on stdin and prints one JSON object. The default is `claude -p`.
Override it with `--drafter` or the `drafter` file in the home.

The drafter sees what is already present where the batch comes from: the
current knowledge, then the proposals waiting for a person, of ours, mine
and machine, and of each repository the batch's candidates come from. Each
node is shown by its id, scope, kind and status and the first 100 characters
of its body, in at most 24,000 characters. What does not fit is counted.

- The drafter returns nothing for a lesson that is already present.
- When a candidate adds to a present node, the draft names that node in
  `refines`.
- When a person corrects an agent on what a current node already says, the
  drafter names that node in `reteaches` and drafts nothing.

Every draft is validated like any proposal. An invalid one is refused, and
the run says so.

### Possible duplicates

Every draft costs a person a review, so the drafter's answer is checked, and
nothing in it is dropped. A draft whose title says mostly what the title of
a present node of its own scope says is written like any other proposal, and
marked a *possible duplicate* of that node. Present means current, or
proposed and waiting.

- A node's title is its id: its words, lowercased, without filler (`a`,
  `the`, `and`, `with`, …), each with one common ending cut (`-ing`, `-ed`,
  `-es`, `-s`, `-e`).
- Two titles are near when they share at least two words, and the shared
  words are at least 60% of both (Dice's coefficient, 2·shared / (a + b) ≥
  0.6).
- The node a draft refines, and those it supersedes, are not compared.

Nothing of this is stored. `ohjaa review` works it out again for every
waiting proposal and shows a possible duplicate right after the one it may
repeat. A near draft is named, never refused: two distinct lessons can have
near titles.

### Progress

A run drafts from at most `--batch` × `--max-batches` candidates (40 × 10 by
default), oldest first, and never from one the drafter has already answered
for.

- **Handled candidates are kept.** As soon as the drafter answers for a
  batch, its candidates go into `intake-handled` in the state folder: a time
  and a digest each, never the words. An empty or cut-short file is an
  error, never read as less.
- **A refused batch is split.** When the drafter fails on a batch, the batch
  is split in halves, down to the candidate it fails on. After failing alone
  three times, that candidate is set aside and named by its time and source
  in every run after. `--retry-failed` sends every set-aside candidate
  again.
- **A run stops** when it gets no answer at all, or when two candidates fail
  alone in a row. What came before is kept.
- **Exit codes.** A run exits 4 when it leaves work for the next run, sets a
  candidate aside, or could not read a source. Once nothing is left and every
  source was read, it says `nothing new to draft from` and exits 0. Only one
  intake runs at a time: a second says `intake already running` and exits 3.

### The cursor

One cursor covers every source (`intake-since`). It moves only after a run
that read all three sources with no warning, and then only past candidates
that were handled. It never moves back.

- A run with `--source`, or with a warning, leaves the cursor where it is
  and says so.
- With no repositories under the roots, reviews and reverts warn and hold
  the cursor: nothing read is not nothing there.
- A repository whose current branch has no commits has nothing to read. It
  is empty, not unread, and holds nothing. A branch whose ref is lost warns
  and holds the cursor.
- When vessa lists only part (its exit 4), intake drafts from what it lists.
  A source vessa could not read is a warning and holds the cursor. So is a
  vessa missing from PATH, or one that cannot list messages. A store vessa
  knows of and never reads is named on every run and holds nothing.
- A first run keeps the start of its window, fourteen days back, as the
  cursor before it drafts.
- A `--since` before the cursor reads that stretch again.

## Does knowledge work? What a person had to teach again

A node the person keeps re-teaching is not working: it needs rewording, or
making a decision. A node nothing touches may no longer hold. Intake records
both.

The drafter's answer is one JSON object:

```json
{"drafts": [{"scope": "mine", "repo": null, "id": "…", "kind": "lesson",
             "body": "…", "origin": ["…"], "refines": null, "supersedes": []}],
 "reteaches": [{"candidate": 2, "ids": ["code-is-rust"]}]}
```

`candidate` is the number the prompt gives a candidate, and `ids` the
current nodes it corrects an agent against. A message that only re-teaches
makes no draft. A bare array of drafts is read too, as re-teaching nothing.

**The answer is checked.** A re-teaching is kept only when it names a
candidate of the batch that is a person's words (never a revert), and a
current node the drafter was shown with that candidate. Everything else is
dropped and counted by why on every run. A run names each re-teaching it
records by node, time and source, never the words.

**It is kept as history** in `knowledge-evidence.json` in the state folder
(`ohjaa-knowledge-evidence.v1`). Per node it holds each message that
re-taught it (its time, a digest and its session's id, never its words) and
the newest time a draft cited it. A message counts once per node however
often the drafter answers for it. Deleting the file loses no knowledge, only
the counts. A damaged file stops intake before anything is sent, and is
never replaced. `ohjaa list`, `check` and `review` then say so, and show no
counts instead of zeros.

**It is shown, and only suggested.** A count is since the node's `as_of`,
when a person last accepted or confirmed it.

- `ohjaa list`, under each current node: `re-taught 3 times since
  2026-09-20, last 2026-09-23`.
- `ohjaa check`: the same for each current node (`evidence <id>: …`), then a
  suggestion where one applies (`suggest  <id>: …`):
  - re-taught 3 or more times in the last 30 days: `reword it, or make it a
    decision`;
  - nothing re-taught, cited, accepted or confirmed it in 60 days: `still
    true?`
- `ohjaa review`, when a proposal may repeat a current node or refines one:
  that node's count, under it.

Ohjaa never changes a node itself. Rewording, deciding, confirming and
retiring stay a person's.

## The human view

```bash
ohjaa line                # at terminal startup: attention only, otherwise silent
ohjaa digest              # attention in full, and unread pull requests
ohjaa digest --since 7d   # a window instead; nothing is marked seen
ohjaa collect             # gather the snapshot that line and digest read
```

The human view only records. Nothing is sent to anyone, decided or refused.

- **`ohjaa line`** shows only what needs attention or a decision: a source
  that could not be read, possible secrets in agent sessions, and proposals
  waiting for you. The first terminal of a local day shows up to 15 lines.
  Later terminals show a short notice only when the attention changes. With
  none it is silent. Silence is not a claim that everything is healthy.
  Terminals opened together say a notice once. It always exits 0.
- **`ohjaa digest`** shows the attention in full; the pull requests merged,
  opened and closed since you last read a digest; one waiting proposal, in
  rotation; and "Now": who toimija says is working on this machine, then the
  open pull requests of the owners of your repositories, the last updated
  first, at most 20 listed and the rest counted. That is where teammates'
  work shows. Reading a digest at a terminal marks the activity seen. Piping
  it, `--since`, or running it inside an agent session does not.
- **Only a person sees the line.** `line` prints, reads, writes and starts
  nothing unless stdout is a terminal and no agent's marker is set
  (`ohjaa line --help` lists the markers). An agent's shell reads `~/.zshrc`
  too.
- **`line` starts no git and touches no network.** It reads the proposals
  itself and everything else from the snapshot `ohjaa collect` writes, shown
  with its age. When the snapshot is older than 15 minutes, `line` starts
  one collect in the background. `ohjaa digest` collects first when the
  snapshot is more than 5 minutes old, and waits at most 10 seconds.
  `--no-collect` uses the snapshot as it is.
- **`ohjaa collect`** asks two sources once, at the same time. A source that
  fails keeps what it held before, shown as of then.
  - GitHub: one `gh api graphql` search, at most 2 calls, for the pull
    requests updated in the owners of the prepared repositories under the
    roots (those with `.toimija/repo.yaml`). It reads at least a day back
    and at most a week.
  - Secrets: one `vessa secrets --json` scan (`vessa-secrets.v1`) since the
    last complete scan. Only locations, rules, counts and SHA-256
    fingerprint prefixes are kept, never a value. One of this machine's own
    credentials is an alarm. Broad pattern matches are evidence in the
    digest, not startup attention.
- **What could not be read is said, never an all-clear.** A source that did
  not answer, or could not read all it asked, is shown with the age of what
  it holds, and `ohjaa collect` and `ohjaa digest` exit 4.

Lines wrap at 100 characters, except URLs. In text from outside (a title, a
tool's error), every control character and bidi control is replaced with
U+FFFD before it reaches the terminal.

The view's state is in `human/` in the state folder: `seen.json` (what the
person last saw, and where unread activity begins), `collected.json` (the
snapshot), and two lock files. Deleting them loses no knowledge. The view
then starts from the last day.

## The perimeter and coverage

`ohjaa system` says what every prepared repository under the roots is, from
each repository's own `.toimija/repo.yaml`. It is derived when asked and
never stored, so it cannot be staler than the moment it was computed. Who is
working in a repository comes from `toimija sessions`.

The roots are the sibling directories of your git root (outside a
repository, of the folder you are in), unless the `roots` file in the home
or `--root` says otherwise. A root that does not exist is an error, never an
empty list.

A scan that failed is not a scan that found nothing:

```
coverage: scanned 17/18 repo(s)
  incomplete — absence here is not evidence of absence:
    somerepo: not a Git repository
```

- `ohjaa system` exits 4 when coverage is incomplete.
- A session toimija reports as `stale` stays visible, with its age: "quiet
  for ninety minutes" and "gone" look the same from outside.

## Boundaries in full

`AGENTS.md` and `.toimija/repo.yaml` carry these boundaries in short. This
is each in full, with what ohjaa keeps where.

- **One binary.** The `ohjaa` crate produces a single `ohjaa` binary. The
  knowledge-node format has a crate of its own beside it,
  `crates/knowledge-node`, which depends only on `toml`. Ohjaa's other
  dependencies are `anyhow`, `clap`, `serde`, `serde_json` and `libc` (file
  locks, process groups, the local time zone). Tests alone also use
  `serde_yaml`. Every addition needs a reason.
- **Ohjaa holds knowledge; toimija reports facts.** Ohjaa never
  reimplements what toimija derives. It runs toimija's CLI and reads its
  `--json`.
- **Ohjaa stores knowledge, in git.** Ours is in its repository. Repo
  knowledge is in the repository's own `AGENTS.md`. Mine and machine are in
  the home's own history. Everything else is derived when asked. The human
  view's snapshot is the one cache, and it is shown with its age wherever it
  appears. There is no knowledge index and no daemon.
- **The workflow lives in the home, outside every repository.** Outside the
  home, `ohjaa apply` changes only a marked block in git's global excludes
  file, one line of `~/.zshrc`, and a marked block in each agent's own
  global instruction file that already exists outside every repository. No
  file, hook or config of a repository's checkout is written. While repo
  knowledge is delivered, ohjaa uses a worktree in its own state folder and
  removes it after the push.
- **Ohjaa checks nothing and refuses nothing for the workflow.** The checks
  a commit must pass, and refusing a push without them, are sykli's: `sykli
  run` proves a commit from the repository's own `sykli.json`, and `sykli
  hook install --pre-push` installs the hook that refuses. Ohjaa runs no
  check and installs no hook.
- **Ohjaa runs no sessions.** Agents start as they are typed and read the
  knowledge from their own instruction files. The note for the next agent is
  `toimija handover`. What agents used is vessa's.
- **Agents propose; only a person makes knowledge current**, retires it or
  rejects it. What ohjaa writes into a repository names no tool, and a
  repository that is not private is sent nothing that names what is.
- **Cross-repo yes, messaging no.** There is no request and response between
  agents, no presence and no dispatch. The human view is a record a person
  reads at their own terminal, never a notification.
- **Network only through `gh`, the drafter, and git to GitHub to deliver
  repo knowledge.** That is `ohjaa intake` (review comments, drafting),
  `ohjaa collect` (one search for pull requests), and a person's decision on
  repo knowledge and `ohjaa lessons publish` (one fetch and one push by
  HTTPS, with gh's credentials). Every other command works from the
  filesystem and local CLIs.
- **Failed coverage is never shown as an empty result.** If a scan could
  not look, it says so and exits non-zero.

### What the state folder holds

Everything is under `$XDG_STATE_HOME/ohjaa`:

| Path | Holds | If deleted |
|---|---|---|
| `proposals/` | proposals waiting for a person | the proposals are lost |
| `lessons/<clone>/` | each accepted repo node with its why, and `delivery.json` | what is not delivered yet is lost, and the why of the rest |
| `lessons/.work/`, `lessons/lessons.lock` | the worktree of a delivery while it runs, and its lock | nothing |
| `intake-since` | intake's cursor | intake reads its default window again |
| `intake-handled` | per candidate the drafter answered for or failed on: its time and a digest, never the words | intake sends again what is still in its window |
| `intake.lock` | the lock that lets one intake run at a time | nothing |
| `knowledge-evidence.json` | per node, each re-teaching and the last citation | the counts start again |
| `human/` | `seen.json`, `collected.json` and two locks | the view starts from the last day |

### Few git processes

Git is slow to start, so ohjaa counts the git processes it starts.

- `apply` and `doctor` read git's global config in one process and no clone.
- The home's history costs two per commit (`add -A` and `commit`), and one
  more once, the home's `git init`.
- The agents' instruction files are read and written without git.
- `ohjaa collect` reads the config of each prepared repository under the
  roots once, at the same time.
- `ohjaa line` starts none.

`tests/config_home.rs` and `tests/terminal.rs` hold these budgets.

## Tests

`cargo test` runs against stand-ins for toimija, vessa, gh and the agents,
in a sandboxed home, never the person's own. With `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings`, it is what CI runs.

The same three are the repository's `sykli.json`, the contract `sykli run`
runs on a person's machine, with every input file it reads listed and pinned
in `sykli.lock`. A new source or test file goes into each task's inputs,
followed by `sykli lock sykli.json`, or `sykli run` refuses.

| Suite | What it sandboxes |
|---|---|
| `tests/cli.rs` | the commands, against stand-in tools and a home in a temporary folder |
| `tests/config_home.rs` | apply, undo and doctor, in a temporary `HOME`; the git process budgets |
| `tests/terminal.rs` | a person's decisions, on a pty of the test's own |
| `tests/lessons.rs` | delivery of repo knowledge, to a local bare repository as the remote: git's `insteadOf` turns the GitHub URL into it, and `GIT_ALLOW_PROTOCOL=file` keeps git off the network should that fail |
| `tests/instructions.rs` | that `CLAUDE.md` imports `AGENTS.md`, and that both stay small |
| `tests/repo_config.rs` | that toimija can parse `.toimija/repo.yaml`, and that git ignores toimija's working files |
| `crates/knowledge-node/tests/conformance.rs` | nothing: it runs the golden fixtures in `fixtures/knowledge-node.v1/` through the format's crate, and pins their bytes by a digest |

A test writes a stand-in, or any file it then runs, only through its crate's
`write_executable`, which has `cp` write it. Linux refuses to run a file
that any process holds open for writing (`Text file busy`). A child that
another test thread forks holds a copy of every descriptor the test process
has open until that child execs. So a file the test process wrote itself can
still be open for writing in such a child at the moment it runs.

## What it will never do

- **No messaging between agents, no request and response, no presence.** A
  message from an agent that has since died is a silent lie. Knowledge is
  not a message: it carries its time, its why and whose it is, and a person
  reviewed it.
- **No scheduler and no dispatch.** Claiming work stays in GitHub issues and
  pull requests.
