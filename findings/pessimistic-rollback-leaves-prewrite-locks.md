## Summary

When a **pessimistic** transaction's 2PC commit fails after `prewrite` has
already placed locks on some regions, those prewrite locks are not cleaned up —
not by `commit()` (which propagates the error and does nothing), and **not even
by a subsequent `Transaction::rollback()`**, which issues `PessimisticRollback`,
silently returns `Ok`, and leaves the 2PC locks in place. The keys stay locked
until TTL expiry + another transaction's lock resolution. client-go proactively
cleans up in this situation. (Optimistic mode is **not** affected — its
`rollback()` uses `BatchRollback`, which does clear prewrite locks.)

## Environment

- client-rust `master` @ `e53837d`
- TiKV / PD `v8.5.5` (server behavior verified against `tikv/tikv` source)

## Mechanism (file:line)

- **Prewrite fans out one task per region.** `PrewriteRequest: Shardable`
  shards mutations by region (`src/transaction/requests.rs:253-274`);
  `single_plan_handler` `tokio::spawn`s each shard then `try_join_all`
  (`src/request/plan.rs:125`, `:136`). On the first region's key error, the
  sibling tasks are **detached, not cancelled** — their prewrite RPCs run to
  completion and durably place locks.
- **`commit()` does no cleanup.** `Committer::commit` propagates the prewrite
  error via `?` (`src/transaction/transaction.rs:1275`); there is no
  `Committer` `Drop`, no background cleanup. The public `Transaction::commit`
  returns the error unchanged (`:680-683`), leaving status `StartedCommit`.
- **The terminal pessimistic `rollback()` uses the wrong RPC.**
  `Committer::rollback`'s pessimistic arm sends `new_pessimistic_rollback_request`
  (`:1541-1542`). Per TiKV, `PessimisticRollback` acts only on
  `LockType::Pessimistic` locks (`tikv/tikv
  src/storage/txn/commands/pessimistic_rollback.rs:81-122`; `is_pessimistic_lock`
  in `components/txn_types/src/lock.rs:651-653`), and its own unit test asserts
  it does nothing to a non-pessimistic (prewritten) lock. After prewrite the
  lock is `Put`/`Delete`, so it is **skipped** — and because TiKV returns no
  key error, `rollback()` returns `Ok` while the locks remain.
- **Aggravator:** the auto-heartbeat started in `commit()` (`:665`, loop
  `:947-1002`) breaks only on `Rolledback | Committed | Dropped` (`:974-981`),
  not `StartedCommit` — so after a failed commit it **keeps extending** the
  orphaned locks' TTL until the txn is finally rolled back or dropped.

## Contrast with client-go

`twoPhaseCommitter.execute` defers a best-effort `cleanup()` goroutine on commit
failure, skipped only when the txn already committed or the result is
undetermined (`txnkv/transaction/2pc.go:1717-1764`). For a normal pessimistic
2PC txn, `cleanup` issues **`BatchRollback`** over all mutations
(`cleanup.go:64-77`, `2pc.go:1689-1690`) — and `BatchRollback` clears a lock by
`start_ts` regardless of lock type (`tikv/tikv actions/cleanup.rs:54-75`), so it
removes the prewrite locks. client-rust has no equivalent, and its terminal
pessimistic rollback uses `PessimisticRollback`, which cannot.

## Impact

After a failed pessimistic commit, secondary keys on the regions that prewrote
successfully stay locked until TTL expiry + resolution, blocking other
transactions on those keys for that window (extended by the heartbeat). Not data
loss — a liveness/latency and API-completeness bug. (Before #544, when the
primary was never written, the orphan was never resolvable at all — see #543.)

## Suggested fix (two parts)

1. **Bug:** make the terminal pessimistic `Committer::rollback` clear prewrite
   locks — use `BatchRollback` for the post-prewrite cleanup path, as client-go
   does. (`PessimisticRollback` remains correct for aborting an in-flight
   `pessimistic_lock` *acquisition*, `transaction.rs:896-929` — the defect is
   specifically its use in the terminal rollback after prewrite.)
2. **Enhancement:** have `Committer::commit` proactively roll back placed locks
   on prewrite/commit failure (best-effort, like client-go's `cleanup()`),
   skipped when `self.undetermined` (`:1295-1297`).

## Not a duplicate

Complementary to #543/#544 (read-side resolution of an orphan). Related: #528,
#235 ("why don't we resolve locks for pessimistic txns?"), #259, #216, #313.

## Reproduction

Self-contained test — depends only on `tikv-client`, `tokio`, and `fail`. It
uses the `after-prewrite` failpoint to fail a commit *after* prewrite has placed
its locks, then compares what `rollback()` leaves behind in the two modes
(`scan_locks` is the ground truth). Run with
`PD_ADDRS=127.0.0.1:2379 cargo test -- --test-threads=1 --nocapture`.

```rust
use std::env;
use fail::FailScenario;
use tikv_client::{CheckLevel, Key, TransactionClient, TransactionOptions};

fn pd_addrs() -> Vec<String> {
    env::var("PD_ADDRS").unwrap_or_else(|_| "127.0.0.1:2379".to_owned())
        .split(',').map(From::from).collect()
}

fn unique(suffix: &str) -> Vec<u8> {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("repro/{n}/{suffix}").into_bytes()
}

/// Locks currently held on exactly `key`, via the `scan_locks` maintenance API.
async fn locks_on(client: &TransactionClient, key: &[u8]) -> usize {
    let ts = client.current_timestamp().await.unwrap();
    let mut upper = key.to_vec();
    upper.push(0x00);
    let range = Key::from(key.to_vec())..Key::from(upper);
    client.scan_locks(&ts, range, 16).await.unwrap()
        .iter().filter(|l| l.key == key).count()
}

#[tokio::test]
async fn pessimistic_rollback_leaves_prewrite_lock() {
    let client = TransactionClient::new(pd_addrs()).await.unwrap();
    let scenario = FailScenario::setup();
    let (k_pess, k_opt) = (unique("pessimistic"), unique("optimistic"));

    // Pessimistic: prewrite places the lock, the failpoint fails the commit
    // after it, and rollback() reports Ok while leaving the lock.
    fail::cfg("after-prewrite", "return").unwrap();
    let mut txn = client
        .begin_with_options(TransactionOptions::new_pessimistic().drop_check(CheckLevel::Warn))
        .await.unwrap();
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
        .await.unwrap();
    txn2.put(k_opt.clone(), b"v".to_vec()).await.unwrap();
    assert!(txn2.commit().await.is_err(), "failpoint must fail the commit");
    fail::cfg("after-prewrite", "off").unwrap();
    txn2.rollback().await.expect("optimistic rollback()");
    let opt_locks = locks_on(&client, &k_opt).await;
    scenario.teardown();

    println!("pessimistic key: {pess_locks} lock(s); optimistic key: {opt_locks} lock(s)");
    assert_eq!(opt_locks, 0, "optimistic rollback clears the prewrite lock");
    assert_eq!(pess_locks, 1,
        "pessimistic rollback() returned Ok but left the prewrite lock");
}
```

Observed (client-rust `master` @ `e53837d`, TiKV v8.5.5):

```
pessimistic key: 1 lock(s); optimistic key: 0 lock(s)
```

- **Pessimistic:** `rollback()` returned `Ok`, yet a prewrite lock survives
  (`PessimisticRollback` skipped the non-pessimistic 2PC lock).
- **Optimistic (baseline):** `rollback()` (`BatchRollback`) cleared it.

The lock-resolution paths are untouched by #543, so this reproduces on `e53837d`
and on that fix branch alike.
