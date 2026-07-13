//! A TRACE — what one run of a scenario observed, in order.
//!
//! Written verbatim: raw timestamps, raw TTLs, raw native error strings. The trace is
//! the EVIDENCE, and evidence is not edited on the way in. `project.rs` decides what
//! a given claim looks at.

use serde::Deserialize;
use serde::Serialize;

use crate::command::Hello;
use crate::observation::Observation;

pub const TRACE_SCHEMA: &str = "parity-trace/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub schema: String,
    /// The scenario that produced this.
    pub scenario: String,
    /// Which binding of roles→clients this run was, e.g. "oracle" or "subject".
    pub run: String,
    /// role → the driver that played it. The thing a reader most needs to know and
    /// the thing a bare pair of traces would otherwise leave implicit.
    pub roles: Vec<RoleBinding>,
    /// The run's unique key prefix. Redacted out of the projection: the two runs of a
    /// comparison SHARE a cluster and therefore get DIFFERENT prefixes, so without
    /// redaction every single key would diverge and the diff would be worthless.
    pub prefix: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBinding {
    pub role: String,
    pub driver: String,
    pub hello: Hello,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// The scenario's step id — stable across edits, unlike an index.
    ///
    /// Claims bind to ids, not positions, so that INSERTING a step cannot silently
    /// re-target a ledger claim at a different observation. A claim that quietly
    /// changes what it is about is worse than one that breaks.
    pub id: String,
    pub role: String,
    /// The op name, for readability in the trace.
    pub op: String,
    pub observation: Observation,
}

impl Trace {
    /// Every reason this trace cannot settle a claim.
    ///
    /// Checked BEFORE any diff. A trace with an inadmissible step is not "a
    /// divergence" — it is not evidence at all, and treating it as one is how a
    /// broken harness masquerades as a confirmed finding.
    pub fn inadmissible(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter_map(|s| {
                s.observation
                    .class
                    .inadmissible_reason()
                    .map(|why| format!("step `{}` ({}): {why}", s.id, s.op))
            })
            .collect()
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Class;

    fn trace_with(steps: Vec<Step>) -> Trace {
        Trace {
            schema: TRACE_SCHEMA.to_owned(),
            scenario: "s".to_owned(),
            run: "subject".to_owned(),
            roles: vec![],
            prefix: "p/".to_owned(),
            steps,
        }
    }

    fn step(id: &str, class: Class) -> Step {
        Step {
            id: id.to_owned(),
            role: "reader".to_owned(),
            op: "get".to_owned(),
            observation: Observation::new(class),
        }
    }

    #[test]
    fn a_clean_trace_is_admissible() {
        let t = trace_with(vec![step("a", Class::Ok), step("b", Class::NotFound)]);
        assert!(t.inadmissible().is_empty());
    }

    #[test]
    fn an_unmapped_error_makes_the_whole_trace_inadmissible() {
        let t = trace_with(vec![
            step("a", Class::Ok),
            step(
                "b",
                Class::Internal {
                    detail: "surprise".to_owned(),
                },
            ),
        ]);
        let bad = t.inadmissible();
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("step `b`"), "{:?}", bad);
    }

    #[test]
    fn a_region_error_makes_the_run_evidence_of_nothing() {
        // A region error is a CLUSTER event. It says nothing about either client, so
        // the runner must retry rather than report a divergence.
        let t = trace_with(vec![step("a", Class::RegionError)]);
        assert_eq!(t.inadmissible().len(), 1);
    }

    #[test]
    fn unsupported_does_not_taint_a_trace() {
        // The capability inventory depends on this: `unsupported` vs `ok` IS the
        // finding, so a trace full of them must still be admissible.
        let t = trace_with(vec![step(
            "a",
            Class::Unsupported {
                detail: "no checksum".to_owned(),
            },
        )]);
        assert!(t.inadmissible().is_empty());
    }
}
