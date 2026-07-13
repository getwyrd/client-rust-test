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

/// The world a run happened in, read ONCE before any scenario executes and stamped into
/// every artifact it produces.
///
/// `scripts/provenance.sh` writes this; `make parity` runs it first. The runner refuses
/// to produce evidence without it, because a trace that does not say what it was gathered
/// against cannot be adjudicated later — and re-deriving provenance at adjudication time
/// would describe the wrong moment entirely (see `Trace::provenance`).
pub fn load_provenance() -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string("results/provenance.json").map_err(|e| {
        format!(
            "cannot read results/provenance.json ({e}). Run `make provenance` first: a run \
             that cannot say what world it was produced in is not evidence."
        )
    })?;
    serde_json::from_str(&raw)
        .map_err(|e| format!("results/provenance.json is not valid JSON: {e}"))
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
    provenance: &serde_json::Value,
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
        // The world this run happened in, bound to the artifact it produced.
        provenance: provenance.clone(),
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

/// Namespace every argument of a command under the run's key prefix.
///
/// KEY position and VALUE position are treated differently, and the distinction is
/// load-bearing: a key MUST be namespaced (the two runs share a cluster, and the oracle
/// deliberately leaves residue), while a binary value MUST NOT be touched (values are
/// opaque bytes that have to round-trip byte-identically). This match is the only place
/// that knows which field is which, so it is the only place that can get it right.
fn substitute(cmd: &Command, prefix: &str) -> Command {
    use Command::*;
    match cmd {
        Put {
            session,
            key,
            value,
        } => Put {
            session: session.clone(),
            key: key.as_key(prefix),
            value: value.as_value(prefix),
        },
        Get { session, key } => Get {
            session: session.clone(),
            key: key.as_key(prefix),
        },
        SnapshotGet { client, key } => SnapshotGet {
            client: client.clone(),
            key: key.as_key(prefix),
        },
        ScanLocks {
            client,
            start,
            end,
            batch_size,
        } => ScanLocks {
            client: client.clone(),
            // Range bounds are keys: they must bracket THIS run's prefix, or a scan would
            // observe the other run's locks and report them as this client's residue.
            start: start.as_key(prefix),
            end: end.as_key(prefix),
            batch_size: *batch_size,
        },
        PrewriteOnly {
            session,
            primary,
            keys,
        } => PrewriteOnly {
            session: session.clone(),
            primary: primary.as_key(prefix),
            keys: keys.iter().map(|k| k.as_key(prefix)).collect(),
        },
        other => other.clone(),
    }
}
