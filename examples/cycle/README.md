# Cycle kit

The composition owner's side of the autonomous cycle v0 (sykli
`docs/autonomous-cycle-v0.md`): seal `cycle.v1` from real artifacts, then let
`ohjaa cycle run` drive it. Ohjaa stores nothing; everything here lives in
`$CYCLE_DIR`, a directory outside the repository tree.

```bash
export CYCLE_DIR=/tmp/cycle-1
export KELPO=~/projects/kelpo/kelpo.py
export KELPO_ASSEMBLE=~/projects/kelpo/examples/assemble_smoke_verdict.py
export KISKO_SEED=$(cd ~/projects/kisko && cargo build -q --example seed_store && \
                    echo "$(cargo metadata --format-version 1 | jq -r .target_directory)/debug/examples/seed_store")

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
evaluates the locked Sykli graph in a fresh worktree so outcomes are observed,
normalizes the receipt through Kelpo, assembles and judges a verdict, and
writes it to `$CYCLE_DIR/verdict.json`. It never talks to Teko: the loop
delivers the verdict between legs, because Toimija holds Teko's single-process
store for the whole session.

A real agent replaces `worker.sh`; the leg still arrives via the session's
`cycle_action`, and the QA leg still owes a sealed verdict at the path the loop
was given.

Terminal outcomes and their exit codes are Ohjaa's: `closed` 0, `failed` 1,
`held` 3. A failed cycle is terminal; recovery is a new `cycle.json` with a
new digest (change a budget), never edited state.
