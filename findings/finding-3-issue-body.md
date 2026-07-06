<!-- Finding 3 — standalone enhancement issue for tikv/client-rust. BODY ONLY
     (title passed separately on the gh command line). Adversarially reviewed:
     the current WriteConflict behavior is by-design and matches client-go's
     default LockKeys — NOT a bug. The gap is a missing opt-in feature.
     Suggested title:
       Support fair/aggressive pessimistic locking (WakeUpModeForceLock) to lock-with-conflict instead of surfacing WriteConflict -->

## Today (correct, by design)

A pessimistic `get_for_update` / `lock_keys` that waits on another transaction's
lock and is then woken by TiKV gets a genuine `WriteConflict`
(`reason: PessimisticRetry`) whenever the key was committed by another
transaction at a `commit_ts` greater than this request's `for_update_ts`.
client-rust surfaces it as
`PessimisticLockError { inner: MultipleKeyErrors([KeyError { .. WriteConflict }]), .. }`
with no retry (`Transaction::pessimistic_lock` in `src/transaction/transaction.rs`;
`new_pessimistic_lock_request` in `src/transaction/requests.rs` always uses the
default `WakeUpModeNormal`).

This is **correct and matches client-go's default `LockKeys`**, which likewise
returns `ErrWriteConflict` to the caller — so it is **not a bug**. The caller is
expected to restart at a fresh `for_update_ts`. (For completeness: the
"transparent retry with a new `for_update_ts`" often attributed to the Go client
actually lives one layer up in TiDB — `pkg/executor/adapter.go:
handlePessimisticLockError` — not in client-go itself.)

## Gap (a missing feature)

client-rust has no equivalent of client-go's opt-in **fair / aggressive
locking**: `StartAggressiveLocking()`, which for a single-key `LockKeys` sends
`WakeUpModeForceLock` (`tikv/client-go txnkv/transaction/txn.go`). In that mode
TiKV **acquires the lock despite the conflict** and returns the latest value plus
`LockedWithConflictTS` instead of an error, so the caller can proceed at the
higher timestamp without abandoning the lock attempt.

client-rust already carries the generated proto for this
(`PessimisticLockWakeUpMode_WakeUpModeForceLock`, and the response
`results` / `locked_with_conflict_ts` fields, in `src/generated/kvrpcpb.rs`) but
never sets `wake_up_mode` and never reads `results`.

## Proposal

1. Add opt-in fair/aggressive locking: allow sending `WakeUpModeForceLock` and
   decode the `results` / `locked_with_conflict_ts` fields, so a single-key
   pessimistic lock can lock-with-conflict rather than surface an error.
2. *(Optional)* Provide a small retry-on-`WriteConflict` helper for
   `WakeUpModeNormal` users, since the equivalent driver logic lives in TiDB and
   is absent from the pure-Rust stack.

This is a feature-parity enhancement; the current default behavior would be
unchanged.

## Related

- #486 — "what causes this `WriteConflict`" (root cause discussed there; this
  issue tracks the missing fair-locking feature specifically, not the FAQ).
- #487, #482 — same `WriteConflict`-under-pessimistic question.
- #328 — `lock_keys` blocking / `Failed to resolve lock` ergonomics (tangential).
