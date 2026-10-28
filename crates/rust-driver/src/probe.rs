//! The driver code that depends on a client-rust API the pin and `upstream_base` spell
//! differently, and the switch each piece costs.
//!
//! ADR-0001 confines such code to one module — this one — and caps the switches at two.
//! Both slots are taken:
//!
//! - **`prewrite_only`**, through `Transaction::probe()` behind client-rust's `test-utils`
//!   feature (getwyrd/client-rust `test-probes`, offered upstream as a draft). The fork's
//!   `integration` has it; upstream `master` does not yet.
//!   - built with `--features rust-driver/test-probes,tikv-client/test-utils` (the
//!     Makefile's default, `PROBE_FEATURES`): `prewrite_only` drives the probe, and
//!     `hello` lists it;
//!   - built without them (CI's upstream view only): `prewrite_only` answers
//!     `unsupported`, as it did before the probe existed.
//!
//!   The features are named on the command line because Cargo rejects a manifest that
//!   names `tikv-client/test-utils` when the client checkout lacks it, even behind a
//!   feature that is off. Delete the switch, keeping the probe arm, once `upstream_base`
//!   contains the probe.
//!
//! - **`raw_checksum`**, through `RawClient::checksum` (getwyrd/client-rust#6, the
//!   close-gap for G-0002). The fork's `integration` has it from 385a1a3; upstream
//!   `master` does not.
//!   - built without `pre-raw-checksum` (the default, and every gating build):
//!     `raw_checksum` sends `kvrpcpb.RawChecksumRequest` through the client, and
//!     `hello` lists it;
//!   - built with `--features rust-driver/pre-raw-checksum` (CI's upstream view only):
//!     `raw_checksum` answers `unsupported` with no request sent, as it did before the
//!     method existed, and `hello` does not list it.
//!
//!   Retiring this switch is a MANUAL step of the pin move that brings
//!   `RawClient::checksum` into `upstream_base`: delete the stand-in below, the
//!   `pre-raw-checksum` feature in `Cargo.toml`, and its `--features` in the upstream-view
//!   job of `.github/workflows/ci.yml`. Nothing fires on its own. The stand-in names only
//!   `RawClient`, which both trees have, so the feature build keeps compiling — and keeps
//!   answering `unsupported` — after upstream gains the method; that is unlike `pre-557`
//!   (ADR-0001), which named a variant upstream removed. Until it is retired, the
//!   upstream view's parity diff keeps showing G-0002 as `ok` versus `unsupported`.
//!
//! Neither stand-in emulates anything: `unsupported` with a detail string is the whole
//! answer (ADR-0003). In both builds the caller looks its session or client up BEFORE
//! calling in here, so an unknown one is a `driver_error` whatever the switch says
//! (parity-protocol v1 §4.3 rule 4).

#[cfg(not(feature = "pre-raw-checksum"))]
use parity_proto::ChecksumObs;
use parity_proto::Observation;
use tikv_client::RawClient;
#[cfg(feature = "test-probes")]
use tikv_client::TimestampExt;
use tikv_client::Transaction;

#[cfg(not(feature = "pre-raw-checksum"))]
use crate::mapping::classify;

/// The same TTL the Go driver sets (`orphanLockTTLms`), so the setup is held constant
/// across both runs: the reader meets a lock that has genuinely expired.
#[cfg(feature = "test-probes")]
const ORPHAN_LOCK_TTL_MS: u64 = 1000;

/// How long `prewrite_only` waits for TiKV. A timeout is a `driver_error`: the setup did
/// not happen, so the run is inadmissible.
#[cfg(feature = "test-probes")]
const PREWRITE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// How long `raw_checksum` waits for the client. The runner waits on every reply with no
/// deadline of its own, so an await that never returns would stall the whole run. Giving
/// up is a `driver_error` — no answer is not an answer — never a class the client did not
/// produce.
#[cfg(not(feature = "pre-raw-checksum"))]
const RAW_CHECKSUM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// What `hello` lists beyond the always-present capabilities: each switched command
/// whose real call is compiled into this build.
pub fn features() -> Vec<String> {
    let switched = [
        ("prewrite_only", cfg!(feature = "test-probes")),
        ("raw_checksum", !cfg!(feature = "pre-raw-checksum")),
    ];
    switched
        .into_iter()
        .filter(|(_, compiled_in)| *compiled_in)
        .map(|(name, _)| name.to_owned())
        .collect()
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
    // A prewrite TiKV accepts but never answers must not stall the run: the runner waits
    // on this reply with no deadline of its own.
    match tokio::time::timeout(PREWRITE_DEADLINE, probe.prewrite(keys)).await {
        Ok(Ok(_)) => Observation::ok().with_start_ts(start_ts),
        Ok(Err(e)) => Observation::driver_error(format!("prewrite_only: {e:?}")),
        Err(_) => Observation::driver_error(format!(
            "prewrite_only: no answer within {PREWRITE_DEADLINE:?}"
        )),
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

/// Checksum the raw range `[start, end)` server-side, as the Go driver's arm does with
/// `rawkv.Checksum` (`go/driver/exec.go`): `ok` carrying the client's `RawChecksum` as
/// evidence, or the client's error classified as every other raw arm classifies it.
///
/// The numbers are recorded, never compared: they cover the key bytes, run prefix
/// included, so they legitimately differ between the two runs of one comparison
/// (`parity_proto::Observation::checksum`).
#[cfg(not(feature = "pre-raw-checksum"))]
pub async fn raw_checksum(client: &RawClient, start: Vec<u8>, end: Vec<u8>) -> Observation {
    match tokio::time::timeout(RAW_CHECKSUM_DEADLINE, client.checksum(start..end)).await {
        Ok(Ok(sum)) => Observation::ok().with_checksum(ChecksumObs {
            crc64_xor: sum.crc64_xor,
            total_kvs: sum.total_kvs,
            total_bytes: sum.total_bytes,
        }),
        Ok(Err(e)) => classify(&e),
        Err(_) => Observation::driver_error(format!(
            "raw_checksum: no answer within {RAW_CHECKSUM_DEADLINE:?}"
        )),
    }
}

/// Built for a client-rust without `RawClient::checksum` (upstream `master`): the answer
/// the driver gave before getwyrd/client-rust#6, detail string included, so the upstream
/// view's diff for G-0002 stays the one the open gap declared.
#[cfg(feature = "pre-raw-checksum")]
pub async fn raw_checksum(_client: &RawClient, _start: Vec<u8>, _end: Vec<u8>) -> Observation {
    Observation::unsupported(
        "client-rust has no raw checksum: tikv-client implements no RawChecksum \
         request. client-go's rawkv.Checksum returns crc64_xor/total_kvs/\
         total_bytes computed server-side. The driver does NOT emulate it: the \
         gap is the finding (G-0002).",
    )
}
