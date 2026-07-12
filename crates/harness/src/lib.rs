//! Client-agnostic fixtures for the parity harness.
//!
//! Everything here is true of *a TiKV cluster*, not of any particular client or
//! application contract. That is the dividing line: a suite that drives
//! `tikv-client`, a future runner that drives it differentially against client-go,
//! and the comparator between them all need `$PD_ADDRS` and the cluster's region
//! layout — none of them should have to agree on a `MetadataStore` to get it.
//!
//! Anything that speaks in terms of a specific contract (a store, a `WriteBatch`)
//! belongs to that suite, not here. See `crates/wyrd-gate/tests/common/mod.rs`.

/// PD's HTTP API as ground truth for the cluster's region layout.
pub mod cluster;

use std::env;

/// The cluster under test. Comma-separated, matching client-rust's own
/// integration idiom; defaults to the local docker-compose stack in `cluster/`.
pub fn pd_addrs() -> Vec<String> {
    env::var("PD_ADDRS")
        .unwrap_or_else(|_| "127.0.0.1:2379".to_owned())
        .split(',')
        .map(From::from)
        .collect()
}

/// `<prefix><suffix>` as a key.
pub fn key(prefix: &[u8], suffix: &str) -> Vec<u8> {
    let mut key = prefix.to_vec();
    key.extend_from_slice(suffix.as_bytes());
    key
}

/// A per-run key prefix, unique to the nanosecond.
///
/// Mandatory for any test that can leave a lock behind that the client cannot
/// resolve (the whole point of `d6`): under a fixed prefix such an orphan would
/// poison every later run's cleanup scan, and the failure would look like a bug in
/// a test that never touched it. Test code may read the clock; the store never does.
pub fn unique_prefix(test: &str) -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("gate/{test}/{nanos}/").into_bytes()
}

/// A tiny deterministic byte generator (xorshift64*) for value corpora — no
/// `rand` dependency, reproducible across runs.
pub fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.max(1);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.truncate(len);
    out
}
