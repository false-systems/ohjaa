#!/bin/bash
# Seal one cycle.v1 for the active Teko work in the current repository.
#
# This is the composition owner's job, not Ohjaa's: it produces the three
# immutable inputs the reducer trusts by digest — the Teko work artifact, a
# signed Kisko decision, a sealed Kelpo plan — and stamps cycle.v1 through
# sauma. Everything lands in $CYCLE_DIR, outside the repository tree, so the
# repository point does not move while the cycle is sealed.
#
# Requires on PATH: teko, toimija, kisko, sauma, jq, ssh-keygen, python3.
# Requires: an active Toimija session for this repository (so Teko records
# Toimija's point), KELPO=<path to kelpo.py>, KISKO_SEED=<kisko seed_store
# example binary>. Run after `teko begin …`.
set -euo pipefail

# One file instead of a handful of variables: cycle.env next to the artifacts.
CYCLE_ENV=${CYCLE_ENV:-${CYCLE_DIR:-.}/cycle.env}
if [ -f "$CYCLE_ENV" ]; then
  # shellcheck disable=SC1090
  . "$CYCLE_ENV"
fi
: "${CYCLE_DIR:?CYCLE_DIR must point at a directory outside the repository}"
: "${KELPO:?KELPO must point at kelpo.py}"
: "${KISKO_SEED:?KISKO_SEED must point at the kisko seed_store example binary}"
REPO_ID=${CYCLE_REPOSITORY_ID:-repo:$(basename "$(git rev-parse --show-toplevel)")}
SIGNER=${CYCLE_SIGNER:-operator}
RIGOR=${CYCLE_RIGOR:-smoke}
SUITE_ROOT=${CYCLE_SUITE_ROOT:-tests}
# The QA contract: the graph Sykli runs as Kelpo's tests. It must already be
# committed, so its hash can be sealed into the plan before any implementation
# evidence exists. Smoke rigor may point at the main graph.
QA_CONTRACT=${CYCLE_QA_CONTRACT:-sykli.json}
[ -f "$QA_CONTRACT" ] || { echo "QA contract $QA_CONTRACT does not exist in the tree" >&2; exit 3; }
if [ "$RIGOR" != smoke ] && [ "$QA_CONTRACT" = sykli.json ]; then
  echo "$RIGOR rigor needs a QA contract of its own (CYCLE_QA_CONTRACT), not the main graph" >&2; exit 3
fi
mkdir -p "$CYCLE_DIR/kisko/.kisko"

echo "== teko work artifact"
teko packet --artifact > "$CYCLE_DIR/work.json"
sauma verify "$CYCLE_DIR/work.json" >/dev/null
toimija -q repository-point --json > "$CYCLE_DIR/baseline-point.json"
git rev-parse HEAD > "$CYCLE_DIR/baseline-commit"
WD=$(jq -r .sauma.digest "$CYCLE_DIR/work.json")
WID=$(jq -r .packet.work.id "$CYCLE_DIR/work.json")
TOKEN=$(jq -r .repository.consistency_token "$CYCLE_DIR/baseline-point.json")
RP=$(jq -r .repository.id "$CYCLE_DIR/baseline-point.json")
WTOKEN=$(jq -r .repository.token "$CYCLE_DIR/work.json")
if [ "$TOKEN" != "$WTOKEN" ]; then
  echo "work artifact token $WTOKEN is not Toimija's current token $TOKEN — begin the work under a Toimija session" >&2
  exit 3
fi

echo "== kisko decision (signed by $SIGNER)"
cd "$CYCLE_DIR/kisko"
[ -f judgements.redb ] || "$KISKO_SEED" seed judgements.redb "$REPO_ID"
PDIG="sha256:$(shasum -a 256 "$CYCLE_DIR/baseline-point.json" | cut -d' ' -f1)"
jq -n --arg id "$WID" --arg d "$WD" --arg repo "$REPO_ID" \
  '{obligation:{id:$id,digest:$d},scopes:[{kind:"global"},{kind:"repository",id:$repo},{kind:"obligation",id:$id}]}' > obligation.json
jq -n --arg t "$TOKEN" --arg d "$PDIG" --arg wd "$WD" \
  '{repository_point:{repository:"'"$REPO_ID"'",consistency_token:$t,digest:$d},labels:[],
    facts:[{id:"F-1",kind:"observed",statement:"The active Teko work artifact and its declared scope",source:$wd}]}' > context.json
kisko decide --obligation obligation.json --repository-context context.json --store judgements.redb --json > inconclusive.json
CTX=$("$KISKO_SEED" context-digest inconclusive.json)
python3 - "$CTX" "${CYCLE_DECISION_SUMMARY:-Do the declared work within its scope}" <<'PY'
import json, hashlib, sys
ctx, summary = sys.argv[1], sys.argv[2]
analysis = {
  "facts_used": ["F-1"],
  "criteria_used": [{"logical_id": "C-1", "version": 1}],
  "alternatives": [
    {"id": "declared", "summary": summary, "invariant": "Only the declared write scope changes",
     "compatibility_impact": "none", "operational_impact": "none"},
    {"id": "widen", "summary": "Amend the scope first", "invariant": "Only the declared write scope changes",
     "compatibility_impact": "wider diff", "operational_impact": "an audited amendment"}
  ]
}
decision = {"selected": "declared", "rationale": "The declared scope is sufficient for the work.",
            "rejected": [{"alternative": "widen", "reason": "no evidence the scope is too narrow"}],
            "implementation_constraints": ["stay inside the declared scope", "commit; do not push"]}
