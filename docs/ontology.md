# Ontology

What the concepts of agent-assisted software work mean, independent of any tool.

This document is the top layer. Contracts and tools sit beneath it.

## Purpose

> A personal way to work and release software fast with agents, built so anyone
> can adopt it: fast iterations, with confidence. The ontology stays the same
> when the tools change, or when the person does.

## Why Knowledge is at the centre

In a count of about 151 corrections that one person made to coding agents, the
corrections break down roughly like this:

| The agent… | Share |
|---|---|
| did not know the person's world: intent, current names, environment, taste | ~60% |
| overstepped or faked it | ~26% |
| stalled, because it could not tell what was safe | ~10% |

Control addresses about a quarter of that. The ontology is derived top-down,
from concepts that were true before agents and will be true after them.
**Knowledge** sits at its centre, not control.

## Admission rule: the punch-card test

A concept belongs here only if it was true for a programmer in 1970 and will
still be true when agents are replaced by whatever comes next.

- "Repository point" fails, because it belongs to the Git era. *State* passes.
- "Receipt" fails, because it is a format. *Evidence* passes.

A format that realises a concept is a **contract**. Contracts are versioned and
come with fixtures (for example [`knowledge-node.v1`](knowledge-node-v1.md)).
Tools implement contracts and may change freely.

```
ONTOLOGY    never changes     this document
CONTRACTS   change rarely     knowledge-node.v1, tree-identity.v2, …
TOOLS       change freely     git, GitHub, toimija, ohjaa, sykli, Claude, Codex, …
```

## Concepts

| Concept | Meaning | The question it answers |
|---|---|---|
| **Intent** | What a Person wants to become true | What are we doing, and when is it done? |
| **Knowledge** | What is known that bears on the work: facts, decisions, taste, lessons | What must an Agent understand before acting? |
| **State** | An exact, identifiable version of the work | Which version, exactly? |
| **Change** | A move from one State to another | How did we get here? |
| **Person** | A human who wants, knows, decides and judges | On whose behalf, and by whose authority? |
| **Agent** | Anyone or anything that acts for a Person | Who made the Change? |
| **Evidence** | Something observed at a State by someone other than the claimant | What was seen, rather than said? |
| **Judgement** | A decision that Evidence is enough for an Intent, made by someone other than the Agent that made the Change | Is it good enough? |
| **Boundary** | A point past which a Change is expensive to undo | Where does it get expensive? |
| **Outcome** | What happened after a Boundary was crossed | What did it do in the world? |

```
            corrections · Outcomes
                     │ feed
                     ▼
 Intent ──► KNOWLEDGE ──► Agent ──► Change ──► State ──► Evidence ──► Judgement ──► Boundary ──► Outcome
   ▲       (understood                                                (Knowledge                        │
   │        before acting)                                              applied)                        │
   └───────────────────────────── Outcomes raise new Intent ◄──────────────────────────────────────────┘
```

A Person and an Agent can be the same human. The distinction is in the role,
not the being. What matters is that the Agent that made a Change is never the
one that judges it.

## Laws

1. **Evidence and Judgement are about one exact State.** Evidence about one
   State says nothing about another.
2. **The Agent never judges its own Change.** Someone else does: another Person,
   or a mechanism the Agent cannot influence.
3. **Evidence is observed, not asserted.** A self-report is a claim.
4. **Only Boundaries are enforced, and enforcement is as strong as the crossing
   is hard to undo.** Inside a Boundary, work is free. This is the speed law.
5. **Outcomes and corrections feed Knowledge.** A correction a Person has to
   repeat is Knowledge that failed to reach the Agent.
6. **Knowledge carries its time, its why, and whose it is.** Without them it
   rots.

Law 6 is law 1 applied to Knowledge. Evidence is valid for one State, and
Knowledge is valid as of one moment. Both guard against the same failure:
something that was true once, used as if it were true now.

## Knowledge

**Kinds.**

| Kind | Meaning | Holds until |
|---|---|---|
| fact | True about the world | It expires, and must be re-confirmed |
| decision | Something chosen that binds the work | It is superseded |
| taste | A preference about *how* | It is superseded |
| lesson | Something learned from an Outcome or a correction | It is superseded |

**Whose.**

| Scope | Belongs to | Changes by |
|---|---|---|
| ours | every Person on the team, in every project | review |
| repo | one project, shared by everyone on it | review |
| mine | one Person | its owner |
| machine | one computer | its owner |

**Precedence.** Knowledge may only build on Knowledge that is at least as
shared. A personal preference never overrides a shared decision. Replacing a
piece of Knowledge happens within its own scope.

**Lifecycle.** proposed → current → superseded | retired.

- Agents may propose Knowledge. Only a Person makes it current (law 2).
- Nothing is deleted. Superseded Knowledge keeps its time and its why. Disproved
  assumptions are retained, never deleted.

## Speed and confidence

- An **iteration** is one trip round the loop, ending at a crossed Boundary.
  **Fast** means the trip is short and mostly needs no Person.
- **Confidence** means being able to answer four questions at every crossing,
  without trusting the Agent:
  - Which State crossed?
  - What was observed?
  - Who judged it, applying which Knowledge?
  - How is it undone?

These two do not trade off, provided Evidence is a by-product of the work and
never an extra step.

**Measures**, all derivable from records of crossings:

- iteration time
- crossing rate
- escape rate: crossings that later proved wrong
- recovery time
- **attention**: how often a Person had to step in

Attention is the scarce resource. Every Judgement a Person makes by hand is
Knowledge the system has not yet learned.

## Adoption principles

- **A1. The ontology is the product; tools are reference implementations.**
  Anyone can realise a concept with their own tools.
- **A2. Laws are fixed; the mechanisms that enforce them are pluggable.**
- **A3. Useful from the first Boundary.** Everything else is adopted in steps.
- **A4. A stranger understands it from one page before installing anything.**
- **A5. Safe defaults, with no dependence on a particular machine, account or
  layout.**
- **A6. Every promise the system makes is tested against its behaviour.**

## Who implements what today

This table is not normative. It is the only part of this document
expected to change.

| Concept | Implementation |
|---|---|
| Intent | a GitHub issue |
| Knowledge | ohjaa, in the [`knowledge-node.v1`](knowledge-node-v1.md) format |
| State | a Git commit; toimija reports live facts (sessions, current work) |
| Change | a GitHub pull request |
| Person, Agent | GitHub accounts; Claude, Codex |
| Evidence | sykli receipts; CI checks bound to a commit |
| Judgement | pull-request review by another Person |
| Boundary | merge to main; release |
| Outcome | issues, reverts |
