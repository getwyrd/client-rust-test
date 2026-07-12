//! Fixtures specific to the wyrd `MetadataStore` contract.
//!
//! Only what speaks in terms of *this* contract lives here. Everything that is
//! merely true of a TiKV cluster — `$PD_ADDRS`, PD's region layout, deterministic
//! byte corpora — now lives in the `harness` crate, so a future differential runner
//! can use it without first having to agree to a `MetadataStore`.

#![allow(dead_code)]

use harness::pd_addrs;
use wyrd_gate::traits::CommitOutcome;
use wyrd_gate::traits::MetadataStore;
use wyrd_gate::traits::WriteBatch;
use wyrd_gate::LockMode;
use wyrd_gate::TikvMetadataStore;

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