body = json.dumps(analysis, sort_keys=True, separators=(",", ":")).encode()
model = {"adapter": "human", "model": "operator", "context_digest": ctx,
         "output_digest": "sha256:" + hashlib.sha256(body).hexdigest()}
json.dump({"analysis": analysis, "decision": decision, "model": model}, open("deliberation.json", "w"), indent=2)
PY
[ -f key ] || ssh-keygen -q -t ed25519 -N "" -C "$SIGNER" -f key
printf '%s %s\n' "$SIGNER" "$(cut -d' ' -f1,2 key.pub)" > .kisko/allowed_signers
rm -f deliberation.json.sig
ssh-keygen -Y sign -q -f key -n kisko-decision-v1 deliberation.json
kisko decide --obligation obligation.json --repository-context context.json --store judgements.redb \
  --deliberation deliberation.json --signature deliberation.json.sig --identity "$SIGNER" --json > "$CYCLE_DIR/decision.json"
STATUS=$(jq -r .status "$CYCLE_DIR/decision.json")
[ "$STATUS" = decided ] || { echo "kisko did not decide: $STATUS $(jq -r '.problem.code // ""' "$CYCLE_DIR/decision.json")" >&2; exit 3; }
sauma verify "$CYCLE_DIR/decision.json" >/dev/null
DD=$(jq -r .sauma.digest "$CYCLE_DIR/decision.json")
cd - >/dev/null

echo "== kelpo plan ($RIGOR, cases are the tasks of $QA_CONTRACT)"
CH=$(sykli plan "$QA_CONTRACT" --json | jq -r .contract_hash)
TASKS=$(sykli plan "$QA_CONTRACT" --json | jq -c '.tasks')
[ "$(echo "$TASKS" | jq length)" -gt 0 ] || { echo "QA contract $QA_CONTRACT has no tasks" >&2; exit 3; }
jq -n --arg w "$WID" --arg d "$WD" --arg t "$WTOKEN" --arg ch "$CH" --arg rigor "$RIGOR" --arg root "$SUITE_ROOT" --arg contract "$QA_CONTRACT" \
  --argjson tasks "$TASKS" --argjson obligations "$(jq -c '[.packet.obligations[].id]' "$CYCLE_DIR/work.json")" '
  ($obligations | to_entries | map({obligation: .value, task: $tasks[.key % ($tasks|length)]})) as $pairs |
  {schema:"kelpo-plan.v1", teko:{work:$w,digest:$d}, repository:{token:$t},
   obligations: ($pairs | map({obligation: .obligation, rigor: $rigor})),
   cases: ($pairs | map({id: .task, obligation: .obligation, layer: "unit",
            claim: ("QA task " + .task + " fails at the baseline and passes at the final tree"),
            setup: ("Evaluate " + $contract + " in a fresh worktree"),
            stimulus: ("sykli run " + $contract + " — task " + .task), oracle: "outcome passed at final; failed at baseline for standard rigor"})),
   gaps: [], suite: {roots: [$root], contract_path: $contract, contract_hash: $ch}}' > "$CYCLE_DIR/plan-body.json"
python3 "$KELPO" seal-plan "$CYCLE_DIR/plan-body.json" --work-artifact "$CYCLE_DIR/work.json" > "$CYCLE_DIR/plan.json"
sauma verify "$CYCLE_DIR/plan.json" >/dev/null
PD=$(jq -r .sauma.digest "$CYCLE_DIR/plan.json")

echo "== cycle.v1"
jq -n --arg wd "$WD" --arg dd "$DD" --arg pd "$PD" --arg rp "$RP" --arg signer "$SIGNER" \
  --argjson effects "${CYCLE_EFFECTS:-[\"commit\"]}" \
  --argjson max "${CYCLE_MAX_ATTEMPTS:-2}" --argjson retry "${CYCLE_WORKER_RETRY:-1}" '
  {schema:"cycle.v1",
   work:{artifact:"teko-work.v1",digest:$wd,repository_point:$rp},
   decision:{artifact:"kisko.decision.v1",digest:$dd,repository_point:$rp,signers:[$signer]},
   qa_plan:{artifact:"kelpo-plan.v1",digest:$pd,repository_point:$rp},
   implementation:{worker_role:"implementation",max_attempts:$max},
   qa:{worker_role:"qa",must_differ_from:"implementation"},
   effects:$effects, retry:{gate_failure:1,worker_failure:$retry}}' > "$CYCLE_DIR/cycle-body.json"
sauma stamp "$CYCLE_DIR/cycle-body.json" --producer "$SIGNER" --anchor "$TOKEN" \
  --artifact "$WD" --artifact "$DD" --artifact "$PD" --token "$TOKEN" > "$CYCLE_DIR/cycle.json"
sauma verify "$CYCLE_DIR/cycle.json" >/dev/null
echo "sealed $(jq -r .sauma.digest "$CYCLE_DIR/cycle.json")"
echo "next: ohjaa cycle run --cycle $CYCLE_DIR/cycle.json --work $CYCLE_DIR/work.json --decision $CYCLE_DIR/decision.json --qa-plan $CYCLE_DIR/plan.json --verdict $CYCLE_DIR/verdict.json --intent '<why>' <worker>"
