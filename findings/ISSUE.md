## Summary

The `rollback_if_not_exist` escalation in `LockResolver::get_txn_status_from_lock` — the mechanism that recovers an **orphaned lock whose primary transaction was never written** — is unreachable. `check_txn_status` matches `Error::ExtractedErrors`, but its plan (`retry_multi_region → merge(CollectSingle) → extract_error`) can only deliver a per-key error as `Error::MultipleKeyErrors`. As a result, any key left under such an orphaned lock becomes **permanently unreadable and unwritable** through every API except `cleanup_locks`, even long after the lock's TTL expires.

This appears to be a regression introduced by #519 (`7d80f59`, "transaction: Resolve locks using kv_resolve_lock interface..."). The previous code sent a legacy `Cleanup` on the primary, which rolls back a missing primary unconditionally; #519 replaced it with the `CheckTxnStatus` flow whose escalation arm never fires for this error shape.

## Environment

- client-rust `master` @ `e53837d`
- TiKV / PD `v8.5.5`, single node, api-v1, no keyspace

## How the orphan arises (no API misuse required)

Prewrite requests are dispatched concurrently per region. A transaction whose keys span two regions can have its **secondary**'s prewrite land while its **primary**'s prewrite fails (lost write-write race) or never runs (client crash mid-prewrite). The result is a lock on the secondary key naming a primary that has no lock and no commit/rollback record — exactly the state `rollback_if_not_exist` exists to recover. Note this also arises from a plain crash between the parallel prewrites, with no client code running afterward.

## Reproduction

Using the public transactional API against a cluster where keys `P` and `S` fall in **different regions** (the orphan needs the two prewrites to be separate per-region batches; within one region the prewrite fails atomically and leaves nothing):

```rust
// P sorts first, S last; ensure a region boundary between them.
let mut txn = client.begin_optimistic().await?;
txn.lock_keys(vec![P.clone()]).await?;   // P becomes the primary
txn.put(S.clone(), b"orphaned".to_vec()).await?;

// Invalidate P from another transaction so `txn`'s prewrite on P loses:
let mut racer = client.begin_optimistic().await?;
racer.put(P.clone(), b"newer".to_vec()).await?;
racer.commit().await?;

let err = txn.commit().await;            // Err(WriteConflict) — S's prewrite already landed
drop(txn);                               // no rollback — i.e. a crash

// Now S is poisoned:
let read = another_txn.get(S).await;     // Err, forever — see below
```

**Observed** — every read of `S` (immediately, and long after the ~3s lock TTL expires):

```
MultipleKeyErrors([KeyError(KeyError {
    txn_not_found: Some(TxnNotFound { start_ts: <txn's ts>, primary_key: <P> }), .. })])
```

Only `TransactionClient::cleanup_locks` over the range recovers `S`.

## Root cause (file:line, `master` @ e53837d)

1. The read hits the lock and enters resolution: `resolve_locks` (`src/transaction/lock.rs:51`) → `get_txn_status_from_lock` (`:527`, `rollback_if_not_exist = false`) → `check_txn_status` (`:426`).
2. TiKV's CheckTxnStatus on the missing primary returns `KeyError { txn_not_found }`. `single_shard_handler` takes the key error and returns `Err(Error::MultipleKeyErrors(..))` (`src/request/plan.rs:220-222`). Because the error is already stripped from the response, the downstream `ExtractError` just propagates it (`plan.rs:842`) — `Error::ExtractedErrors` (built only at `plan.rs:844/846`) is impossible in this plan shape.
3. `check_txn_status`'s conversion arm matches only `Err(Error::ExtractedErrors(..))` (`lock.rs:469-479`) to turn `txn_not_found` into `Error::TxnNotFound`. `MultipleKeyErrors` falls through `Err(err) => return Err(err)` (`lock.rs:480`) unconverted.
4. `get_txn_status_from_lock`'s retry loop only special-cases `Err(Error::TxnNotFound)` (`lock.rs:565-573`) — the branch that, once the TTL has expired, sets `rollback_if_not_exist = true` and retries (writing the rollback record). It never runs; the raw error escapes (`lock.rs:589`).
5. No layer above re-enters with different state; every fresh read repeats identically. `cleanup_locks` heals only because it calls `check_txn_status` with `rollback_if_not_exist = true, current_ts = u64::MAX` (`lock.rs:325-327`).

For contrast, three sibling call sites place `.extract_error()` **before** `.merge(..)`, where `HasKeyErrors for Result<T,E>` (`src/store/errors.rs:163-174`) re-wraps into `ExtractedErrors` — so `send_heart_beat`, `check_all_secondaries`, and `commit_primary` convert correctly. `check_txn_status` alone has the adapter order swapped relative to its own match arm.

## Proposed fix

Accept both wrappers in `check_txn_status` (`src/transaction/lock.rs:469`):

```rust
Err(Error::ExtractedErrors(mut errors) | Error::MultipleKeyErrors(mut errors)) => {
    match errors.pop() {
        Some(Error::KeyError(key_err)) => {
            if let Some(txn_not_found) = key_err.txn_not_found {
                return Err(Error::TxnNotFound(txn_not_found));
            }
            return Err(Error::KeyError(key_err));
        }
        Some(err) => return Err(err),
        None => unreachable!(),
    }
}
```

Validated against this repro: the poisoned key self-heals after TTL expiry (no `cleanup_locks` needed); pre-TTL reads return a correctly-typed `Err(TxnNotFound)` after backoff. No regressions in the unit suite or in an integration suite covering CAS races, write-skew locking, pessimistic waits, and scans. (An equivalent fix reorders `check_txn_status`'s plan to the heartbeat shape, `.extract_error().merge(CollectSingle)`. Please don't unify the wrappers globally at `plan.rs:222` — `MultipleKeyErrors` feeds other merge/collect logic and public error surfaces.)

Happy to open a PR if the approach looks right.

## Impact

Any client crash between parallel per-region prewrites — or any failed commit not followed by an explicit `rollback()` — can leave keys permanently unreadable for all readers. A contributing factor: `Committer::commit` propagates a prewrite failure without cleaning up already-placed prewrite locks (unlike client-go, whose `twoPhaseCommitter.execute` runs a deferred best-effort `BatchRollback` on commit failure), so even orderly failed commits produce this state unless the caller rolls back — and `PessimisticRollback` does not clear prewrite locks, so pessimistic transactions can orphan even with that discipline.

## Workaround

Run `cleanup_locks(range, &safepoint, options)` periodically or on `TxnNotFound` read failures; always `rollback()` after a failed `commit()`.

## Related

- #519 (the regressing change / current resolution baseline), #528 (resolve-lock gaps for async-commit locks), #497 (`txn_not_found` seen in the wild), #315, #313.
