# ohjaa

**The deciding layer above toimija.** *Ohjaa* — Finnish for *steer*.

Toimija reports facts and refuses to decide. That refusal is deliberate and
load-bearing: its boundaries forbid dispatch, assignment, and network. So the
deciding has to live somewhere else. This is that somewhere.

```bash
ohjaa fleet                        # who is live, across every repo
ohjaa system                       # what every tool in the fleet is, derived now
ohjaa why <digest|token>           # provenance walk, every root filled in
ohjaa run claude --intent "…"      # inspect siblings, then hand off
ohjaa cycle step --facts cycle.json # reduce verified cycle facts to one action
ohjaa cycle run --facts cycle.json codex --intent "implement" --dry-run
```

## What it is

One small Rust binary that shells out to `toimija` and `sauma` and stores
nothing. It never speaks the plugin protocol, never compiles a packet, never
reads a repository point. It asks the tools that own those things, decides, and
gets out of the way.

```
ohjaa                    checks coverage · derives the fleet view
 └─ toimija run          packet · session · heartbeat
     ├─ teko  work.v1    ── stdio JSON-RPC ──┐
     └─ named worker     ── direct or worker.v1 ──┘   (repository_point on every call)

sauma                    verifies envelopes, walks provenance across repos
```

## The system perimeter

An agent launched into this stack should not have to discover the stack by
guessing. Every repo in the fleet self-declares what it is in
`.toimija/repo.yaml`, and toimija surfaces that as the `repo` object of
`toimija context --json`. `ohjaa system` derives the whole picture from those
declarations plus live occupancy — nothing is stored, and the view cannot be
staler than the moment it was computed. A repo whose role cannot be derived
is reported as unknown and makes the exit code `4`: absence is not evidence.

`ohjaa run` delivers the same picture to the agent it launches: the perimeter
— fleet, roles, who is live, "you are here", the coordination rules — is
prepended to the task, so it travels in toimija's composed launch prompt and
the agent cannot not-see it. The briefing tells the agent how to re-derive
every claim (`ohjaa system`, `ohjaa fleet`), because a derived view is never
stale and a delivered one always is. `--no-briefing` opts out.

## Two agents, one checkout

Run both from the repository they will share:

```bash
# terminal 1
ohjaa run codex --intent "change the parser" --task "fix the lexer" --scope src/parser/

# terminal 2, same directory
ohjaa run claude --intent "refresh the guide"

# everything after `--` reaches the agent command itself
ohjaa run codex --intent "change the parser" -- --model o3
```

Each agent gets its own Toimija session and live packet. `--task` and
`--scope` are forwarded to `toimija run` (the task travels in the composed
launch prompt, with the system perimeter prepended), and anything after `--`
is passed through to the agent command; `--dry-run` decides and explains,
printing the exact command it would exec, without launching. The agents still
share files and Git's index, so Ohjaa reports sibling warnings but does not
pretend it can prevent edit collisions without scopes or isolation. If
Toimija cannot inspect the sessions, Ohjaa refuses to launch blind.

## Derive, never store

The whole stack is **pull-based**: toimija compiles the packet on demand, sauma
computes provenance views on demand, proposal 0010 derives team facts from
`refs/remotes/*`. Nothing is ever *delivered to* anything.

A derived view is never stale, because it is computed now. A delivered message
is stale the moment the sender's world moves.

So ohjaa is **a scan, not an inbox** — and **a courier of anchored facts, not a
message bus**. Anything that crosses a repo boundary travels as a digest
anchored to a repository point, so it self-invalidates exactly like a gate
receipt does when the tree moves. Nothing is trusted because it was sent.

The root list follows the same rule: by default it is *derived* (the sibling
directories of your git root), so working in `~/projects/toimija` makes the
fleet `~/projects/*` with no configuration at all. `~/.config/ohjaa/roots`
exists as an override and holds paths — never contents.

## Coverage is a fact

A scan that failed is not a scan that found nothing:

```
coverage: scanned 17/18 repo(s)
  incomplete — absence here is not evidence of absence:
    somerepo: not a Git repository
```

`ohjaa fleet` exits `4` when coverage is degraded or failed. `ohjaa run` also
refuses either state. Reporting quiet when you could not fully look is the
failure mode this whole stack exists to prevent.

The same rule governs staleness. Toimija reports `stale` both for a session
whose process is gone and for one that has merely been quiet past its
threshold — an agent thinking hard for ninety minutes looks exactly like a
dead one. Ohjaa therefore counts every session that is not `closed` as still
occupying the checkout, and marks the uncertain ones `stale` rather than
quietly dropping them. Only `closed` is something Toimija observed rather than
inferred from silence.

The label says only `stale`; the `seen:` age beside it carries the judgment. A
session last seen 491 hours ago is almost certainly gone and one last seen 95
minutes ago is probably thinking, and neither claim is Ohjaa's to make — the
timestamp is the confession, so the reader decides.

## What it will never do

Permanently refused, for the reasons proposal 0010 refused them:

- **No inbox, no messaging, no request/response.** A message from an agent that
  has since died is a silent lie. An inbox is a second database with
  last-writer-wins races — exactly what proposal 0004 was killed for.
- **No agent-to-agent handoff.** `ohjaa cycle step` derives exactly one action
  from a verified facts snapshot. `ohjaa cycle run` can launch only its next
  implementation or QA leg through Toimija; it never stores workflow state or
  schedules future work. Claiming work stays in GitHub issues, draft PRs, and
  labels.
- **No presence.** "Is that agent online?" is unanswerable without heartbeats,
  and heartbeats lie by default. The truthful ceiling is a timestamped last
  observation, which toimija already provides.
- **No index, no cache, no database, no daemon.**
- **No network.** Cross-*repo* is filesystem reads. Cross-*machine* is a
  separate boundary decision, not a side effect of this one.

## Status

Early, but real: `fleet`, `system`, `why`, and `run` work today. `run`
requires complete local session coverage, reports every sibling that may
still be working the checkout, derives the system perimeter into the agent's
task, and then hands the agent to Toimija. Existing sessions are facts,
not locks.

The sibling count is a reading taken just before launch, not a standing fact:
nothing serializes the window between the scan and `toimija run`, so two
invocations started in the same instant each miss the other. That is inherent
to deriving rather than locking, and it is deliberately not being fixed.

A lock would be stored state — the one thing this repo refuses — and it would
buy nothing, because nothing is gated on the count. Ohjaa reports who is here
and launches either way; a number that is milliseconds stale changes no
decision. Serializing launches to make an advisory line exact, in a tool whose
whole claim is that sessions are facts and not locks, would be the tail
wagging the dog. The line says "at launch" so it does not sound like a
guarantee it cannot give.

If a future policy ever *does* gate on occupancy, this stops being cosmetic
and the lock becomes a real boundary decision. It is not one today.

## Install

```bash
cargo install --path .
```

## Validation

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```
