//! Deterministic autonomous-cycle reduction.
//!
//! The facts are supplied by adapters owned by Teko, Toimija, Kisko, and
//! Kelpo. Ohjaa stores none of them: it reduces the declared snapshot to one
//! legal next action.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub schema: String,
    pub decision: Decision,
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

pub fn next(facts: &Facts) -> Result<Next, &'static str> {
    if facts.schema != "ohjaa-cycle-facts.v1" {
        return Err("unsupported cycle facts schema");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            schema: "ohjaa-cycle-facts.v1".into(),
            decision: Decision::Allow,
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
        let mut facts = facts();
        assert_eq!(
            next(&facts).unwrap(),
            Next::Launch {
                leg: "implementation"
            }
        );
        facts.implementation = Evidence::Current;
        assert_eq!(next(&facts).unwrap(), Next::RunGates);
        facts.gates = Evidence::Current;
        assert_eq!(next(&facts).unwrap(), Next::Launch { leg: "qa" });
        facts.qa = Evidence::Current;
        assert_eq!(next(&facts).unwrap(), Next::JudgeQa);
        facts.verdict = Evidence::Current;
        assert_eq!(next(&facts).unwrap(), Next::CloseWork);
        facts.closure = Evidence::Current;
        assert_eq!(next(&facts).unwrap(), Next::Closed);
    }

    #[test]
    fn never_launches_without_a_current_decision_and_plan() {
        let mut facts = facts();
        facts.decision = Decision::Hold;
        assert_eq!(
            next(&facts).unwrap(),
            Next::Held {
                reason: "decision_held"
            }
        );
        facts.decision = Decision::Allow;
        facts.qa_plan = Evidence::Inconclusive;
        assert_eq!(next(&facts).unwrap(), Next::Held { reason: "qa_plan" });
    }
}
