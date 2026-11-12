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
| repo | `<repo>/AGENTS.md`, its `## Lessons` section; ohjaa keeps each node, with its why, in its state folder | `ohjaa lessons` in a working copy, then a commit |
| mine | `mine/` in the home | you |
| machine | `machine/` in the home | you |

The home is `$XDG_CONFIG_HOME/ohjaa`, by default `~/.config/ohjaa`. The state
folder is `$XDG_STATE_HOME/ohjaa`, by default `~/.local/state/ohjaa`.

Knowledge is ordered **ours → repo → mine → machine**, then **decision → fact
→ taste → lesson**. A personal node never supersedes a shared one, and shared
knowledge never builds on personal knowledge. Ohjaa defines the format and those rules in
[knowledge-node-v1.md](knowledge-node-v1.md). The reference implementation
is the crate `crates/knowledge-node` (package `ohjaa-knowledge-node`), and the golden fixtures are in
`fixtures/knowledge-node.v1/`. [ontology.md](ontology.md) says what the
concepts mean.

## Commands

| Command | What it does |
|---|---|
| `ohjaa apply` | Writes the knowledge into each agent's own instruction file, and an ignore block into git's global excludes. `--dry-run`, `--undo`. |
| `ohjaa doctor` | Reports what differs from what `apply` would make it. Read-only. |
| `ohjaa propose` | Proposes a node. It waits until a person decides. |
| `ohjaa review` | Goes through the proposals, one at a time. A person only. `--list` only lists them. |
| `ohjaa accept <id>`, `reject <id>` | Decide one proposal. A person only. |
| `ohjaa retire <id> --why <why>` | Retires a node. It is kept, with its why. A person only. |
| `ohjaa confirm <id>` | Confirms that a node still holds, as of today. A person only. |
| `ohjaa list` | Lists the knowledge that applies here. `--all` includes superseded and retired nodes. |
| `ohjaa check` | Validates every node, and flags what may no longer hold. `--strict`, `--budget`. |
| `ohjaa lessons [<path>]` | Writes the repository's lessons into the `AGENTS.md` of the working copy it runs in, or the one `<path>` is in, for you to commit. `--dry-run`. |
| `ohjaa brief` | Prints the briefing for the repository it runs in. `--scope`, `--agent`, `--knowledge-only`, `--write`. |

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
  checkout. Repo knowledge goes into the repository's own `AGENTS.md` with
  `ohjaa lessons`, and the person commits and pushes it like any change.

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
- Unreadable proposals make the review incomplete (exit 4), never an empty
  or complete queue.

### Possible duplicates

Agents propose one lesson several times over, and every proposal costs a
person a review. A proposal whose title says mostly what the title of a
present node of its own scope says (of the same repository, for a repo one)
is shown right after that node, and marked a *possible duplicate* of it.
Present means current, or proposed and waiting before it.

- A node's title is its id: its words, lowercased, without filler (`a`,
  `the`, `and`, `with`, …), each with one common ending cut (`-ing`, `-ed`,
  `-es`, `-s`, `-e`).
- Two titles are near when they share at least two words, and the shared
  words are at least 60% of both (Dice's coefficient, 2·shared / (a + b) ≥
  0.6).
- The node a proposal refines, and those it supersedes, are not compared.

Nothing of this is stored, and nothing is refused: two distinct lessons can
have near titles. The person decides.

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
- Only what ohjaa keeps there is committed: `mine/`, `machine/`,
  `applied.toml` and `ours`, by an allowlist in the repository's own
  `.git/info/exclude`. The home holds no `.gitignore`. A `.DS_Store`, an
  editor's swap file or a file of your own beside them is never committed.

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
  mine/ machine/ ours               knowledge and settings
  applied.toml                      what apply changed outside the home, for --undo
  .git/                             the home's history; never pushed
~/.config/git/ignore                git's global excludes (or core.excludesFile):
                                    a block marked "ohjaa apply" holding .sykli/ and .ohjaa/
