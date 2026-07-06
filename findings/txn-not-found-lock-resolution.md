# Regression (#519): orphaned secondary lock is never resolved — reads fail with `TxnNotFound` forever

Draft upstream issue for tikv/client-rust. Status: verified locally, causal
chain adversarially reviewed and confirmed, fix validated twice independently
(see Fix validation). Ready to file.

## Summary

The `rollback_if_not_exist` escalation in `get_txn_status_from_lock` — the
mechanism that recovers **orphaned locks whose primary was never written** —
is dead code: `check_txn_status` matches `Error::ExtractedErrors`, but its
plan (`retry_multi_region → merge(CollectSingle) → extract_error`) can only
deliver per-key errors as `Error::MultipleKeyErrors`. Consequently any key
under such an orphan is **unreadable and unwritable forever**, by every API
except `cleanup_locks`, even long after the lock's TTL expires. Every
transactional access fails with:

```
MultipleKeyErrors([KeyError(KeyError { txn_not_found: Some(TxnNotFound {
    start_ts: <orphaner's ts>, primary_key: <the never-written primary> }), .. })])
```

This is a **regression introduced by `7d80f59`** ("transaction: Resolve locks
using kv_resolve_lock interface...", #519, 2026-01-20): the previous code sent
a legacy `Cleanup` on the primary ("let the status of the primary lock
converge"), which rolls back a missing primary unconditionally. #519 replaced
it with the client-go-style `CheckTxnStatus` flow whose escalation arm was
stillborn. No test covers the `txn_not_found` path (the lock.rs tests mock
only committed/region-error paths).

## How the orphan arises (no API misuse required)

Prewrite requests are dispatched **concurrently per region**. A transaction
whose keys span two regions can have its secondary's prewrite succeed while
its primary's fails (lost write-write race) or is never applied (client crash
mid-prewrite). Result: a lock on the secondary naming a primary that has no
lock and no commit/rollback record. Recovering exactly this state is what
`rollback_if_not_exist` exists for.

Notably this is not dismissible as caller error:

- Nothing documents that a failed `commit()` must be followed by
  `rollback()` (`Committer::commit` propagates the prewrite error without
  cleaning up already-placed locks — unlike client-go, whose
  `twoPhaseCommitter.execute` runs a deferred best-effort background
  `BatchRollback` of all the transaction's keys on commit failure unless the
  outcome is undetermined).
- The `Drop` check is silent for this exact sequence: after a failed commit
  the status is `StartedCommit`, and the check fires only for `Active` — so
  even `CheckLevel::Panic` (the default) never warns.
- A crash between parallel prewrites produces the state with zero client code
  running afterwards.

## Reproduction

Deterministic test:
`tests/gate.rs::d6_orphaned_lock_must_be_resolved_by_client_rust` in the
`client-rust-test` harness (TiKV/PD v8.5.5, single node, api-v1, no
keyspace; client-rust `master` @ e53837d). The test asserts the *correct*
behavior — the orphaned key must be readable once the lock's TTL expires — so
it **fails** on unfixed client-rust (that failure is the repro) and passes once
the fix lands. Outline:

1. Ensure a region boundary between keys P (primary) and S (secondary) —
   the harness forces one and confirms the orphan with `scan_locks` before
   asserting anything.
2. `begin_optimistic`; `lock_keys([P])` (P becomes primary); `put(S, ..)`.
3. Invalidate P: commit a conflicting write to P from another transaction.
4. `commit()` → fails with `WriteConflict`; the per-region prewrite for S has
   already succeeded → lock on S, primary P, no record ever at P.
5. Drop the transaction without `rollback()` — equivalently, crash here.
6. Read S.

Observed (verbatim, from a pristine `e53837d` run before the fix):

```
orphan confirmed: lock on "gate/d6/z-secondary", primary "gate/d6/a-primary", ttl 3012ms
observed poisoned-read error shape: MultipleKeyErrors([KeyError(KeyError {
    locked: None, ..., txn_not_found: Some(TxnNotFound {
        start_ts: 467476145087184897,
        primary_key: [.. b"gate/d6/a-primary" ..] }), ... })])
poisoned read still fails with the same wrapper after lock-TTL expiry (no self-heal)
```

The read fails immediately with `MultipleKeyErrors([KeyError { txn_not_found }])`;
still fails **identically after the lock's TTL (~3s) has expired** — and minutes
later, across processes — until `cleanup_locks` is run over the range. With the
one-line fix below, the same repro self-heals after TTL expiry. (The test
asserts the *specific* `MultipleKeyErrors`-wrapping-`txn_not_found` shape, not a
`txn_not_found` substring, so it fails rather than false-passes against a fixed
client whose converted error is `Error::TxnNotFound`.)

## Root cause (verified file:line)

1. A read hits the orphan; the `KeyIsLocked` LockInfo is harvested by the
   `ResolveLock` plan step (`error_locks!(GetResponse)`
   `src/transaction/requests.rs:821`; `ResolveLock::execute`
   `src/request/plan.rs:621-641`) → `resolve_locks`
   (`src/transaction/lock.rs:51`) → `get_txn_status_from_lock` (lock.rs:527),
   `rollback_if_not_exist = false` initially (lock.rs:547) →
   `check_txn_status` (lock.rs:426).
2. TiKV answers CheckTxnStatus on the never-written primary with
   `KeyError { txn_not_found }`. `single_shard_handler` **takes** the key
   error out of the response (`has_key_error!(CheckTxnStatusResponse)`,
   `src/store/errors.rs:66-83`) and returns
   `Err(Error::MultipleKeyErrors(..))` (`src/request/plan.rs:220-222`).
3. Because the error was already stripped from the response, the downstream
   `ExtractError` adapter's `?` just propagates it (`plan.rs:842`);
   `Error::ExtractedErrors` (constructed only at `plan.rs:844/846`) is
   **impossible** in this plan shape.
4. `check_txn_status`'s conversion arm matches only
   `Err(Error::ExtractedErrors(..))` (`lock.rs:469-479`) to turn
   `txn_not_found` into `Error::TxnNotFound`; `MultipleKeyErrors` falls
   through unconverted (`lock.rs:480`). The arm is dead code.
   (Sibling call sites that *do* work — `send_heart_beat`
   `transaction.rs:766-768`, `check_all_secondaries` `lock.rs:500-504`,
   `commit_primary` `transaction.rs:1392-1393` — place `.extract_error()`
   **before** `.merge(..)`, where `HasKeyErrors for Result<T,E>`
   (`store/errors.rs:163-174`) re-wraps into `ExtractedErrors`.
   `check_txn_status` alone has the adapter order swapped relative to its own
   match arm.)
5. `get_txn_status_from_lock`'s heal branch triggers only on
   `Err(Error::TxnNotFound)` (`lock.rs:565-573`: TTL-expiry check via
   `lock_until_expired_ms`, then `rollback_if_not_exist = true` and retry,
   which writes the rollback record). Unreachable; the raw error leaks to the
   caller (`lock.rs:589`).
6. No retry layer above re-enters with different state: every fresh read
   repeats the identical sequence with `rollback_if_not_exist = false`.
   `lock_backoff` loops only after successful resolution (`plan.rs:642-653`);
   region/gRPC backoffs never engage for this error class. TiKV never
   self-expires locks — expiry only makes them *resolvable*, and nothing ever
   resolves.

All transactional APIs funnel through this resolver (`get`, `key_exists`,
`batch_get`, all `scan*`, `get_for_update`, snapshots, and the write paths'
`.resolve_lock(..)` steps). The new public `TransactionClient::resolve_locks`
(#524) also fails on this orphan. `cleanup_locks` alone recovers, because it
passes `current_ts = u64::MAX, rollback_if_not_exist = true` (lock.rs:325-327).

## Fix validation (performed twice, independently)

Minimal fix — widen one match arm in `check_txn_status`
(`findings/fix-check-txn-status-wrapper.patch`):

```rust
Err(Error::ExtractedErrors(mut errors) | Error::MultipleKeyErrors(mut errors)) => { ... }
```

With only this change, on otherwise identical trees, both validation runs
observed:

- Pre-TTL read: fails with a properly-typed `Err(TxnNotFound)` after ~1.5s
  backoff (client-go parity — the transaction could still be alive).
- Post-TTL read: **succeeds** (`Ok(None)`): the heal branch fires, the
  rollback record is written, the orphan resolves through the normal read
  path. No `cleanup_locks` needed.
- No regressions: 63/63 client-rust unit tests pass; 16/16 other gate
  integration tests pass (CAS races, write-skew locking, pessimistic waits,
  scans).

Blast radius: error-variant changes confined to `check_txn_status`'s two
callers — reads on unexpired missing-primary locks now surface `TxnNotFound`
instead of `MultipleKeyErrors`; `cleanup_locks` would surface stray key errors
as `KeyError` (same control flow either way).

Alternative equivalent fix: reorder `check_txn_status`'s plan to the
heartbeat shape (`.extract_error().merge(CollectSingle)`), matching the
sibling call sites. Do **not** unify the wrappers globally at `plan.rs:222` —
`MultipleKeyErrors` feeds `CollectWithShard`/`CollectError` merge logic and
public error surfaces.

## Workaround (pre-fix)

Run `TransactionClient::cleanup_locks(range, &safepoint, options)`
periodically or on `TxnNotFound` read failures, and always `rollback()` after
a failed `commit()` to avoid creating orphans in the first place (crash
windows remain).

## Related upstream issues (checked 2026-07-05 — no duplicate found)

- tikv/client-rust#528 — resolve-lock gaps for async-commit locks (different
  path; same subsystem). PR #519 is the current lock-resolution baseline and
  the regressing change.
- tikv/client-rust#497 — partially-applied mutations after TiKV crashes;
  comments show a `txn_not_found` error in the wild.
- tikv/client-rust#315 — `TxnLockNotFound` on optimistic commit (old).
- tikv/client-rust#313 (closed) — rollback of failed pessimistic lock
  requests; closest prior art for the missing-cleanup contributing factor.
- tikv/client-rust#486 — pessimistic `WriteConflict` surfaced to callers
  (related behavior, not this bug).
