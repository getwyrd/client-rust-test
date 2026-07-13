//! The M4 evaluation gate, run against a real TiKV/PD cluster.
//!
//! Every test names the proposal-0015 obligation it verifies. Groups:
//!
//! - `a*` — trait-contract basics: object-safe + `Send` usage, byte-identical
//!   storage, `Conflict` as an `Ok` with zero side effects.
//! - `b*` — the M4 directory-operation shapes: create / rename / file-commit
//!   as multi-key atomic batches.
//! - `c*` — contention: exactly-one-winner CAS, `require_absent` collision,
//!   the write-skew rule (why preconditions must be locked).
//! - `d*` — API-shape confirmations the proposal explicitly defers to "confirm
//!   against the pinned version": the write-conflict error shape, locking-read
//!   freshness, paged scans, prefix edge cases.
//!
//! Needs `--features integration-tests` and a reachable cluster (`make gate`).

#![cfg(feature = "integration-tests")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use common::store;
use common::wipe;
use harness::deterministic_bytes;
use harness::key;
use tikv_client::CheckLevel;
use tikv_client::TransactionClient;
use tikv_client::TransactionOptions;
use tokio::sync::Barrier;
use wyrd_gate::is_lost_race;
use wyrd_gate::traits::CommitOutcome;
use wyrd_gate::traits::MetadataStore;
use wyrd_gate::traits::WriteBatch;
use wyrd_gate::LockMode;
use wyrd_gate::SCAN_PAGE;

