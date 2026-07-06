//! Self-contained reproduction for tikv/client-rust#545, using ONLY the public
//! `tikv-client` API (plus `tokio` and `fail`). No private harness.
//!
//! Cargo.toml:
//!   [dependencies]      tikv-client = "0.4"  # or a path to your checkout
//!   [dev-dependencies]  tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
//!                       fail  = { version = "0.4", features = ["failpoints"] }
//!
//! Run against a TiKV cluster:
//!   PD_ADDRS=127.0.0.1:2379 cargo test -- --test-threads=1 --nocapture
//!
//! Observed (client-rust master @ e53837d, TiKV v8.5.5):
//!   pessimistic key: 1 lock(s); optimistic key: 0 lock(s)

use std::env;

use fail::FailScenario;
use tikv_client::CheckLevel;
use tikv_client::Key;
use tikv_client::TransactionClient;
use tikv_client::TransactionOptions;

fn pd_addrs() -> Vec<String> {
    env::var("PD_ADDRS")
        .unwrap_or_else(|_| "127.0.0.1:2379".to_owned())
        .split(',')
        .map(From::from)
        .collect()
}

fn unique(suffix: &str) -> Vec<u8> {
    // A per-run unique key so reruns don't collide (no wall-clock dependency in
    // the assertion; just uniqueness).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("repro/{nanos}/{suffix}").into_bytes()
}

/// Locks currently held on exactly `key`, via the `scan_locks` maintenance API.
async fn locks_on(client: &TransactionClient, key: &[u8]) -> usize {
    let ts = client.current_timestamp().await.unwrap();
    let mut upper = key.to_vec();
    upper.push(0x00);
    let range = Key::from(key.to_vec())..Key::from(upper);
    let locks = client.scan_locks(&ts, range, 16).await.unwrap();
    locks.iter().filter(|l| l.key == key).count()
}

/// Finding #545: after a 2PC commit fails at prewrite, `Transaction::rollback()`
/// clears the prewrite lock in OPTIMISTIC mode but silently LEAVES it (returning
/// Ok) in PESSIMISTIC mode.
#[tokio::test]
async fn pessimistic_rollback_leaves_prewrite_lock() {
    let client = TransactionClient::new(pd_addrs()).await.unwrap();
    let scenario = FailScenario::setup();

    let k_pess = unique("pessimistic");
    let k_opt = unique("optimistic");

    // Pessimistic: prewrite places the lock, the failpoint fails the commit
    // after it, and rollback() reports Ok while leaving the lock.
    fail::cfg("after-prewrite", "return").unwrap();
    let mut txn = client
        .begin_with_options(TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn))
        .await
        .unwrap();
    txn.get_for_update(k_pess.clone()).await.unwrap();
    txn.put(k_pess.clone(), b"v".to_vec()).await.unwrap();
    assert!(txn.commit().await.is_err(), "failpoint must fail the commit");
    fail::cfg("after-prewrite", "off").unwrap();
    txn.rollback().await.expect("pessimistic rollback() returns Ok");
    let pess_locks = locks_on(&client, &k_pess).await;

    // Optimistic: identical sequence; rollback() (BatchRollback) clears the lock.
    fail::cfg("after-prewrite", "return").unwrap();
    let mut txn2 = client
        .begin_with_options(TransactionOptions::new_optimistic().drop_check(CheckLevel::Warn))
        .await
        .unwrap();
    txn2.put(k_opt.clone(), b"v".to_vec()).await.unwrap();
    assert!(txn2.commit().await.is_err(), "failpoint must fail the commit");
    fail::cfg("after-prewrite", "off").unwrap();
    txn2.rollback().await.expect("optimistic rollback()");
    let opt_locks = locks_on(&client, &k_opt).await;

    scenario.teardown();

    println!("pessimistic key: {pess_locks} lock(s); optimistic key: {opt_locks} lock(s)");
    assert_eq!(opt_locks, 0, "optimistic rollback clears the prewrite lock");
    assert_eq!(
        pess_locks, 1,
        "BUG: pessimistic rollback() returned Ok but left the prewrite lock"
    );
}
