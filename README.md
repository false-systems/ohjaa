# ohjaa

**The deciding layer above toimija.** *Ohjaa* — Finnish for *steer*.

Toimija reports facts and refuses to decide. That refusal is deliberate and
load-bearing: its boundaries forbid dispatch, assignment, and network. So the
deciding has to live somewhere else. This is that somewhere.

```bash
ohjaa fleet                        # who is live, across every repo
ohjaa why <digest|token>           # provenance walk, every root filled in
ohjaa run claude --intent "…"      # seat an agent safely, then hand off
```

## What it is

One small Rust binary that shells out to `toimija` and `sauma` and stores
nothing. It never speaks the plugin protocol, never compiles a packet, never
reads a repository point. It asks the tools that own those things, decides, and
gets out of the way.

```
ohjaa                    decides a seat · derives the fleet view
 └─ toimija run          packet · session · heartbeat
     ├─ teko  work.v1    ── stdio JSON-RPC ──┐
     └─ kisko worker.v1  ── stdio JSON-RPC ──┘   (repository_point on every call)

sauma                    verifies envelopes, walks provenance across repos
```

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

`ohjaa fleet` exits `4` when coverage is incomplete. Reporting quiet when you
could not look is the failure mode this whole stack exists to prevent.

## What it will never do

Permanently refused, for the reasons proposal 0010 refused them:

- **No inbox, no messaging, no request/response.** A message from an agent that
  has since died is a silent lie. An inbox is a second database with
  last-writer-wins races — exactly what proposal 0004 was killed for.
- **No task dispatch or handoff between agents.** The human remains the
  orchestrator. Claiming work stays in GitHub issues, draft PRs, and labels.
- **No presence.** "Is that agent online?" is unanswerable without heartbeats,
  and heartbeats lie by default. The truthful ceiling is a timestamped last
  observation, which toimija already provides.
- **No index, no cache, no database, no daemon.**
- **No network.** Cross-*repo* is filesystem reads. Cross-*machine* is a
  separate boundary decision, not a side effect of this one.

## Status

Early, but real: `fleet` and `why` work today.

`choose_seat()` is **intentionally unimplemented**. It encodes how much friction
is worth accepting to prevent collisions between agents, and that is a human
call — so `ohjaa run` refuses with a message pointing at the five decisions it
needs, and a test guards it against being quietly filled in.

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