fn val(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

// ---------------------------------------------------------------------------
// P. Preconditions — what the rest of the gate assumes about the cluster
// ---------------------------------------------------------------------------

/// The cluster must be able to split regions at all.
///
/// The proposal's headline obligation — a multi-key `commit(WriteBatch)` is
/// atomic — is only interesting when a batch genuinely spans Raft regions, which
/// is why `cluster/tikv.toml` sets `region-max-keys = 10`. But nothing verified
/// that the config was actually in force: if the mount were missing or the
/// thresholds ignored, every "cross-region" test would quietly run inside one
/// region and still pass, proving nothing. An assumption no test can fail is not
/// an assumption, it is a hole.
///
/// So write enough keys to force splits and assert against PD that they happened.
/// Cheap (one batch), and it fails the gate loudly rather than letting the rest
/// pass vacuously.
#[tokio::test]
async fn p0_cluster_can_split_regions() {
    let store = store(LockMode::Pessimistic).await;

    // A PER-RUN prefix, and deliberately not a fixed one.
    //
    // `wipe` deletes keys; it does not delete REGION BOUNDARIES. Under a fixed
    // prefix, a boundary carved by an earlier run survives into this one, and the
    // check below would be satisfied instantly by that stale split — passing even
    // if cluster/tikv.toml were missing and TiKV could no longer split anything.
    // The test would then assert nothing while looking green, which is the exact
    // failure it exists to prevent. A fresh range has no boundary to inherit, so
    // the split it observes must have been made by TiKV, now.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let prefix_owned = format!("gate/p0/{nanos}/");
    let prefix = prefix_owned.as_bytes();

    let before = harness::cluster::region_count().await;
    let up = harness::cluster::stores_up().await;
    assert!(!up.is_empty(), "PD reports no TiKV store Up");

    // Comfortably past region-max-keys = 10, in one batch.
    let mut batch = WriteBatch::new();
    for i in 0..200 {
        batch = batch.put(key(prefix, &format!("k/{i:04}")), val("v"));
    }
    assert_eq!(
        store.commit(batch).await.expect("seed"),
        CommitOutcome::Committed
    );

    // Deliberately waits for TiKV's OWN split checker rather than asking PD to
    // split at a key (which is what cluster::ensure_cross_region does when a test
    // needs two *specific* keys separated). The point here is to prove that
    // `cluster/tikv.toml` is in force — an explicit split would succeed even with
    // the config missing, and every other test's cross-region claim rests on
    // natural splitting actually happening.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let lo = key(prefix, "k/0000");
    let hi = key(prefix, "k/0199");
    loop {
        // Both keys located in ONE PD snapshot: two separate lookups could
        // straddle a split/merge and report a boundary that never existed.
        if harness::cluster::are_cross_region(&lo, &hi).await {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "PRECONDITION FAILED: 200 keys did not split across regions (cluster has {} regions, \
             was {before}). The gate's multi-region obligations are VOID without splits — check \
             that cluster/tikv.toml is mounted (region-max-keys = 10).",
            harness::cluster::region_count().await
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    println!(
        "cluster splits regions: {} -> {} regions, stores Up {up:?}",
        before,
        harness::cluster::region_count().await
    );
}

// ---------------------------------------------------------------------------
// A. Trait-contract basics
// ---------------------------------------------------------------------------

/// The store works through `Arc<dyn MetadataStore>` across a spawned task —
/// the object-safety + `Send`-futures obligation exercised at runtime, not
/// just at compile time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a1_roundtrip_through_object_safe_trait() {
    const PREFIX: &[u8] = b"gate/a1/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    let dyn_store: Arc<dyn MetadataStore> = Arc::new(store);
    let spawned = Arc::clone(&dyn_store);
    tokio::spawn(async move {
        let batch = WriteBatch::new()
            .put(key(PREFIX, "inode:1"), val("one"))
            .put(key(PREFIX, "dirent:root/a"), val("1"));
        spawned.commit(batch).await.expect("commit")
    })
    .await
    .expect("task join")
    .eq(&CommitOutcome::Committed)
    .then_some(())
    .expect("unconditional batch commits");

    let got = dyn_store.get(&key(PREFIX, "inode:1")).await.expect("get");
    assert_eq!(got, Some(val("one")));

    let listed = dyn_store.scan(PREFIX).await.expect("scan");
    assert_eq!(listed.len(), 2);

    let outcome = dyn_store
        .commit(WriteBatch::new().delete(key(PREFIX, "dirent:root/a")))
        .await
        .expect("delete commit");
    assert_eq!(outcome, CommitOutcome::Committed);
    let gone = dyn_store
        .get(&key(PREFIX, "dirent:root/a"))
        .await
        .expect("get after delete");
    assert_eq!(gone, None);
}

/// Values come back byte-for-byte — the soundness condition for value-equality
/// CAS. Any server- or client-side normalization would turn every version CAS
/// into a spurious conflict.
#[tokio::test]
async fn a2_values_round_trip_byte_identically() {
    const PREFIX: &[u8] = b"gate/a2/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    let all_bytes: Vec<u8> = (0u8..=255).collect();
    let corpus: Vec<(Vec<u8>, Bytes)> = vec![
        (key(PREFIX, "empty"), Bytes::new()),
        (key(PREFIX, "nul"), Bytes::from_static(&[0x00])),
        (key(PREFIX, "ff-run"), Bytes::from_static(&[0xFF; 32])),
        // An invalid UTF-8 sequence — values must stay opaque bytes.
        (
            key(PREFIX, "not-utf8"),
            Bytes::from_static(&[0xC3, 0x28, 0xA0, 0xA1, 0xF0, 0x28, 0x8C, 0x28]),
        ),
        (key(PREFIX, "all-bytes"), Bytes::from(all_bytes)),
        (
            key(PREFIX, "blob-128k"),
            // Seed spells "WYRD" in ASCII; the corpus is reproducible.
            Bytes::from(deterministic_bytes(0x5759_5244, 128 * 1024)),
        ),
    ];

    let mut batch = WriteBatch::new();
    for (k, v) in &corpus {
        batch = batch.put(k.clone(), v.clone());
    }
    assert_eq!(
        store.commit(batch).await.expect("commit corpus"),
        CommitOutcome::Committed
    );

    for (k, want) in &corpus {
        let got = store.get(k).await.expect("get").expect("present");
        assert_eq!(&got, want, "get must be byte-identical for {k:?}");
    }
    let scanned = store.scan(PREFIX).await.expect("scan");
    assert_eq!(scanned.len(), corpus.len());
    for (k, v) in scanned {
        let want = corpus.iter().find(|(ck, _)| *ck == k).expect("known key");
        assert_eq!(v, want.1, "scan must be byte-identical for {k:?}");
    }
}

/// A failed precondition is `Ok(Conflict)` — never `Err` — and leaves zero
/// side effects: the trait's "either every precondition holds and every
/// put/delete lands, or nothing changes".
#[tokio::test]
async fn a3_conflict_is_ok_and_leaves_no_side_effects() {
    for mode in [LockMode::Pessimistic, LockMode::OptimisticLocked] {
        let prefix = format!("gate/a3/{mode:?}/");
        let prefix = prefix.as_bytes();
        let store = store(mode).await;
        wipe(&store, prefix).await;

        let guarded = key(prefix, "inode:1");
        let never = key(prefix, "inode:2");
        assert_eq!(
            store
                .commit(WriteBatch::new().put(guarded.clone(), val("v1")))
                .await
                .expect("setup"),
            CommitOutcome::Committed
        );

        // require(exact value) mismatch: puts AND deletes must both not land.
        let outcome = store
            .commit(
                WriteBatch::new()
                    .require(guarded.clone(), val("WRONG"))
                    .put(never.clone(), val("side-effect"))
                    .delete(guarded.clone()),
            )
            .await
            .expect("conflict is Ok, not Err");
        assert_eq!(outcome, CommitOutcome::Conflict, "{mode:?}");
        assert_eq!(store.get(&guarded).await.expect("get"), Some(val("v1")));
        assert_eq!(store.get(&never).await.expect("get"), None);

        // require_absent on a present key.
        let outcome = store
            .commit(
                WriteBatch::new()
                    .require_absent(guarded.clone())
                    .put(never.clone(), val("side-effect")),
            )
            .await
            .expect("conflict is Ok, not Err");
        assert_eq!(outcome, CommitOutcome::Conflict, "{mode:?}");
        assert_eq!(store.get(&never).await.expect("get"), None);
    }
}

// ---------------------------------------------------------------------------
// B. The M4 directory-operation shapes
// ---------------------------------------------------------------------------

/// create = atomic `{ put inode + put dirent }` guarded by `require_absent` on
/// both keys: when the dirent collides, the inode put must not land either.
#[tokio::test]
async fn b1_create_is_all_or_nothing() {
    const PREFIX: &[u8] = b"gate/b1/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    let inode = key(PREFIX, "inode:7");
    let dirent = key(PREFIX, "dirent:root/name");
    // The name is already taken.
    assert_eq!(
        store
            .commit(WriteBatch::new().put(dirent.clone(), val("inode:1")))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );

    let outcome = store
        .commit(
            WriteBatch::new()
                .require_absent(inode.clone())
                .require_absent(dirent.clone())
                .put(inode.clone(), val("{\"version\":1}"))
                .put(dirent.clone(), val("inode:7")),
        )
        .await
        .expect("collision is Conflict, not Err");
    assert_eq!(outcome, CommitOutcome::Conflict);
    // All-or-nothing: the non-colliding half must not have landed.
    assert_eq!(store.get(&inode).await.expect("get"), None);
    assert_eq!(store.get(&dirent).await.expect("get"), Some(val("inode:1")));
}

/// rename = atomic `{ delete old dirent + put new dirent }`, `require` on the
/// source re-pinned at commit, `require_absent` on the target.
#[tokio::test]
async fn b2_rename_is_atomic_delete_plus_put() {
    const PREFIX: &[u8] = b"gate/b2/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    let src = key(PREFIX, "dirent:root/old");
    let dst = key(PREFIX, "dirent:root/new");
    assert_eq!(
        store
            .commit(WriteBatch::new().put(src.clone(), val("inode:9")))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );

    let rename = WriteBatch::new()
        .require(src.clone(), val("inode:9"))
        .require_absent(dst.clone())
        .delete(src.clone())
        .put(dst.clone(), val("inode:9"));
    assert_eq!(
        store.commit(rename.clone()).await.expect("rename"),
        CommitOutcome::Committed
    );
    assert_eq!(store.get(&src).await.expect("get"), None);
    assert_eq!(store.get(&dst).await.expect("get"), Some(val("inode:9")));

    // Replaying the same rename must conflict (source is gone), not error.
    assert_eq!(
        store.commit(rename).await.expect("replay"),
        CommitOutcome::Conflict
    );
}

/// The file commit: version-conditional `require(prior) + put(next)` over a
/// JSON record — the exact `commit_chunk_map` shape from `wyrd-core`.
#[tokio::test]
async fn b3_file_commit_is_a_version_cas() {
    const PREFIX: &[u8] = b"gate/b3/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    let inode = key(PREFIX, "inode:3");
    let v1 = Bytes::from(
        serde_json::to_vec(&serde_json::json!({"version": 1, "state": "PENDING"})).unwrap(),
    );
    let v2 = Bytes::from(
        serde_json::to_vec(&serde_json::json!({"version": 2, "state": "COMMITTED"})).unwrap(),
    );

    assert_eq!(
        store
            .commit(WriteBatch::new().put(inode.clone(), v1.clone()))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );
    assert_eq!(
        store
            .commit(
                WriteBatch::new()
                    .require(inode.clone(), v1.clone())
                    .put(inode.clone(), v2.clone())
            )
            .await
            .expect("cas"),
        CommitOutcome::Committed
    );
    // A stale writer still holding v1 must be rejected distinguishably.
    assert_eq!(
        store
            .commit(
                WriteBatch::new()
                    .require(inode.clone(), v1)
                    .put(inode.clone(), val("stale"))
            )
            .await
            .expect("stale cas"),
        CommitOutcome::Conflict
    );
    assert_eq!(store.get(&inode).await.expect("get"), Some(v2));
}

// ---------------------------------------------------------------------------
// C. Contention — the heart of the gate
// ---------------------------------------------------------------------------

/// Race `tasks` concurrent commits of `make_batch(i)` from a barrier; return
/// (committed tags, conflicts, faults).
async fn race(
    store: &wyrd_gate::TikvMetadataStore,
    tasks: usize,
    make_batch: impl Fn(usize) -> WriteBatch,
) -> (Vec<usize>, usize, Vec<String>) {
    let barrier = Arc::new(Barrier::new(tasks));
    let mut handles = Vec::with_capacity(tasks);
    for (i, batch) in (0..tasks).map(|i| (i, make_batch(i))) {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            (i, store.commit(batch).await)
        }));
    }
    let mut winners = Vec::new();
    let mut conflicts = 0usize;
    let mut faults = Vec::new();
    for handle in handles {
        let (i, outcome) = handle.await.expect("task join");
        match outcome {
            Ok(CommitOutcome::Committed) => winners.push(i),
            Ok(CommitOutcome::Conflict) => conflicts += 1,
            Err(e) => faults.push(format!("task {i}: {e:?}")),
        }
    }
    (winners, conflicts, faults)
}

