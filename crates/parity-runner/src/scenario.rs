//! SCENARIOS — declarative JSON, and deliberately not a programming language.
//!
//! **No conditionals. No loops. No variables. No expression language.** If a scenario
//! needs control flow, it is a Rust test, not a scenario. This is the piece most likely
//! to rot into a bad programming language, and the line is held here, on purpose.
//!
//! A scenario names ROLES (`orphaner`, `reader`), binds them to drivers in one or more
//! RUNS, and lists STEPS. Because both drivers speak the same protocol, the same steps
//! can run all-Go (the oracle), all-Rust, or with roles split across clients — which is
//! what makes cross-client interop expressible at all.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;

pub const SCENARIO_SCHEMA: &str = "parity-scenario/v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Scenario {
    pub schema: String,
    pub name: String,
    /// What this scenario proves, and why the shape is what it is. Prose, for a human.
    pub doc: String,
    /// The gap id in ledger.toml this scenario is evidence for, if any.
    #[serde(default)]
    pub gap: Option<String>,
    pub roles: Vec<String>,
    pub runs: Vec<Run>,
    /// The two run names to diff, e.g. `["oracle", "subject"]`.
    pub compare: Vec<String>,
    /// Extra projection paths this scenario's claim needs, e.g. `native.type`.
    #[serde(default)]
    pub also_compare: Vec<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Run {
    pub name: String,
    /// role → driver ("rust" | "go").
    pub bind: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Step {
    /// Stable id. Claims bind to IDS, never to positions — so inserting a step cannot
    /// silently re-target a ledger claim at a different observation. A claim that
    /// quietly changes what it is about is worse than one that breaks loudly.
    pub id: String,
    /// The role that executes this. Absent for runner-local ops (`sleep`).
    #[serde(default)]
    pub role: Option<String>,
    /// The command, verbatim — a `parity_proto::Command`, or `{"op":"sleep","ms":N}`.
    pub cmd: serde_json::Value,
    /// A HARNESS PRECONDITION, never a parity claim.
    ///
    /// A failed assert makes the whole run INADMISSIBLE — not a divergence, not
    /// evidence. This is `gate-verdict.sh`'s WRONG-FAILURE rule hoisted into the
    /// scenario: if the setup did not hold, the run proves nothing, and must not be
    /// allowed to masquerade as a confirmed finding.
    ///
    /// It is also what lets `gate::d6` retire its PRECONDITION-FAILED panic: the
    /// precondition is checked HERE, once, rather than conflated with the assertion.
    #[serde(default)]
    pub assert: Option<Assert>,
    /// Why this step exists. Especially load-bearing on `sleep`.
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Assert {
    /// The exact number of locks `scan_locks` must have returned.
    #[serde(default)]
    pub lock_count: Option<usize>,
    /// The class the observation must have had.
    #[serde(default)]
    pub class: Option<String>,
}

impl Assert {
    /// Returns why the precondition failed, or None if it held.
    pub fn check(&self, obs: &parity_proto::Observation) -> Option<String> {
        if let Some(want) = self.lock_count {
            let got = obs.locks.len();
            if got != want {
                return Some(format!(
                    "expected {want} lock(s), observed {got}. \
                     The scenario's SETUP did not hold, so nothing downstream of it is \
                     evidence about either client."
                ));
            }
        }
        if let Some(want) = &self.class {
            let got = obs.class.tag();
            if got != want {
                return Some(format!(
                    "expected class `{want}`, observed `{got}`. \
                     The scenario's SETUP did not hold, so nothing downstream of it is \
                     evidence about either client."
                ));
            }
        }
        None
    }
}

impl Scenario {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let s: Scenario = serde_json::from_str(&raw)
            .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        s.validate()?;
        Ok(s)
    }

    /// Catch, statically, the mistakes that would otherwise become a mysterious runtime
    /// failure or — far worse — a vacuous pass.
    fn validate(&self) -> Result<(), String> {
        if self.schema != SCENARIO_SCHEMA {
            return Err(format!(
                "unknown scenario schema `{}` (expected `{SCENARIO_SCHEMA}`)",
                self.schema
            ));
        }
        if self.compare.len() != 2 {
            return Err(format!(
                "`compare` must name exactly 2 runs, got {:?}",
                self.compare
            ));
        }
        // A scenario comparing a run WITH ITSELF diffs one trace against itself, which is
        // empty by construction. For an `expect = "agrees"` ledger entry that is a free
        // green: the claim holds without the subject having been exercised at all. A
        // comparison that cannot fail is not a comparison.
        if self.compare[0] == self.compare[1] {
            return Err(format!(
                "`compare` names the run `{}` twice. Diffing a run against itself is empty \
                 by construction — it would pass an `agrees` claim without ever exercising \
                 the subject.",
                self.compare[0]
            ));
        }
        // Two runs sharing a name would make `compare` ambiguous and let the wrong trace
        // be adjudicated.
        let mut seen = std::collections::HashSet::new();
        for run in &self.runs {
            if !seen.insert(&run.name) {
                return Err(format!("duplicate run name `{}`", run.name));
            }
        }
        // Distinct NAMES are not enough. Two runs can be named differently and still bind
        // every role to the same driver — comparing, say, all-Go against all-Go. The diff
        // is then empty for a reason that has nothing to do with either client, and an
        // `expect = "agrees"` claim passes without client-rust ever being exercised. This
        // is the self-comparison hole wearing a different name, so it is closed here too:
        // the two compared runs must actually CONTRAST something.
        //
        // The runs are looked up (not `expect`ed) because `compare` naming an undefined
        // run is a scenario error, and it must be REPORTED, not panicked on.
        let (a, b) = (&self.compare[0], &self.compare[1]);
        let find = |want: &String| {
            self.runs
                .iter()
                .find(|r| &r.name == want)
                .ok_or_else(|| format!("`compare` names run `{want}`, which is not defined"))
        };
        let bind_a = &find(a)?.bind;
        let bind_b = &find(b)?.bind;
        if bind_a == bind_b {
            return Err(format!(
                "runs `{a}` and `{b}` bind every role to the same driver ({bind_a:?}), so \
                 comparing them contrasts nothing. The diff would be empty for a reason \
                 unrelated to either client, and an `agrees` claim would pass without the \
                 subject ever being exercised."
            ));
        }
        // An unknown opt-in path must fail HERE, loudly, before a single command runs.
        // Left to diff time it would compare nothing while looking like it compared
        // something — the vacuous pass this whole harness exists to make impossible.
        parity_proto::Spec {
            also_compare: self.also_compare.clone(),
        }
        .validate()?;
        // (`compare` naming an undefined run is caught by `find` above.)

        // Every step must name a role the scenario declares, and every run must bind
        // every role. A step whose role is unbound would silently never execute.
        let mut ids = std::collections::HashSet::new();
        for step in &self.steps {
            if !ids.insert(&step.id) {
                return Err(format!("duplicate step id `{}`", step.id));
            }
            if let Some(role) = &step.role {
                if !self.roles.contains(role) {
                    return Err(format!(
                        "step `{}` names role `{role}`, which is not in `roles`",
                        step.id
                    ));
                }
            }

            // Parse every command NOW, and check every byte-string argument. A command
            // that only fails to parse mid-run wastes a cluster round trip; a malformed
            // base64 argument is worse than that — it decodes to EMPTY bytes, both
            // drivers agree on the empty key, and the ledger certifies a scenario nobody
            // wrote. (`sleep` is the runner's own op and is not a driver command.)
            let op = step.cmd.get("op").and_then(|o| o.as_str());

            // `hello` is the RUNNER's handshake, not a scenario step: the driver answers
            // it with a `hello`, not an observation, so a step using it would deserialize
            // fine, pass every static check, and then abort the run at execution time
            // ("driver returned no observation"). A statically valid scenario must not be
            // able to fail for a protocol reason it cannot see.
            if op == Some("hello") {
                return Err(format!(
                    "step `{}`: `hello` is the runner's handshake, not a scenario step. \
                     It returns no observation, so the run would abort.",
                    step.id
                ));
            }

            if op != Some("sleep") {
                let cmd: parity_proto::Command = serde_json::from_value(step.cmd.clone())
                    .map_err(|e| format!("step `{}`: bad command: {e}", step.id))?;
                for arg in cmd.args() {
                    arg.validate()
                        .map_err(|e| format!("step `{}`: {e}", step.id))?;
                }
            }
        }
        for run in &self.runs {
            for role in &self.roles {
                if !run.bind.contains_key(role) {
                    return Err(format!("run `{}` does not bind role `{role}`", run.name));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario_json(compare: &str, roles: &str, steps: &str, runs: &str) -> String {
        format!(
            r#"{{"schema":"parity-scenario/v1","name":"n","doc":"d",
                 "roles":{roles},"runs":{runs},"compare":{compare},"steps":{steps}}}"#
        )
    }

    fn parse(s: &str) -> Result<Scenario, String> {
        let sc: Scenario = serde_json::from_str(s).map_err(|e| e.to_string())?;
        sc.validate()?;
        Ok(sc)
    }

    const RUNS: &str =
        r#"[{"name":"oracle","bind":{"r":"go"}},{"name":"subject","bind":{"r":"rust"}}]"#;

    #[test]
    fn a_valid_scenario_parses() {
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            RUNS,
        );
        assert!(parse(&s).is_ok());
    }

    #[test]
    fn a_step_naming_an_undeclared_role_is_rejected() {
        // Otherwise the step would silently never execute, and the scenario would
        // "pass" without having tested anything.
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"ghost","cmd":{"op":"open_client","name":"c"}}]"#,
            RUNS,
        );
        assert!(parse(&s).unwrap_err().contains("ghost"));
    }

    #[test]
    fn a_run_that_does_not_bind_every_role_is_rejected() {
        let runs = r#"[{"name":"oracle","bind":{}},{"name":"subject","bind":{"r":"rust"}}]"#;
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            runs,
        );
        assert!(parse(&s).unwrap_err().contains("does not bind role"));
    }

    #[test]
    fn a_hello_step_is_rejected() {
        // `hello` is the runner's handshake. It deserializes as a valid Command and passed
        // every static check, but the driver answers it with a `hello` rather than an
        // observation — so the run aborted at execution time. A statically valid scenario
        // must not be able to fail for a protocol reason its author cannot see.
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"hello"}}]"#,
            RUNS,
        );
        assert!(parse(&s).unwrap_err().contains("handshake"));
    }

    #[test]
    fn comparing_an_undefined_run_errors_rather_than_panics() {
        // The binding check looks these up; a missing run must be REPORTED, not unwrapped.
        let s = scenario_json(
            r#"["oracle","ghost"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            RUNS,
        );
        let err = parse(&s).unwrap_err();
        assert!(
            err.contains("ghost") && err.contains("not defined"),
            "{err}"
        );
    }

    #[test]
    fn malformed_base64_in_a_scenario_is_rejected() {
        // A bad b64 decoded to EMPTY bytes, so both drivers would agree on a prefix-only
        // key and the ledger would certify a scenario nobody wrote — a green for a test
        // that never happened. A typo must be loud.
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"get","session":"t","key":{"b64":"!!!"}}}]"#,
            RUNS,
        );
        let err = parse(&s).unwrap_err();
        assert!(err.contains("base64"), "{err}");
    }

    #[test]
    fn two_runs_with_identical_bindings_cannot_be_compared() {
        // Distinct NAMES are not enough: two all-Go runs contrast nothing, so an
        // `agrees` claim would pass without client-rust ever being exercised.
        let runs = r#"[{"name":"oracle","bind":{"r":"go"}},{"name":"subject","bind":{"r":"go"}}]"#;
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            runs,
        );
        let err = parse(&s).unwrap_err();
        assert!(err.contains("contrasts nothing"), "{err}");
    }

    #[test]
    fn comparing_a_run_with_itself_is_rejected() {
        // A self-comparison diffs a trace against itself: empty by construction. For an
        // `expect = "agrees"` claim that is a free green — the claim would hold without
        // the subject ever being run. A comparison that cannot fail is not a comparison.
        let s = scenario_json(
            r#"["oracle","oracle"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            RUNS,
        );
        assert!(parse(&s).unwrap_err().contains("twice"));
    }

    #[test]
    fn duplicate_run_names_are_rejected() {
        let runs = r#"[{"name":"oracle","bind":{"r":"go"}},{"name":"oracle","bind":{"r":"rust"}}]"#;
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            runs,
        );
        assert!(parse(&s).is_err());
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        // Claims bind to step ids. Two steps with one id would make a claim ambiguous.
        let s = scenario_json(
            r#"["oracle","subject"]"#,
            r#"["r"]"#,
            r#"[{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}},{"id":"a","role":"r","cmd":{"op":"open_client","name":"c"}}]"#,
            RUNS,
        );
        assert!(parse(&s).unwrap_err().contains("duplicate step id"));
    }

    #[test]
    fn a_failed_lock_count_assert_explains_that_the_setup_did_not_hold() {
        let a = Assert {
            lock_count: Some(1),
            class: None,
        };
        let obs = parity_proto::Observation::ok(); // zero locks
        let why = a.check(&obs).expect("must fail");
        assert!(why.contains("expected 1 lock"));
        assert!(why.contains("SETUP did not hold"));
    }
}
