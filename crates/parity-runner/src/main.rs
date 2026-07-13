//! THE PARITY RUNNER — drives both clients through one scenario and diffs them.
//!
//! NOTE WHAT THIS CRATE DOES NOT DEPEND ON: `tikv-client`. The thing that decides a
//! verdict must not be able to reach for the subject under test. That is enforced in
//! Cargo.toml (and by `parity-proto` carrying no client dependency either), not merely
//! intended.

mod driver;
mod run;
mod scenario;

use std::path::PathBuf;

use parity_proto::project;
use parity_proto::Spec;

use crate::run::Binaries;
use crate::scenario::Scenario;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let scenarios: Vec<PathBuf> = if args.len() > 1 {
        args[1..].iter().map(PathBuf::from).collect()
    } else {
        // Default: every scenario in scenarios/.
        let mut v: Vec<PathBuf> = std::fs::read_dir("scenarios")
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };

    if scenarios.is_empty() {
        eprintln!("no scenarios to run");
        std::process::exit(1);
    }

    let bins = Binaries {
        rust: PathBuf::from(
            std::env::var("PARITY_DRIVER_RUST")
                .unwrap_or_else(|_| "target/debug/parity-driver-rust".to_owned()),
        ),
        go: PathBuf::from(
            std::env::var("PARITY_DRIVER_GO")
                .unwrap_or_else(|_| "target/parity-driver-go".to_owned()),
        ),
    };

    // Read the world ONCE, before anything runs, and stamp it into every artifact. A
    // trace that cannot say what it was produced against cannot be adjudicated later.
    let provenance = match run::load_provenance() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\nRUNNER ERROR: {e}");
            std::process::exit(1);
        }
    };

    let mut failed = false;
    for path in &scenarios {
        match run_scenario(path, &bins, &provenance) {
            Ok(true) => {}
            Ok(false) => failed = true,
            Err(e) => {
                eprintln!("\nRUNNER ERROR in {}: {e}", path.display());
                failed = true;
            }
        }
    }

    std::process::exit(if failed { 1 } else { 0 });
}

/// Returns Ok(true) if the scenario produced an admissible result.
///
/// NOTE: this reports the DIFF. It does not decide whether the diff is expected — that
/// is the ledger's job (`scripts/ledger-check.sh`). Keeping "what happened" separate
/// from "was that what we predicted" is the whole point: a runner that decided its own
/// verdict could not be checked against a declared expectation.
fn run_scenario(
    path: &std::path::Path,
    bins: &Binaries,
    provenance: &serde_json::Value,
) -> Result<bool, String> {
    let scenario = Scenario::load(path)?;
    println!("\n═══ {} ═══", scenario.name);
    println!("{}", scenario.doc.trim());

    let out_dir = PathBuf::from("results/traces");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir results/traces: {e}"))?;

    let mut traces = std::collections::BTreeMap::new();

    for run in &scenario.runs {
        let bind: Vec<String> = scenario
            .roles
            .iter()
            .map(|r| format!("{r}={}", run.bind[r]))
            .collect();
        println!("\n── run `{}` [{}]", run.name, bind.join(" "));

        match run::execute(&scenario, run, bins, provenance)? {
            Ok(trace) => {
                let file = out_dir.join(format!("{}.{}.json", scenario.name, run.name));
                std::fs::write(
                    &file,
                    serde_json::to_string_pretty(&trace).map_err(|e| e.to_string())?,
                )
                .map_err(|e| format!("write {}: {e}", file.display()))?;
                println!("   {} steps -> {}", trace.steps.len(), file.display());
                traces.insert(run.name.clone(), trace);
            }
            Err(bad) => {
                // INADMISSIBLE — not a divergence. Say so in those words, because the
                // difference between "the harness broke" and "the clients differ" is
                // the difference between noise and a finding.
                println!(
                    "\n   INADMISSIBLE — this run is evidence of nothing.\n   {}\n",
                    bad.why
                );
                eprintln!(
                    "INADMISSIBLE: scenario `{}` run `{}`: {}",
                    scenario.name, bad.run, bad.why
                );
                return Ok(false);
            }
        }
    }

    // ── The diff ─────────────────────────────────────────────────────────────
    let spec = Spec {
        also_compare: scenario.also_compare.clone(),
    };
    let (a, b) = (&scenario.compare[0], &scenario.compare[1]);
    let ta = traces.get(a).ok_or_else(|| format!("no trace for `{a}`"))?;
    let tb = traces.get(b).ok_or_else(|| format!("no trace for `{b}`"))?;

    let divergences = parity_proto::diff(&project(ta, &spec), &project(tb, &spec));

    println!("\n── diff: {a} (oracle) vs {b} (subject)");
    if divergences.is_empty() {
        println!("   NO DIVERGENCE — the two clients agree on every compared field.");
    } else {
        for d in &divergences {
            println!(
                "   {} · {}\n       {a:>8}: {}\n       {b:>8}: {}",
                d.step, d.path, d.oracle, d.subject
            );
        }
    }

    // The divergence report is an artifact in its own right: it is what a ledger claim is
    // checked against, and what an upstream issue quotes. It carries its OWN provenance,
    // so `ledger-check` adjudicates the world this result was produced in rather than the
    // world it happens to be adjudicated in.
    let report = serde_json::json!({
        "schema": "parity-divergence/v1",
        "scenario": scenario.name,
        "gap": scenario.gap,
        "provenance": provenance,
        "oracle": a,
        "subject": b,
        "divergences": divergences,
    });
    let file = PathBuf::from("results").join(format!("divergence.{}.json", scenario.name));
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", file.display()))?;
    println!("   -> {}", file.display());

    Ok(true)
}