/// The load-bearing M4 property: under genuine concurrency the version CAS
/// admits exactly one winner per round; every loser gets `Ok(Conflict)` and
/// **never** `Err`. Run in both safe lock modes.
async fn cas_exactly_one_winner(mode: LockMode) {
    const TASKS: usize = 8;
    const ROUNDS: usize = 4;
    let prefix = format!("gate/c1/{mode:?}/");
    let prefix = prefix.as_bytes();
    let store = store(mode).await;
    wipe(&store, prefix).await;

    let inode = key(prefix, "inode:1");
    let mut current = val("version:0");
    assert_eq!(
        store
            .commit(WriteBatch::new().put(inode.clone(), current.clone()))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );

    for round in 0..ROUNDS {
        let (winners, conflicts, faults) = race(&store, TASKS, |i| {
            WriteBatch::new()
                .require(inode.clone(), current.clone())
                .put(inode.clone(), val(&format!("version:{round}:winner:{i}")))
        })
        .await;

        assert!(
            faults.is_empty(),
            "{mode:?} round {round}: losers must be Conflict, never Err — got {faults:#?}"
        );
        assert_eq!(
            winners.len(),
            1,
            "{mode:?} round {round}: exactly one CAS winner, got {winners:?}"
        );
        assert_eq!(conflicts, TASKS - 1, "{mode:?} round {round}");

        let expected = val(&format!("version:{round}:winner:{}", winners[0]));
        let stored = store.get(&inode).await.expect("get").expect("present");
        assert_eq!(
            stored, expected,
            "{mode:?} round {round}: winner's write is the stored value"
        );
        current = expected;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c1_version_cas_exactly_one_winner_pessimistic() {
    cas_exactly_one_winner(LockMode::Pessimistic).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c1_version_cas_exactly_one_winner_optimistic_locked() {
    cas_exactly_one_winner(LockMode::OptimisticLocked).await;
}

/// The `require_absent` collision guard under contention: racing creates of
/// the same name admit exactly one creator.
async fn require_absent_exactly_one_creator(mode: LockMode) {
    const TASKS: usize = 8;
    let prefix = format!("gate/c2/{mode:?}/");
    let prefix = prefix.as_bytes();
    let store = store(mode).await;
    wipe(&store, prefix).await;

    let inode_for = |i: usize| key(prefix, &format!("inode:{i}"));
    let dirent = key(prefix, "dirent:root/name");
    let (winners, conflicts, faults) = race(&store, TASKS, |i| {
        WriteBatch::new()
            .require_absent(dirent.clone())
            .require_absent(inode_for(i))
            .put(dirent.clone(), val(&format!("inode:{i}")))
            .put(inode_for(i), val("{\"version\":1}"))
    })
    .await;

    assert!(faults.is_empty(), "{mode:?}: {faults:#?}");
    assert_eq!(winners.len(), 1, "{mode:?}: exactly one creator");
    assert_eq!(conflicts, TASKS - 1, "{mode:?}");
    // All-or-nothing per loser: only the winner's inode key exists.
    let inodes = store.scan(&key(prefix, "inode:")).await.expect("scan");
    assert_eq!(
        inodes.len(),
        1,
        "{mode:?}: losers' inode puts must not land"
    );
    assert_eq!(inodes[0].0, inode_for(winners[0]), "{mode:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c2_require_absent_exactly_one_creator_pessimistic() {
    require_absent_exactly_one_creator(LockMode::Pessimistic).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c2_require_absent_exactly_one_creator_optimistic_locked() {
    require_absent_exactly_one_creator(LockMode::OptimisticLocked).await;
}

/// Why the locking rule exists. A batch whose precondition key is **only
/// read** (require guard, write elsewhere — the rename source shape) is
/// exposed to write skew under plain optimistic reads: TiKV conflict-checks
/// only written keys. This test pins the anomaly with a deterministic
/// interleaving — read guard, let a mutator commit, then commit — and shows
/// `lock_keys` closes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c3_write_skew_anomaly_and_the_lock_that_prevents_it() {
    const PREFIX: &[u8] = b"gate/c3/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;
    let client = TransactionClient::new(harness::pd_addrs())
        .await
        .expect("raw client");

    let guard = key(PREFIX, "guard");
    let data_skew = key(PREFIX, "data-skew");
    let data_locked = key(PREFIX, "data-locked");
    let reset = |g: &str| {
        WriteBatch::new()
            .put(guard.clone(), val(g))
            .delete(data_skew.clone())
            .delete(data_locked.clone())
    };

    // --- 1. UNLOCKED optimistic reads exhibit the anomaly. -----------------
    assert_eq!(
        store.commit(reset("g0")).await.expect("reset"),
        CommitOutcome::Committed
    );
    let mut skewed = client
        .begin_with_options(TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin");
    // Plain read of the precondition key: sees g0, takes no lock.
    let seen = skewed.get(guard.clone()).await.expect("get");
    assert_eq!(seen.as_deref(), Some(val("g0").as_ref()));
    // A mutator commits g0 → g1 *between our read and our commit*.
    assert_eq!(
        store
            .commit(
                WriteBatch::new()
                    .require(guard.clone(), val("g0"))
                    .put(guard.clone(), val("g1"))
            )
            .await
            .expect("mutator"),
        CommitOutcome::Committed
    );
    skewed
        .put(data_skew.clone(), b"written-under-stale-guard".to_vec())
        .await
        .expect("put");
    // The commit SUCCEEDS even though the "precondition" it read is stale —
    // this is the write-skew hole. If client/TiKV behavior ever changes to
    // reject this, the gate should know: that would let M4 relax the rule.
    skewed.commit().await.expect(
        "UNLOCKED optimistic commit succeeds despite the stale read — the documented anomaly",
    );
    assert_eq!(
        store.get(&data_skew).await.expect("get").as_deref(),
        Some(&b"written-under-stale-guard"[..]),
        "anomaly confirmed: the write landed under a stale precondition"
    );

    // --- 2. `lock_keys` closes the hole in optimistic mode. ----------------
    assert_eq!(
        store.commit(reset("g0")).await.expect("reset"),
        CommitOutcome::Committed
    );
    let mut locked = client
        .begin_with_options(TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin");
    locked
        .lock_keys(vec![guard.clone()])
        .await
        .expect("lock_keys");
    let seen = locked.get(guard.clone()).await.expect("get");
    assert_eq!(seen.as_deref(), Some(val("g0").as_ref()));
    assert_eq!(
        store
            .commit(
                WriteBatch::new()
                    .require(guard.clone(), val("g0"))
                    .put(guard.clone(), val("g1"))
            )
            .await
            .expect("mutator"),
        CommitOutcome::Committed
    );
    locked
        .put(data_locked.clone(), b"must-not-land".to_vec())
        .await
        .expect("put");
    let err = locked
        .commit()
        .await
        .expect_err("lock_keys makes the stale read a commit-time conflict");
    assert!(
        is_lost_race(&err),
        "the loser surfaces as a write-conflict (classifiable as Conflict), got: {err:?}"
    );
    // Gate finding: the failed commit's parallel prewrite may have left an
    // orphaned lock on `data_locked` (its primary, the guard key, never got
    // one). Without this rollback the read below errors with `TxnNotFound`
    // instead of resolving the orphan — failed commits must be rolled back.
    locked
        .rollback()
        .await
        .expect("rollback after failed commit");
    assert_eq!(
        store.get(&data_locked).await.expect("get"),
        None,
        "the guarded write must not land"
    );

    // --- 3. Pessimistic `get_for_update` blocks the mutator outright. ------
    assert_eq!(
        store.commit(reset("g0")).await.expect("reset"),
        CommitOutcome::Committed
    );
    let mut holder = client
        .begin_with_options(TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin");
    let seen = holder
        .get_for_update(guard.clone())
        .await
        .expect("locking read");
    assert_eq!(seen.as_deref(), Some(val("g0").as_ref()));

    let mutator_store = store.clone();
    let mutator_guard = guard.clone();
    let mutator_started = Arc::new(tokio::sync::Notify::new());
    let started_signal = Arc::clone(&mutator_started);
    let mutator = tokio::spawn(async move {
        // Signal immediately before issuing the blocking commit, so the
        // window below measures real lock-wait, not task-scheduling lag.
        started_signal.notify_one();
        mutator_store
            .commit(
                WriteBatch::new()
                    .require(mutator_guard.clone(), val("g0"))
                    .put(mutator_guard, val("g1")),
            )
            .await
    });
    // Only start timing once the mutator has actually begun its commit.
    mutator_started.notified().await;
    // While the locking read is held, the mutator must not complete: the
    // pessimistic lock serializes it behind us. (If the client surfaced an
    // immediate conflict instead of waiting, the mutator would finish fast
    // with Conflict — also safe, but a behavior change worth catching.)
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !mutator.is_finished(),
        "mutator must wait on the pessimistic lock, not proceed or fail fast"
    );
    holder
        .put(data_locked.clone(), b"written-under-held-lock".to_vec())
        .await
        .expect("put");
    holder.commit().await.expect("holder commits first");
    // Lock released: the mutator now proceeds. The guard's *value* is still g0,
    // but the holder's `get_for_update` buffered an `Op::Lock` on it that its
    // commit landed at a commit_ts above the mutator's for_update_ts — so the
    // woken mutator gets a **genuine** WriteConflict (`reason: PessimisticRetry`,
    // "the client should retry if necessary" per kvrpcpb), not a spurious one.
    // This is by-design (client-go's default `LockKeys` behaves identically);
    // client-rust simply doesn't retry it internally, so the caller must
    // restart at a fresh for_update_ts. The safe envelope M4 relies on: the
    // outcome is Committed or Conflict — never a fault — and one
    // re-read-and-retry cycle (exactly what a `Conflict`-handling caller does)
    // then succeeds because the value is unchanged.
    let outcome = tokio::time::timeout(Duration::from_secs(20), mutator)
        .await
        .expect("mutator must unblock once the lock is released")
        .expect("join")
        .expect("woken waiter must fold into an outcome, never a fault");
    println!("observed wake-up behavior for the blocked mutator: {outcome:?}");
    if outcome == CommitOutcome::Conflict {
        // The documented recovery path: re-read, rebuild the batch, retry.
        let reread = store.get(&guard).await.expect("get").expect("present");
        assert_eq!(
            reread.as_ref(),
            val("g0").as_ref(),
            "the conflict was a stale for_update_ts, not a value change: guard is still g0"
        );
        let retry = store
            .commit(
                WriteBatch::new()
                    .require(guard.clone(), reread)
                    .put(guard.clone(), val("g1")),
            )
            .await
            .expect("retry");
        assert_eq!(
            retry,
            CommitOutcome::Committed,
            "one re-read-and-retry cycle must recover from the wake-up conflict"
        );
    }
    assert_eq!(
        store.get(&guard).await.expect("get").as_deref(),
        Some(val("g1").as_ref())
    );
}

// ---------------------------------------------------------------------------
// D. API-shape confirmations (proposal 0015 open-question items, verbatim)
// ---------------------------------------------------------------------------

/// "Reconfirm the write-conflict error path (`tikv_client::Error` wrapping
/// `KeyError::WriteConflict`) against the pinned version." Two raw optimistic
/// transactions race a write; the loser's commit error must carry a
/// `WriteConflict`, classifiable by `is_lost_race`.
#[tokio::test]
async fn d1_write_conflict_error_shape_is_key_error_with_conflict() {
    const PREFIX: &[u8] = b"gate/d1/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;
    let client = TransactionClient::new(harness::pd_addrs())
        .await
        .expect("raw client");
    let contended = key(PREFIX, "contended");

    let options = || TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn);
    let mut winner = client.begin_with_options(options()).await.expect("begin");
    let mut loser = client.begin_with_options(options()).await.expect("begin");
    winner
        .put(contended.clone(), b"winner".to_vec())
        .await
        .expect("put");
    loser
        .put(contended.clone(), b"loser".to_vec())
        .await
        .expect("put");
    winner.commit().await.expect("first commit wins");

    let err = loser
        .commit()
        .await
        .expect_err("second commit must conflict");
    // Record the exact shape for the M4 conflict-classification rule.
    println!("observed write-conflict error shape: {err:?}");
    assert!(
        is_lost_race(&err),
        "loser's error must be classifiable as a lost race, got: {err:?}"
    );
    // Failed commits are rolled back to clean up any prewrite locks.
    loser
        .rollback()
        .await
        .expect("rollback after failed commit");
}

/// The locking read reads the **latest committed** value (at for_update_ts),
/// not the transaction's start snapshot — the freshness the precondition
/// byte-compare depends on.
#[tokio::test]
async fn d2_get_for_update_reads_latest_committed_value() {
    const PREFIX: &[u8] = b"gate/d2/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;
    let client = TransactionClient::new(harness::pd_addrs())
        .await
        .expect("raw client");
    let inode = key(PREFIX, "inode:1");

    assert_eq!(
        store
            .commit(WriteBatch::new().put(inode.clone(), val("v1")))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );

    let mut txn = client
        .begin_with_options(TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin");
    // Bump the value AFTER the transaction's start_ts.
    assert_eq!(
        store
            .commit(
                WriteBatch::new()
                    .require(inode.clone(), val("v1"))
                    .put(inode.clone(), val("v2"))
            )
            .await
            .expect("bump"),
        CommitOutcome::Committed
    );

    let plain = txn.get(inode.clone()).await.expect("snapshot read");
    let locking = txn
        .get_for_update(inode.clone())
        .await
        .expect("locking read");
    txn.rollback().await.expect("rollback");

    assert_eq!(
        plain.as_deref(),
        Some(val("v1").as_ref()),
        "plain get reads the start_ts snapshot"
    );
    assert_eq!(
        locking.as_deref(),
        Some(val("v2").as_ref()),
        "get_for_update must read the latest committed value"
    );
}

/// `scan` pages internally (SCAN_PAGE at a time) and materializes the full
/// prefix — the shape of the interim large-directory guard. 1000 dirents is
/// ~4 pages and, under the cluster's aggressive split config, many regions.
#[tokio::test]
async fn d3_scan_pages_beyond_one_batch() {
    const PREFIX: &[u8] = b"gate/d3/";
    const DIRENTS: usize = 1000;
    assert!(DIRENTS > 3 * SCAN_PAGE as usize, "must exercise paging");

    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    // Write in a few multi-key batches (each one transaction).
    for chunk in (0..DIRENTS).collect::<Vec<_>>().chunks(250) {
        let mut batch = WriteBatch::new();
        for i in chunk {
            batch = batch.put(
                key(PREFIX, &format!("dirent:big/{i:04}")),
                val(&format!("inode:{i}")),
            );
        }
        assert_eq!(
            store.commit(batch).await.expect("bulk put"),
            CommitOutcome::Committed
        );
    }

    let listed = store.scan(&key(PREFIX, "dirent:big/")).await.expect("scan");
    assert_eq!(listed.len(), DIRENTS);
    // Order is unspecified by the trait; verify the *set* is exact.
    let mut seen: Vec<_> = listed
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), DIRENTS, "no duplicates across page boundaries");
    for (k, v) in &listed {
        let name = String::from_utf8_lossy(k);
        let i: usize = name.rsplit('/').next().unwrap().parse().unwrap();
        assert_eq!(v, &val(&format!("inode:{i}")), "value intact for {name}");
    }

    // A prefix with no keys scans to empty, not an error.
    let empty = store
        .scan(&key(PREFIX, "dirent:none/"))
        .await
        .expect("scan");
    assert!(empty.is_empty());
}

/// Prefix-scan boundary hygiene: adjacent sibling prefixes must not bleed
/// into each other, including the `0xFF` carry-over edge.
#[tokio::test]
async fn d4_adjacent_prefixes_do_not_bleed() {
    const PREFIX: &[u8] = b"gate/d4/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;

    // "a/" (0x2F) and its immediate sibling byte "a0" (0x30).
    let mut batch = WriteBatch::new()
        .put(key(PREFIX, "a/x"), val("1"))
        .put(key(PREFIX, "a/y"), val("2"))
        .put(key(PREFIX, "a0"), val("sibling"));
    // A prefix ending in 0xFF: upper bound must carry into the parent byte.
    let mut ff_prefix = key(PREFIX, "f");
    ff_prefix.push(0xFF);
    let mut in_ff_1 = ff_prefix.clone();
    in_ff_1.push(0x01);
    let mut in_ff_2 = ff_prefix.clone();
    in_ff_2.push(0xFF);
    batch = batch
        .put(in_ff_1.clone(), val("in-1"))
        .put(in_ff_2.clone(), val("in-2"))
        .put(key(PREFIX, "g"), val("outside"));
    assert_eq!(
        store.commit(batch).await.expect("setup"),
        CommitOutcome::Committed
    );

    let under_a = store.scan(&key(PREFIX, "a/")).await.expect("scan");
    let keys: Vec<_> = under_a.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(under_a.len(), 2, "sibling 'a0' must not appear: {keys:?}");
    assert!(keys.contains(&key(PREFIX, "a/x")) && keys.contains(&key(PREFIX, "a/y")));

    let under_ff = store.scan(&ff_prefix).await.expect("scan");
    let keys: Vec<_> = under_ff.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(under_ff.len(), 2, "'g' must not appear: {keys:?}");
    assert!(keys.contains(&in_ff_1) && keys.contains(&in_ff_2));
}

/// **Gate finding, pinned.** A pessimistic `get_for_update` that had to WAIT
/// on another transaction's lock is woken with a **genuine** `WriteConflict`
/// once that transaction commits: TiKV (default `WakeUpModeNormal`) returns it
/// whenever another txn committed on the key at `commit_ts > for_update_ts` —
/// including the `Op::Lock`-only commit that a `get_for_update` itself lands,
/// so the value may be unchanged yet the conflict is real ("the client should
/// retry if necessary" per kvrpcpb). This is **by-design and matches
/// client-go's default `LockKeys`**; client-rust simply doesn't retry it
/// internally, so `metadata-tikv` (like any caller) must restart at a fresh
/// `for_update_ts` under lock contention. client-rust also lacks client-go's
/// opt-in fair/aggressive locking (`WakeUpModeForceLock`), which would
/// lock-with-conflict instead of erroring — the one real parity gap (an
/// enhancement, not a bug). This test pins the raw error shape and the
/// invariant that makes it safe to fold into `Conflict`: always classifiable
/// as a lost race, never an opaque fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d5_woken_lock_waiter_surfaces_a_classifiable_write_conflict() {
    const PREFIX: &[u8] = b"gate/d5/";
    let store = store(LockMode::Pessimistic).await;
    wipe(&store, PREFIX).await;
    let client = TransactionClient::new(harness::pd_addrs())
        .await
        .expect("raw client");

    let guarded = key(PREFIX, "guarded");
    let unrelated = key(PREFIX, "unrelated");
    assert_eq!(
        store
            .commit(WriteBatch::new().put(guarded.clone(), val("g0")))
            .await
            .expect("setup"),
        CommitOutcome::Committed
    );

    let pessimistic = || TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn);
    let mut holder = client
        .begin_with_options(pessimistic())
        .await
        .expect("begin");
    holder
        .get_for_update(guarded.clone())
        .await
        .expect("holder takes the lock");

    let waiter_client = client.clone();
    let waiter_key = guarded.clone();
    let waiter_started = Arc::new(tokio::sync::Notify::new());
    let started_signal = Arc::clone(&waiter_started);
    let waiter = tokio::spawn(async move {
        let mut txn = waiter_client
            .begin_with_options(pessimistic())
            .await
            .expect("begin");
        // Signal right before the blocking locking read, so the window below
        // measures the lock-wait and not the begin/timestamp RPCs.
        started_signal.notify_one();
        let woken = txn.get_for_update(waiter_key).await;
        let _ = txn.rollback().await;
        woken
    });

    waiter_started.notified().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiter.is_finished(), "waiter must block on the held lock");
    // The holder commits a write on an UNRELATED key; the guarded key's value
    // is untouched, only its pessimistic lock is released.
    holder
        .put(unrelated.clone(), b"x".to_vec())
        .await
        .expect("put");
    holder.commit().await.expect("holder commits");

    let woken = tokio::time::timeout(Duration::from_secs(20), waiter)
        .await
        .expect("waiter must wake once the lock is released")
        .expect("join");
    match woken {
        // If a future client-rust version adds fair/aggressive locking
        // (WakeUpModeForceLock) or an internal retry, the waiter acquires
        // cleanly and reads the unchanged value — worth noticing, hence the
        // print.
        Ok(value) => {
            println!("waiter acquired transparently (client retried the lock): {value:?}");
            assert_eq!(value.as_deref(), Some(val("g0").as_ref()));
        }
        // Today's pinned behavior: the wake-up WriteConflict surfaces. The
        // load-bearing assertion is that it is *classifiable* — it must fold
        // into the trait's Conflict, never leak as a backend fault.
        Err(err) => {
            println!("observed wake-up error shape: {err:?}");
            assert!(
                is_lost_race(&err),
                "the wake-up error must classify as a lost race, got: {err:?}"
            );
        }
    }
}

/// **Gate finding — client-rust regression (#519).** This test asserts the
/// *correct* behavior and therefore **fails against unfixed client-rust**: it
/// is the finding, expressed as a red test that turns green when the client is
/// fixed. The harness deliberately carries **no lock-sweeping workaround** —
/// resolving an orphaned lock is client-rust's job, and the fix belongs there
/// (see `findings/txn-not-found-lock-resolution.md` and the patch).
///
/// The scenario: an optimistic transaction's prewrite lands a lock on a
/// *secondary* key while its *primary* prewrite fails (lost write-write race)
/// or never runs (crash between the parallel per-region prewrites), leaving an
/// orphaned lock whose primary has no record. A correct client resolves this
/// on the next read once the lock's TTL has expired
/// (`get_txn_status_from_lock` escalates to `rollback_if_not_exist`). In
/// unfixed client-rust that heal path is unreachable — `check_txn_status`
/// matches `Error::ExtractedErrors` but the plan delivers the key error as
/// `Error::MultipleKeyErrors` — so the key stays unreadable forever
/// (`MultipleKeyErrors([KeyError { txn_not_found }])`), and this test's
/// post-TTL read fails.
///
/// Repro subtlety: a prewrite request is per region and fails atomically
/// within a region, so the orphan needs the two keys in **different** regions.
/// The test forces a region boundary (filler keys past the cluster's
/// `region-max-keys`) and confirms the orphan with `scan_locks` before
/// asserting anything; it uses a per-run unique prefix so a lingering orphan
/// (on an unfixed client) never collides with a rerun, and it never sweeps
/// locks itself.
#[tokio::test]
async fn d6_orphaned_lock_must_be_resolved_by_client_rust() {
    let store = store(LockMode::Pessimistic).await;
    let client = TransactionClient::new(harness::pd_addrs())
        .await
        .expect("raw client");

    // Per-run unique prefix: an orphan the client fails to resolve lingers on
    // the (throwaway) cluster, so a fresh prefix keeps reruns independent
    // without the harness ever sweeping locks. (Test code may read the clock;
    // the store never does.)
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let prefix_owned = format!("gate/d6/{nanos}/");
    let prefix = prefix_owned.as_bytes();

    // primary sorts first, secondary last; the region is split at `split_at`,
    // which sits strictly between them, so the two keys land in different regions.
    let primary = key(prefix, "a-primary"); // lock_keys makes this the primary
    let secondary = key(prefix, "z-secondary");
    let split_at = key(prefix, "m-split");
    let mut setup = WriteBatch::new().put(primary.clone(), val("p0"));
    for i in 0..30 {
        setup = setup.put(key(prefix, &format!("m-fill/{i:02}")), val("x"));
    }
    assert_eq!(
        store.commit(setup).await.expect("setup"),
        CommitOutcome::Committed
    );

    // PRECONDITION, asserted against PD rather than hoped for.
    //
    // A prewrite request is per region and fails atomically *within* a region, so
    // the orphan (a lock on the secondary whose primary was never written) can
    // only exist if the two keys live in DIFFERENT regions. This used to be left
    // to chance: write some filler, then retry the orphan 8 times and, if no lock
    // ever appeared, panic "region split never separated the keys". On a cluster
    // with many small regions, pd.toml's merge scheduler can undo the split faster
    // than it lands, so that panic fires — and it is indistinguishable, to anything
    // reading the exit code, from the client failing to resolve the orphan. The
    // test then reads as proof of the bug while having proved nothing.
    //
    // Manufacture the orphan: an optimistic txn locks the primary and puts the
    // secondary, the primary is invalidated by a racing commit so the orphaner
    // loses at prewrite, and it is dropped WITHOUT rollback — a crash.
    let upper = wyrd_gate::prefix_upper_bound(prefix).expect("bounded prefix");
    let mut orphan = None;
    for round in 0..4u32 {
        // Re-establish the precondition on EVERY attempt, not once up front.
        //
        // The boundary is not stable: pd.toml's merge scheduler
        // (max-merge-region-size = 1) actively coalesces the tiny regions this
        // creates, so a boundary confirmed before the loop can be gone by the time
        // the orphaner prewrites. The prewrite would then be single-region, no
        // orphan would appear, and the test would panic as a harness failure —
        // reintroducing, one level up, exactly the "failed for a reason that isn't
        // the finding" problem this precondition exists to eliminate.
        //
        // Cheap when already satisfied: one PD read.
        harness::cluster::ensure_cross_region(&primary, &secondary, &split_at).await;

        let mut orphaner = client
            .begin_with_options(TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn))
            .await
            .expect("begin");
        orphaner
            .lock_keys(vec![primary.clone()])
            .await
            .expect("lock primary");
        orphaner
            .put(secondary.clone(), b"orphaned".to_vec())
            .await
            .expect("put secondary");
        assert_eq!(
            store
                .commit(
                    WriteBatch::new()
                        .require(primary.clone(), val(&format!("p{round}")))
                        .put(primary.clone(), val(&format!("p{}", round + 1)))
                )
                .await
                .expect("racer"),
            CommitOutcome::Committed
        );
        let err = orphaner
            .commit()
            .await
            .expect_err("the orphaner must lose its race");
        assert!(is_lost_race(&err), "sanity: {err:?}");
        drop(orphaner); // no rollback — the crash

        let ts = client.current_timestamp().await.expect("ts");
        let locks = client
            .scan_locks(&ts, prefix.to_vec()..upper.clone(), 128)
            .await
            .expect("scan_locks");
        if let Some(lock) = locks.iter().find(|l| l.key == secondary) {
            orphan = Some(lock.clone());
            break;
        }
        println!("round {round}: no orphan lock yet (prewrite batching); retrying");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let Some(orphan) = orphan else {
        // Not a cross-region problem any more — that is asserted above — so this
        // is a genuine harness failure, and gate-verdict.sh will correctly refuse
        // to count it as evidence that the #519 gap is still open.
        panic!(
            "could not manufacture the orphan even though {:?} and {:?} are in different regions",
            String::from_utf8_lossy(&primary),
            String::from_utf8_lossy(&secondary)
        );
    };
    println!(
        "orphan confirmed: lock on {:?}, primary {:?}, ttl {}ms",
        String::from_utf8_lossy(&orphan.key),
        String::from_utf8_lossy(&orphan.primary_lock),
        orphan.lock_ttl
    );

    // Within the lock's TTL a read may legitimately fail on any client (the
    // orphaner's transaction could still be live) — capture the shape as
    // evidence, but don't assert on it.
    match store.get(&secondary).await {
        Err(e) => println!("within-TTL read (may fail on any client): {e:?}"),
        Ok(v) => println!("within-TTL read already resolved: {v:?}"),
    }

    // The load-bearing assertion: once the lock's TTL has expired, a correct
    // client resolves the orphan on read and the key is readable (absent —
    // the orphaned prewrite was never committed). Unfixed client-rust cannot,
    // and fails here. The harness relies on the client to do this; it does not
    // sweep the lock itself.
    tokio::time::sleep(Duration::from_millis(orphan.lock_ttl + 2000)).await;
    match store.get(&secondary).await {
        Ok(None) => println!("orphaned lock resolved by client-rust after TTL — key readable"),
        Ok(Some(v)) => panic!("the orphaned prewrite must not be visible as data: {v:?}"),
        Err(e) => panic!(
            "client-rust did NOT resolve the orphaned lock after TTL expiry — regression #519 \
             (unfixed?). See findings/txn-not-found-lock-resolution.md. Error: {e:?}"
        ),
    }
}