each agent's own instruction file   a block marked "ohjaa:knowledge"
```

```bash
ohjaa apply --dry-run     # every change, none made
ohjaa apply               # make them, then commit in the home
ohjaa apply --undo        # put back what apply changed; the history stays
ohjaa doctor              # read-only; exits 4 on drift
```

`apply` makes two changes outside the home, and nothing else:

- **The knowledge block.** Ours, mine and machine go into each agent's own
  global instruction file, from one source, the nodes.
- **The ignore block.** A marked block in git's global excludes file ignores
  `.sykli/` (sykli's receipts) and `.ohjaa/` (lessons accepted before ohjaa
  kept them in its state, untracked in a checkout), in every repository.

Every change is planned before any is made, and each is recorded in
`applied.toml` before it is made. So `--undo` puts back exactly what was
there, a missing final newline included, even after an apply that failed
midway. What the record holds that `--undo` cannot take back stays in it,
and undo exits 4.

**The terminal's line, taken out.** An earlier ohjaa wrote one line into
`~/.zshrc` (`[[ -x <ohjaa> ]] && eval "$(<ohjaa> shell-init zsh)"`, under a
comment that names ohjaa). It runs `ohjaa shell-init`, which this ohjaa does
not have, so every new terminal would say so. `apply` takes that line out,
with its comment and the blank line it put before it, and its record forgets
it. The lines it once replaced (an earlier ohjaa launcher) are not put back.
`ohjaa doctor` reports such a line as drift, and `apply --undo` still takes
it back exactly as that apply recorded it. A line you wrote yourself that
runs `ohjaa shell-init` is yours: take it out by hand.

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


### The perimeter

Without `--knowledge-only`, the briefing ends with the perimeter: what the
launch repository and each scoped one is, and who is working there.

- Each repository's role comes from its own `.toimija/repo.yaml`, through
  `toimija context --identity --json` in it. A role that cannot be derived
  is said, never guessed, and `brief` exits 4.
- Who is working comes from `toimija sessions --json` in each, and in the
  other worktrees of the launch clone, and `toimija sessions --json
  --outside-repos` for sessions outside every repository.
- A session toimija reports as `stale` stays visible, with its age: "quiet
  for ninety minutes" and "gone" look the same from outside.
- A scan that failed is not a scan that found nothing: it is said as
  incomplete, with why, and `brief` exits 4.

Other repositories are not scanned. The perimeter is derived when asked and
never stored, so it cannot be staler than the moment it was computed.

## Repo knowledge: the repository's own AGENTS.md

Every agent reads a repository's `AGENTS.md`, so that is where repo knowledge
goes. When you accept a repo proposal, ohjaa keeps the node in its state and
writes nothing into the repository. `ohjaa lessons`, in a working copy of
that repository, then writes its line into the `## Lessons` section of the
working copy's `AGENTS.md`. You read it, commit it and push it like any
change, through the repository's own checks.

```bash
ohjaa lessons --dry-run      # what would change in this working copy's AGENTS.md
ohjaa lessons                # write it; commit and push it yourself
ohjaa lessons ../widget/src  # the working copy that folder is in
```

**The node** is kept in ohjaa's state, with its why:
`lessons/<clone>/<id>.md`, where `<clone>` is the clone's common git
directory, so every worktree of it shares them. A node accepted before ohjaa
kept repo knowledge there sits untracked in the checkout's
`.ohjaa/knowledge/`, and is read from there too; a decision on it moves it
into the state.

**The line**, one per node, holds its body on one line, marked with its id:

```
## Lessons

<!-- This repository's accepted knowledge: one line per node, each marked with its id, … -->

- <!-- lesson: widget-uses-rust --> **decision**: Widget code is Rust. (as of 2026-10-27)
```

- A current node's line is written; a retired or superseded node's line is
  taken out.
- A line is replaced by its id wherever it is. A confirmation moves its
  date. A second line of the same id goes.
- A new line goes after the last of the section's. The section is added at
  the end of the file when there is none, and `AGENTS.md` is made when there
  is none.
- Your own lines, and a line of an id ohjaa does not know, stay as they are.
  So does the line of a node whose file cannot be read: it is named, and
  `ohjaa lessons` exits 4.
- Writing the same nodes again changes nothing.
- Ohjaa's own words there (its marks and the section's intro) name no tool,
  and no why goes with a line. A line is the node's text as a person
  accepted it: ohjaa checks it for nothing, so a line that names a tool or a
  private repository is yours to reword before you commit it.
- `AGENTS.md` is read whole or not at all. A file that is not UTF-8 is never
  written over, and neither is a link that leads outside the working copy,
  or to no file.
