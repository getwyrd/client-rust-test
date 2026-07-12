//! Failpoint-driven proof of **finding 2** (tikv/client-rust#545): after a 2PC
//! commit fails at prewrite, `Transaction::rollback()` clears the already-placed
//! prewrite locks in **optimistic** mode (`BatchRollback`) but **not** in
//! **pessimistic** mode (`PessimisticRollback` only removes
//! `LockType::Pessimistic` locks, silently skipping the prewritten Put/Delete 2PC
//! locks — and still returns `Ok`).
//!
//! Uses client-rust's built-in `after-prewrite` failpoint to make a commit fail
//! *after* prewrite has durably placed its locks — deterministic, single-key,
//! no cross-region timing. `scan_locks` is the ground truth.
//!
//! Its own binary + serial: the `fail` registry is process-global, so this must
//! not share a process with `gate`'s parallel commits. Run via
//! `make failpoint-test` (which passes `--test-threads=1`).
//!
//! # This test asserts the CORRECT behavior, so it is RED until the fix lands
//!
//! Like `gate::d6`, and per the harness's governing principle — *where the client
//! is deficient, that is a finding expressed as a failing test, fixed in
//! client-rust, not papered over here* — `d7` asserts what `rollback()` **must**
//! do: clear the locks it placed, in both lock modes.
//!
//! It therefore **fails on the pinned baseline** (`e53837d`), where the bug is
//! live, and **passes** once tikv/client-rust#547 lands. It is the regression
//! test for that fix, not a preservation of the bug.
//!
//! (It previously asserted `pess_locks == 1` — i.e. that the bug was *present* —
//! which inverted the signal: the test went red the moment the client was fixed,
//! and a green suite meant a broken client. `scripts/gate-verdict.sh` owns the
//! expectation instead, and shouts if this ever passes unexpectedly.)

#![cfg(feature = "integration-tests")]

mod common;

use fail::FailScenario;
use tikv_client::CheckLevel;
use tikv_client::TransactionClient;
use tikv_client::TransactionOptions;

/// Number of locks currently held on exactly `key`, via the maintenance
/// `scan_locks` API (ground truth, independent of the read path).
async fn locks_on(client: &TransactionClient, key: &[u8]) -> usize {
    let ts = client.current_timestamp().await.expect("ts");
    let mut upper = key.to_vec();
    upper.push(0x00);
    let locks = client
        .scan_locks(&ts, key.to_vec()..upper, 16)
        .await
        .expect("scan_locks");
    locks.iter().filter(|l| l.key == key).count()
}

#[tokio::test]
async fn d7_pessimistic_rollback_leaves_prewrite_locks() {
    let _ = env_logger::try_init();
    let client = TransactionClient::new(common::pd_addrs())
        .await
        .expect("connect to TiKV — is the cluster up? (`make cluster-up`)");
    let scenario = FailScenario::setup();

    // Unique keys per run (a lock the buggy path leaves behind lingers on the
    // throwaway cluster; a fresh prefix keeps reruns independent).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let k_pess = format!("gate/d7/{nanos}/pessimistic").into_bytes();
    let k_opt = format!("gate/d7/{nanos}/optimistic").into_bytes();

    // --- Pessimistic: prewrite places a lock, the commit fails after it, and
    //     rollback() reports Ok while LEAVING the prewrite lock (the bug). ---
    fail::cfg("after-prewrite", "return").unwrap();
    let mut txn = client
        .begin_with_options(TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin pessimistic");
    txn.get_for_update(k_pess.clone())
        .await
        .expect("get_for_update acquires the pessimistic lock");
    txn.put(k_pess.clone(), b"v".to_vec()).await.expect("put");
    let commit = txn.commit().await;
    fail::cfg("after-prewrite", "off").unwrap();
    assert!(
        commit.is_err(),
        "the after-prewrite failpoint must fail the commit (after locks are placed)"
    );
    // The documented cleanup path. It returns Ok — but does it actually clear
    // the prewrite lock?
    txn.rollback()
        .await
        .expect("pessimistic rollback() reports Ok");
    let pess_locks = locks_on(&client, &k_pess).await;

    // --- Optimistic: identical sequence; rollback() uses BatchRollback, which
    //     DOES clear the prewrite lock (the clean baseline). ---
    fail::cfg("after-prewrite", "return").unwrap();
    let mut txn2 = client
        .begin_with_options(TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn))
        .await
        .expect("begin optimistic");
    txn2.put(k_opt.clone(), b"v".to_vec()).await.expect("put");
    let commit2 = txn2.commit().await;
    fail::cfg("after-prewrite", "off").unwrap();
    assert!(
        commit2.is_err(),
        "the after-prewrite failpoint must fail the commit"
    );
    txn2.rollback().await.expect("optimistic rollback()");
    let opt_locks = locks_on(&client, &k_opt).await;

    println!(
        "after a failed commit + rollback(): pessimistic key holds {pess_locks} lock(s), \
         optimistic key holds {opt_locks} lock(s)"
    );

    scenario.teardown();

    // The baseline: optimistic BatchRollback cleared the prewrite lock. This is
    // the proof that the assertion below is reachable and that the failpoint did
    // what it claims — an optimistic rollback on the very same sequence is clean.
    assert_eq!(
        opt_locks, 0,
        "optimistic rollback (BatchRollback) must clear the prewrite lock"
    );
    // The obligation. `rollback()` is the documented cleanup path for a failed
    // commit; a rollback that reports Ok while leaving the locks it placed is a
    // rollback that did not roll back. Optimistic mode already honors this
    // (above), so pessimistic mode must too.
    //
    // RED on the pinned baseline (e53837d): PessimisticRollback removes only
    // LockType::Pessimistic locks and silently skips the prewritten 2PC lock.
    // GREEN once tikv/client-rust#547 lands. See
    // findings/pessimistic-rollback-leaves-prewrite-locks.md.
    assert_eq!(
        pess_locks, 0,
        "FINDING 2 (tikv/client-rust#545): pessimistic rollback() returned Ok but left the \
         prewrite lock in place — PessimisticRollback skips prewritten 2PC locks. The same \
         sequence in optimistic mode leaves 0 locks, so this is not inherent to the flow."
    );
}
