//! `prewrite_only` through client-rust's test probe, and the one switch it costs.
//!
//! client-rust gained `Transaction::probe()` behind its `test-utils` feature
//! (getwyrd/client-rust `test-probes`, offered upstream as a draft). The fork's
//! `integration` has it; upstream `master` does not yet, so the upstream view cannot
//! build a driver that calls it. ADR-0001 confines such a switch to one module: this one.
//!
//! - built with `--features rust-driver/test-probes,tikv-client/test-utils` (the
//!   Makefile's default, `PROBE_FEATURES`): `prewrite_only` drives the probe, and
//!   `hello` lists it;
//! - built without them (CI's upstream view only): `prewrite_only` answers
//!   `unsupported`, as it did before the probe existed.
//!
//! The features are named on the command line because Cargo rejects a manifest that
//! names `tikv-client/test-utils` when the client checkout lacks it, even behind a
//! feature that is off. Delete the switch, keeping the probe arm, once `upstream_base`
//! contains the probe.

use parity_proto::Observation;
#[cfg(feature = "test-probes")]
use tikv_client::TimestampExt;
use tikv_client::Transaction;

/// The same TTL the Go driver sets (`orphanLockTTLms`), so the setup is held constant
/// across both runs: the reader meets a lock that has genuinely expired.
#[cfg(feature = "test-probes")]
const ORPHAN_LOCK_TTL_MS: u64 = 1000;

/// What `hello` lists beyond the always-present capabilities.
pub fn features() -> Vec<String> {
    if cfg!(feature = "test-probes") {
        vec!["prewrite_only".to_owned()]
    } else {
        Vec::new()
    }
}

/// Prewrite `keys` under `primary` and stop. `primary` is named but never prewritten:
/// that is what makes the locks an orphan rather than an in-flight commit.
///
/// A failed prewrite is a `driver_error`, never a client observation: the scenario's
/// setup did not happen, so the run is inadmissible rather than evidence.
#[cfg(feature = "test-probes")]
pub async fn prewrite_only(
    txn: &mut Transaction,
    primary: Vec<u8>,
    keys: Vec<Vec<u8>>,
) -> Observation {
    if keys.is_empty() {
        return Observation::driver_error("prewrite_only: no keys to prewrite");
    }
    let start_ts = txn.start_timestamp().version();
    let mut probe = txn.probe();
    probe
        .set_primary_key(primary)
        .set_lock_ttl(ORPHAN_LOCK_TTL_MS);
    match probe.prewrite(keys).await {
        Ok(_) => Observation::ok().with_start_ts(start_ts),
        Err(e) => Observation::driver_error(format!("prewrite_only: {e:?}")),
    }
}

#[cfg(not(feature = "test-probes"))]
pub async fn prewrite_only(
    _txn: &mut Transaction,
    _primary: Vec<u8>,
    _keys: Vec<Vec<u8>>,
) -> Observation {
    Observation::unsupported(
        "this client-rust has no test probe (Transaction::probe, feature test-utils), so a \
         caller cannot drive 2PC phase by phase. The driver does NOT emulate it.",
    )
}
