#!/bin/sh
# A scripted cycle worker. Toimija launches it once per leg; the leg is read
# back from this session's own marker, never from a prompt.
#
# implementation: runs $CYCLE_IMPLEMENTATION (a shell snippet) inside the
#   repository and commits whatever it changed within the declared scope.
#   Idempotent: if the tree is unchanged afterwards, it exits 0 and commits
#   nothing, so a re-launched leg consumes cleanly.
# qa: evaluates the locked Sykli graph in a fresh worktree of the final tree
#   (observed outcomes, never cached), normalizes it through Kelpo, builds a
#   verdict from the sealed plan, and judges it. It never touches the tree and
#   never talks to Teko: the cycle loop delivers the verdict between legs.
set -eu
CD=${CYCLE_DIR:?CYCLE_DIR must point at the cycle artifact directory}
KELPO=${KELPO:?KELPO must point at kelpo.py}
ASSEMBLE=${KELPO_ASSEMBLE:?KELPO_ASSEMBLE must point at assemble_smoke_verdict.py from kelpo}
SYKLI_BIN=${SYKLI_BIN:-sykli}
LEG=$(toimija -q sessions --json | jq -r --arg id "$TOIMIJA_SESSION" '.sessions[] | select(.id==$id) | .cycle_action.leg')
echo "worker: session $TOIMIJA_SESSION leg=$LEG" >&2
case "$LEG" in
implementation)
  cp "$TOIMIJA_LIVE_JSON" "$CD/ctx-implementation-$TOIMIJA_SESSION.json"
  sh -c "${CYCLE_IMPLEMENTATION:?CYCLE_IMPLEMENTATION must hold the change to make}"
  if git diff --quiet && git diff --cached --quiet && [ -z "$(git status --porcelain)" ]; then
    echo "worker: tree unchanged; nothing to commit" >&2
    exit 0
  fi
  git add -A
  git -c commit.gpgsign=false commit -q -m "${CYCLE_COMMIT_MESSAGE:-cycle: implementation leg}" \
    -m "Committed by the implementation leg of an ohjaa cycle (session $TOIMIJA_SESSION)."
  echo "worker: committed $(git rev-parse --short HEAD)" >&2
  ;;
qa)
  cp "$TOIMIJA_LIVE_JSON" "$CD/ctx-qa.json"
  toimija -q repository-point --json > "$CD/final-point.json"
  W=$(mktemp -d "${TMPDIR:-/tmp}/cycle-qa-XXXXXX")
  git worktree add -q --detach "$W" HEAD
  ( cd "$W" && "$SYKLI_BIN" run sykli.json --json > "$CD/receipt.json" )
  git worktree remove --force "$W"
  python3 "$KELPO" evidence --receipt "$CD/receipt.json" --point "$CD/final-point.json" --context "$CD/ctx-qa.json" > "$CD/evidence.json"
  B=$(mktemp -d "${TMPDIR:-/tmp}/cycle-baseline-XXXXXX")
  git worktree add -q --detach "$B" "$(cat "$CD/baseline-commit")"
  ROOT=$(jq -r '.suite.roots[0]' "$CD/plan.json")
  python3 "$KELPO" suite-manifest --repository "$B" --point "$CD/baseline-point.json" --root "$ROOT" > "$CD/baseline-manifest.json"
  python3 "$KELPO" suite-manifest --repository "$PWD" --point "$CD/final-point.json" --root "$ROOT" > "$CD/final-manifest.json"
  python3 "$ASSEMBLE" "$CD"
  python3 "$KELPO" judge --plan "$CD/plan.json" --verdict "$CD/verdict-body.json" \
    --evidence "$CD/evidence.json" --baseline-manifest "$CD/baseline-manifest.json" \
    --final-manifest "$CD/final-manifest.json" \
    $(for c in "$CD"/ctx-implementation-*.json; do printf -- '--session-context %s ' "$c"; done) \
    --session-context "$CD/ctx-qa.json" --baseline-root "$B" --final-root "$PWD" > "$CD/verdict.json"
  git worktree remove --force "$B"
  sauma verify "$CD/verdict.json" >/dev/null
  echo "worker: verdict $(jq -r .verdict "$CD/verdict.json") sealed at $(jq -r .sauma.digest "$CD/verdict.json")" >&2
  ;;
*)
  echo "worker: unknown leg '$LEG'" >&2; exit 2;;
esac