- Nothing is committed, pushed or sent. Ohjaa makes no network call and runs
  no `gh`: what reaches the repository is what you commit.

### What the briefing does with repo knowledge

The briefing reads the repository's `AGENTS.md` as the working copy holds it. A line inside
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

Until the working copy's `AGENTS.md` holds a node's line as it is now, the
briefing carries the node, marked beside its id `not yet in AGENTS.md`.
`ohjaa list` says, under each current repo node, where it is: `in AGENTS.md
(<path>)`, or what `ohjaa lessons` would do.

A line `AGENTS.md` still holds of a retired or superseded node is named for
every agent:

```
AGENTS.md still says `widget-uses-rust`, but it was retired on 2026-10-28 (the widget is gone); `ohjaa lessons` takes its line out: do not follow it.
```

## Check: keeping it true

`ohjaa check` validates every node and the rules across them. It also flags
what may no longer hold:

- a `holds_while` predicate that is false right now;
- a **fact** not confirmed for 90 days (`ohjaa confirm <id>`);
- a node whose text names, in backticks, an `ohjaa` command that this ohjaa
  does not have: the rule asks for what no agent can do;
- a briefing over the budget (`--budget`, 8,000 characters by default).

A flag never hides a node. The briefing shows the node with the flag beside
it. `check --strict` exits non-zero on flags too, not only on errors. Ohjaa
never changes a node itself: rewording, confirming and retiring stay a
person's.

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
  knowledge is in the repository's own `AGENTS.md`, and in ohjaa's state
  with its why. Mine and machine are in the home's own history. Everything
  else is derived when asked. There is no cache, no knowledge index and no
  daemon.
- **The workflow lives in the home, outside every repository.** Outside the
  home, `ohjaa apply` changes only a marked block in git's global excludes
  file and a marked block in each agent's own global instruction file that
  already exists outside every repository, and takes out of `~/.zshrc` the
  line an earlier apply wrote. Only `ohjaa lessons`, when a person runs it,
  writes into a checkout: the `## Lessons` section of its `AGENTS.md`, to be
  committed like any change. No hook or config of a repository is written.
- **Ohjaa checks nothing and refuses nothing for the workflow.** The checks
  a commit must pass, and refusing a push without them, are sykli's: `sykli
  run` proves a commit from the repository's own `sykli.json`, and `sykli
  hook install --pre-push` installs the hook that refuses. Ohjaa runs no
  check and installs no hook.
- **Ohjaa runs no sessions.** Agents start as they are typed and read the
  knowledge from their own instruction files. The note for the next agent is
  `toimija handover`. What agents used is vessa's.
- **Agents propose; only a person makes knowledge current**, retires it or
  rejects it. Ohjaa's own words in a repository (its marks and the Lessons
  section's intro) name no tool; each line is a node's text as a person
  accepted it. All of it goes into a working copy, never sent: a person
  reads it before it is committed.
- **Cross-repo yes, messaging no.** There is no request and response between
  agents, no presence and no dispatch.
- **No network and no gh.** Every command works from the filesystem and
  local CLIs: git, and toimija for the briefing's perimeter.
- **Failed coverage is never shown as an empty result.** If a scan could
  not look, it says so and exits non-zero.

### What the state folder holds

Everything is under `$XDG_STATE_HOME/ohjaa`:

| Path | Holds | If deleted |
|---|---|---|
| `proposals/` | proposals waiting for a person | the proposals are lost |
| `lessons/<clone>/` | each accepted repo node with its why | the nodes are lost; their lines in `AGENTS.md` stay, as lines of ids ohjaa does not know |

### Few git processes

Git is slow to start, so ohjaa counts the git processes it starts.

- `apply` and `doctor` read git's global config in one process and no clone.
- The home's history costs two per commit (`add -A` and `commit`), and one
  more once, the home's `git init`.
- The agents' instruction files are read and written without git.
- `brief` asks git for the repository's root once.
- `lessons` reads the repository's layout, and starts no git.

`tests/config_home.rs` holds these budgets.

## Tests

`cargo test` runs against stand-ins for toimija, gh and the agents, in a
sandboxed home, never the person's own. With `cargo fmt --check` and
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
| `tests/lessons.rs` | repo knowledge in a working copy's `AGENTS.md`, with a local bare repository as its origin, and a stand-in gh that no test may see called |
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
