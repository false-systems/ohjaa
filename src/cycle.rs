//! Deterministic autonomous-cycle reduction.
//!
//! The facts are derived from the tools that own them — Toimija, Teko,
//! Kisko, and Kelpo — by `facts::derive`, or supplied as a sealed snapshot.
//! Ohjaa stores none of them: it reduces the declared snapshot to one legal
//! next action.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cycle {
    pub schema: String,
    pub work: Artifact,
    pub decision: Artifact,
    pub qa_plan: Artifact,
    pub implementation: Implementation,
    pub qa: Qa,
    pub effects: Vec<Effect>,
    pub retry: Retry,
    pub sauma: Envelope,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub artifact: String,
    pub digest: String,
    pub repository_point: String,
    /// Identities whose signature makes this artifact acceptable; absent means
    /// any signer the artifact's own trust root accepted. Used for the
    /// decision: who may launch a cycle is the composition owner's policy.
    #[serde(default)]
    pub signers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Implementation {
    pub worker_role: String,
    pub max_attempts: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qa {
    pub worker_role: String,
    pub must_differ_from: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Commit,
    Push,
    OpenPr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub gate_failure: u32,
    pub worker_failure: u32,
}

#[derive(Debug, Deserialize)]
pub struct Envelope {
    pub digest: String,
    /// The consistency token the artifact was sealed at. For the cycle, this
    /// is its input repository point, which every sealed input must share.
    #[serde(default)]
    pub anchored_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub schema: String,
    pub cycle: String,
    pub sauma: Envelope,
    pub decision: Decision,
    /// Whether every effect observed on this cycle's sessions was declared.
    /// `current` means within the allowlist, `failed` means an undeclared
    /// effect was observed, `inconclusive` means observation was incomplete.
    pub effects: Evidence,
    pub qa_plan: Evidence,
    pub implementation: Evidence,
    pub gates: Evidence,
    pub qa: Evidence,
    pub verdict: Evidence,
    pub closure: Evidence,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    Hold,
    Escalate,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Pending,
    Current,
    Failed,
    Inconclusive,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Next {
    Launch { leg: &'static str },
    RunGates,
    JudgeQa,
    CloseWork,
    Closed,
    Held { reason: &'static str },
    Failed { reason: &'static str },
}

pub fn next(cycle: &Cycle, facts: &Facts) -> Result<Next, &'static str> {
    validate_cycle(cycle)?;
    if facts.schema != "ohjaa-cycle-facts.v1" {
        return Err("unsupported cycle facts schema");
    }
    if facts.cycle != cycle.sauma.digest {
        return Err("facts do not belong to this cycle");
    }
    if !digest(&facts.sauma.digest) {
        return Err("invalid cycle facts envelope");
    }
    match facts.decision {
        Decision::Allow => {}
        Decision::Deny => {
            return Ok(Next::Held {
                reason: "decision_denied",
            })
        }
        Decision::Hold => {
            return Ok(Next::Held {
                reason: "decision_held",
            })
        }
        Decision::Escalate => {
            return Ok(Next::Held {
                reason: "decision_escalated",
            })
        }
    }
    match facts.effects {
        Evidence::Current | Evidence::Pending => {}
        Evidence::Failed => {
            return Ok(Next::Held {
                reason: "undeclared_effect",
            })
        }
        Evidence::Inconclusive => return Ok(Next::Held { reason: "effects" }),
    }
    match facts.qa_plan {
        Evidence::Current => {}
        Evidence::Failed => {
            return Ok(Next::Failed {
                reason: "qa_plan_failed",
            })
        }
        Evidence::Pending | Evidence::Inconclusive => {
            return Ok(Next::Held { reason: "qa_plan" });
        }
    }
    match facts.implementation {
        Evidence::Current => {}
        Evidence::Pending => {
            return Ok(Next::Launch {
                leg: "implementation",
            })
        }
        Evidence::Failed => {
            return Ok(Next::Failed {
                reason: "implementation_failed",
            })
        }
        Evidence::Inconclusive => {
            return Ok(Next::Held {
                reason: "implementation",
            })
        }
    }
    match facts.gates {
        Evidence::Current => {}
        Evidence::Pending => return Ok(Next::RunGates),
        Evidence::Failed => {
            return Ok(Next::Failed {
                reason: "gates_failed",
            })
        }
        Evidence::Inconclusive => return Ok(Next::Held { reason: "gates" }),
    }
    match facts.qa {
        Evidence::Current => {}
        Evidence::Pending => return Ok(Next::Launch { leg: "qa" }),
        Evidence::Failed => {
            return Ok(Next::Failed {
                reason: "qa_failed",
            })
        }
        Evidence::Inconclusive => return Ok(Next::Held { reason: "qa" }),
    }
    match facts.verdict {
        Evidence::Current => {}
        Evidence::Pending => return Ok(Next::JudgeQa),
        Evidence::Failed => {
            return Ok(Next::Failed {
                reason: "qa_verdict_failed",
            })
        }
        Evidence::Inconclusive => {
            return Ok(Next::Held {
                reason: "qa_verdict",
            })
        }
    }
    match facts.closure {
        Evidence::Current => Ok(Next::Closed),
        Evidence::Pending => Ok(Next::CloseWork),
        Evidence::Failed => Ok(Next::Failed {
            reason: "closure_failed",
        }),
        Evidence::Inconclusive => Ok(Next::Held { reason: "closure" }),
    }
}

fn validate_cycle(cycle: &Cycle) -> Result<(), &'static str> {
    let duplicate_effect = cycle
        .effects
        .iter()
        .enumerate()
        .any(|(index, effect)| cycle.effects[..index].contains(effect));
    let _retry_budget = cycle
        .retry
        .gate_failure
        .saturating_add(cycle.retry.worker_failure);
    if cycle.schema != "cycle.v1" {
        return Err("unsupported cycle schema");
    }
    if cycle.implementation.worker_role != "implementation"
        || cycle.qa.worker_role != "qa"
        || cycle.qa.must_differ_from != "implementation"
        || cycle.implementation.max_attempts == 0
        || duplicate_effect
        || !digest(&cycle.sauma.digest)
        || [&cycle.work, &cycle.decision, &cycle.qa_plan]
            .iter()
            .any(|artifact| {
                artifact.artifact.is_empty()
                    || artifact.repository_point.is_empty()
                    || !digest(&artifact.digest)
            })
    {
        return Err("invalid cycle");
    }
    Ok(())
}

fn digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            schema: "ohjaa-cycle-facts.v1".into(),
            cycle: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            sauma: Envelope {
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
                anchored_at: None,
            },
            decision: Decision::Allow,
            effects: Evidence::Current,
            qa_plan: Evidence::Current,
            implementation: Evidence::Pending,
            gates: Evidence::Pending,
            qa: Evidence::Pending,
            verdict: Evidence::Pending,
            closure: Evidence::Pending,
        }
    }

    #[test]
    fn advances_in_one_fixed_order() {
        let cycle = cycle();
        let mut facts = facts();
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Launch {
                leg: "implementation"
            }
        );
        facts.implementation = Evidence::Current;
        assert_eq!(next(&cycle, &facts).unwrap(), Next::RunGates);
        facts.gates = Evidence::Current;
        assert_eq!(next(&cycle, &facts).unwrap(), Next::Launch { leg: "qa" });
        facts.qa = Evidence::Current;
        assert_eq!(next(&cycle, &facts).unwrap(), Next::JudgeQa);
        facts.verdict = Evidence::Current;
        assert_eq!(next(&cycle, &facts).unwrap(), Next::CloseWork);
        facts.closure = Evidence::Current;
        assert_eq!(next(&cycle, &facts).unwrap(), Next::Closed);
    }

    #[test]
    fn never_launches_without_a_current_decision_and_plan() {
        let cycle = cycle();
        let mut facts = facts();
        facts.decision = Decision::Hold;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Held {
                reason: "decision_held"
            }
        );
        facts.decision = Decision::Allow;
        facts.qa_plan = Evidence::Inconclusive;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Held { reason: "qa_plan" }
        );
    }

    // The golden cases from docs/autonomous-cycle-v0.md that the reducer
    // alone can answer. Cases about live sessions, stale gates, and
    // independence are decided where the facts are derived (tests/cli.rs).

    #[test]
    fn golden_2_missing_decision_holds_before_any_launch() {
        let cycle = cycle();
        let mut facts = facts();
        for (decision, reason) in [
            (Decision::Hold, "decision_held"),
            (Decision::Deny, "decision_denied"),
            (Decision::Escalate, "decision_escalated"),
        ] {
            facts.decision = decision;
            assert_eq!(next(&cycle, &facts).unwrap(), Next::Held { reason });
        }
    }

    #[test]
    fn golden_8_exhausted_budget_fails_without_another_launch() {
        let cycle = cycle();
        let mut facts = facts();
        facts.implementation = Evidence::Failed;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Failed {
                reason: "implementation_failed"
            }
        );
    }

    #[test]
    fn a_failed_gate_or_verdict_is_terminal_not_a_relaunch() {
        let cycle = cycle();
        let mut facts = facts();
        facts.implementation = Evidence::Current;
        facts.gates = Evidence::Failed;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Failed {
                reason: "gates_failed"
            }
        );
        facts.gates = Evidence::Current;
        facts.qa = Evidence::Current;
        facts.verdict = Evidence::Failed;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Failed {
                reason: "qa_verdict_failed"
            }
        );
    }

    #[test]
    fn facts_from_another_cycle_are_refused() {
        let cycle = cycle();
        let mut facts = facts();
        facts.cycle =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
        assert_eq!(
            next(&cycle, &facts).unwrap_err(),
            "facts do not belong to this cycle"
        );
    }

    fn cycle() -> Cycle {
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        Cycle {
            schema: "cycle.v1".into(),
            work: Artifact {
                artifact: "teko-work.v1".into(),
                digest: digest.into(),
                repository_point: "rpoint_x".into(),
                signers: None,
            },
            decision: Artifact {
                artifact: "kisko-decision.v1".into(),
                digest: digest.into(),
                repository_point: "rpoint_x".into(),
                signers: None,
            },
            qa_plan: Artifact {
                artifact: "kelpo-plan.v1".into(),
                digest: digest.into(),
                repository_point: "rpoint_x".into(),
                signers: None,
            },
            implementation: Implementation {
                worker_role: "implementation".into(),
                max_attempts: 2,
            },
            qa: Qa {
                worker_role: "qa".into(),
                must_differ_from: "implementation".into(),
            },
            effects: Vec::new(),
            retry: Retry {
                gate_failure: 1,
                worker_failure: 1,
            },
            sauma: Envelope {
                digest: digest.into(),
                anchored_at: None,
            },
        }
    }

    #[test]
    fn golden_7_an_undeclared_effect_holds_before_the_next_action() {
        let cycle = cycle();
        let mut facts = facts();
        facts.implementation = Evidence::Current;
        facts.effects = Evidence::Failed;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Held {
                reason: "undeclared_effect"
            }
        );
        facts.effects = Evidence::Inconclusive;
        assert_eq!(
            next(&cycle, &facts).unwrap(),
            Next::Held { reason: "effects" }
        );
    }
}
