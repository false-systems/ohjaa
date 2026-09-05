# Cycle kit

The composition owner's side of the autonomous cycle v0 (sykli
`docs/autonomous-cycle-v0.md`): seal `cycle.v1` from real artifacts, then let
`ohjaa cycle run` drive it. Ohjaa stores nothing; everything here lives in
`$CYCLE_DIR`, a directory outside the repository tree.

```bash
ohjaa cycle doctor                                # what is missing, before anything runs
mkdir -p /tmp/cycle-1 && cp examples/cycle/cycle.env.example /tmp/cycle-1/cycle.env
$EDITOR /tmp/cycle-1/cycle.env                    # paths only; the kit sources it
export CYCLE_DIR=/tmp/cycle-1

# inside a Toimija session for the repository, so Teko records Toimija's point
teko begin "record the run" --goal note="the doc has a section" --gate sykli-full --scope docs
seal-cycle.sh                                     # work + decision + plan → cycle.json

CYCLE_IMPLEMENTATION='printf "\n## Note\n\nadded by the cycle\n" >> docs/some.md' \
ohjaa cycle run --cycle $CYCLE_DIR/cycle.json --work $CYCLE_DIR/work.json \
  --decision $CYCLE_DIR/decision.json --qa-plan $CYCLE_DIR/plan.json \
  --verdict $CYCLE_DIR/verdict.json --no-briefing --intent "cycle" \
  --scope docs examples/cycle/worker.sh
```

`seal-cycle.sh` produces, in order: the sealed Teko work artifact and the
baseline Toimija point; a Kisko decision signed by `$CYCLE_SIGNER` (default
`operator`) over an operator-authored deliberation, `decided` only if the
context digest binds it to these exact inputs; a Kelpo plan at `$CYCLE_RIGOR`
(default `smoke`) mapping one sykli task to each Teko obligation; and
`cycle.json` stamped by sauma. It refuses if Teko's work token is not Toimija's
current token, which means `teko begin` ran outside a Toimija session.

`worker.sh` is a scripted worker for both legs. The implementation leg runs
`$CYCLE_IMPLEMENTATION` and commits within scope, idempotently. The QA leg
runs the plan's QA contract (`suite.contract_path`, a second Sykli contract
whose tasks are the plan's cases) in a fresh worktree of the final tree for
green evidence and, at standard rigor, in a worktree of the baseline for red
evidence: the cases must fail there. Kelpo assembles and judges; a case that
fails at the final tree refutes its obligation and the verdict is `fail`. The
verdict lands at `$CYCLE_DIR/verdict.json`. The QA contract is committed
before `teko begin`, so its hash is sealed into the plan before any
implementation evidence exists. It never talks to Teko: the loop
delivers the verdict between legs, because Toimija holds Teko's single-process
store for the whole session.

A real agent replaces `worker.sh`; the leg still arrives via the session's
`cycle_action`, and the QA leg still owes a sealed verdict at the path the loop
was given.

Terminal outcomes and their exit codes are Ohjaa's: `closed` 0, `failed` 1,
`held` 3. A failed cycle is terminal; recovery is a new `cycle.json` with a
new digest (change a budget), never edited state.
