//! Execute a scenario against a binding of roles to drivers, producing a Trace.
//!
//! The RUNNER owns the total order. Commands are synchronous request/response, so a
//! scenario's steps happen in exactly the order written and no client-internal
//! concurrency is observed unless a scenario asks for it. That is why the first
//! milestone needs no concurrency machinery at all — and why its results are not
//! timing-dependent.

use std::collections::BTreeMap;
use std::path::Path;

use parity_proto::Command;
use parity_proto::RoleBinding;
use parity_proto::Step as TraceStep;
use parity_proto::Trace;
use parity_proto::TRACE_SCHEMA;

use crate::driver::Driver;
use crate::scenario::Run;
use crate::scenario::Scenario;

/// A run that could not produce evidence, and why.
///
/// Distinct from a divergence ON PURPOSE. "The setup failed" and "the clients differ"
/// are different facts, and conflating them is how a broken harness gets mistaken for a
/// confirmed finding — the exact hazard `gate-verdict.sh`'s WRONG-FAILURE rule exists
/// to catch.
#[derive(Debug)]
pub struct Inadmissible {
    pub run: String,
    pub why: String,
}

pub struct Binaries {
    pub rust: std::path::PathBuf,
    pub go: std::path::PathBuf,
}

impl Binaries {
    fn get(&self, driver: &str) -> Result<&Path, String> {
        match driver {
            "rust" => Ok(&self.rust),
            "go" => Ok(&self.go),
            other => Err(format!("unknown driver `{other}` (expected rust|go)")),
        }
    }
}

/// Execute one run of a scenario.
pub fn execute(
    scenario: &Scenario,
    run: &Run,
    bins: &Binaries,
) -> Result<Result<Trace, Inadmissible>, String> {
    // A per-run key prefix, unique to the nanosecond. MANDATORY: the two runs of a
    // comparison share a cluster, so without distinct prefixes the oracle run's
    // residue (an orphaned lock it deliberately left behind!) would poison the
    // subject run, and the failure would look like a bug in a client that never
    // touched the key.
    //
    // `project.rs` redacts the prefix back out, so distinct prefixes cost nothing in
    // comparability.
    let prefix = String::from_utf8(harness::unique_prefix(&scenario.name))
        .map_err(|e| format!("prefix is not utf-8: {e}"))?;

    // One driver process per role.
    let mut drivers: BTreeMap<String, Driver> = BTreeMap::new();
    for role in &scenario.roles {
        let driver_name = run
            .bind
            .get(role)
            .ok_or_else(|| format!("run `{}` does not bind role `{role}`", run.name))?;
        let bin = bins.get(driver_name)?;
        drivers.insert(role.clone(), Driver::spawn(driver_name, bin)?);
    }

    let roles: Vec<RoleBinding> = scenario
        .roles
        .iter()
        .map(|r| {
            let d = &drivers[r];
            RoleBinding {
                role: r.clone(),
                driver: d.hello.driver.clone(),
                hello: d.hello.clone(),
            }
        })
        .collect();

    let mut steps: Vec<TraceStep> = Vec::new();

    for step in &scenario.steps {
        // ── Runner-local ops ─────────────────────────────────────────────────
        // `sleep` is the runner's, not a driver's: it is about the CLUSTER's clock
        // (a lock TTL expiring), not about either client. Every sleep must say why,
        // so a reader can tell a principled wait from a superstitious one.
        if step.cmd.get("op").and_then(|o| o.as_str()) == Some("sleep") {
            let ms = step
                .cmd
                .get("ms")
                .and_then(|m| m.as_u64())
                .ok_or_else(|| format!("step `{}`: sleep needs `ms`", step.id))?;
            std::thread::sleep(std::time::Duration::from_millis(ms));
            continue;
        }

        let role = step
            .role
            .as_ref()
            .ok_or_else(|| format!("step `{}`: only `sleep` may omit a role", step.id))?;
        let driver = drivers
            .get_mut(role)
            .ok_or_else(|| format!("step `{}`: no driver for role `{role}`", step.id))?;

        // Substitute the run's key prefix into every utf-8 key argument.
        let cmd: Command = serde_json::from_value(step.cmd.clone())
            .map_err(|e| format!("step `{}`: bad command: {e}", step.id))?;
        let cmd = substitute(&cmd, &prefix);

        let resp = driver.send(&cmd)?;
        let obs = resp
            .observation
            .ok_or_else(|| format!("step `{}`: driver returned no observation", step.id))?;

        // ── The precondition gate ────────────────────────────────────────────
        // A failed assert kills the RUN, immediately. It is not a divergence and it
        // is not evidence: if the orphan was never manufactured, then "the reader
        // could not resolve it" says nothing at all. A vacuous green is the one
        // outcome worse than a red.
        if let Some(assert) = &step.assert {
            if let Some(why) = assert.check(&obs) {
                return Ok(Err(Inadmissible {
                    run: run.name.clone(),
                    why: format!("step `{}` precondition failed: {why}", step.id),
                }));
            }
        }

        steps.push(TraceStep {
            id: step.id.clone(),
            role: role.clone(),
            op: cmd_op(&step.cmd),
            observation: obs,
        });
    }

    let trace = Trace {
        schema: TRACE_SCHEMA.to_owned(),
        scenario: scenario.name.clone(),
        run: run.name.clone(),
        roles,
        prefix,
        steps,
    };

    // A driver_error / internal / region_error anywhere makes the whole run evidence
    // of nothing. Checked here rather than at diff time so the reason is reported as
    // itself, not as a mysterious field disagreement.
    let bad = trace.inadmissible();
    if !bad.is_empty() {
        return Ok(Err(Inadmissible {
            run: run.name.clone(),
            why: bad.join("; "),
        }));
    }

    Ok(Ok(trace))
}

fn cmd_op(cmd: &serde_json::Value) -> String {
    cmd.get("op")
        .and_then(|o| o.as_str())
        .unwrap_or("?")
        .to_owned()
}

/// Substitute `{P}` in every key-ish argument of a command.
fn substitute(cmd: &Command, prefix: &str) -> Command {
    use Command::*;
    match cmd {
        Put {
            session,
            key,
            value,
        } => Put {
            session: session.clone(),
            key: key.substitute(prefix),
            value: value.substitute(prefix),
        },
        Get { session, key } => Get {
            session: session.clone(),
            key: key.substitute(prefix),
        },
        SnapshotGet { client, key } => SnapshotGet {
            client: client.clone(),
            key: key.substitute(prefix),
        },
        ScanLocks {
            client,
            start,
            end,
            batch_size,
        } => ScanLocks {
            client: client.clone(),
            start: start.substitute(prefix),
            end: end.substitute(prefix),
            batch_size: *batch_size,
        },
        PrewriteOnly {
            session,
            primary,
            keys,
        } => PrewriteOnly {
            session: session.clone(),
            primary: primary.substitute(prefix),
            keys: keys.iter().map(|k| k.substitute(prefix)).collect(),
        },
        other => other.clone(),
    }
}
