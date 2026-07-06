//! Shared helpers for the gate tests. Mirrors client-rust's own integration
//! idiom: `$PD_ADDRS` (comma separated) selects the cluster, defaulting to the
//! local docker-compose stack in `cluster/`.

#![allow(dead_code)]

use std::env;

use client_rust_test::traits::CommitOutcome;
use client_rust_test::traits::MetadataStore;
use client_rust_test::traits::WriteBatch;
use client_rust_test::LockMode;
use client_rust_test::TikvMetadataStore;

pub fn pd_addrs() -> Vec<String> {
    env::var("PD_ADDRS")
        .unwrap_or_else(|_| "127.0.0.1:2379".to_owned())
        .split(',')
        .map(From::from)
        .collect()
}

/// Connect a store in `mode`, panicking with a usable hint if the cluster is
/// not reachable.
pub async fn store(mode: LockMode) -> TikvMetadataStore {
    let _ = env_logger::try_init();
    TikvMetadataStore::connect(pd_addrs(), mode)
        .await
        .expect("connect to TiKV — is the cluster up? (`make cluster-up`)")
}

/// Remove everything under `prefix` through the store itself (never the raw
/// API — the gate keeps to the transactional surface M4 will use). Each test
/// owns a unique prefix and wipes it first, so tests are rerun-safe and can
/// run in parallel.
pub async fn wipe(store: &TikvMetadataStore, prefix: &[u8]) {
    // Relies on client-rust: `scan` resolves any lock it encounters through
    // the client's own lock resolution, then a delete batch clears the data.
    // The harness does not sweep locks itself (see d6 / the README finding —
    // the fix for unresolvable orphans belongs in client-rust). Each test
    // owns a disjoint prefix, so a lingering orphan under one prefix cannot
    // affect another.
    let stale = store.scan(prefix).await.expect("scan for wipe");
    if stale.is_empty() {
        return;
    }
    let mut batch = WriteBatch::new();
    for (key, _) in stale {
        batch = batch.delete(key);
    }
    let outcome = store.commit(batch).await.expect("wipe commit");
    assert_eq!(
        outcome,
        CommitOutcome::Committed,
        "unconditional delete batch must commit"
    );
}

/// `<prefix><suffix>` as a key.
pub fn key(prefix: &[u8], suffix: &str) -> Vec<u8> {
    let mut key = prefix.to_vec();
    key.extend_from_slice(suffix.as_bytes());
    key
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
