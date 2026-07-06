# Finding 3 — woken pessimistic `WriteConflict` (postable writeup)

Adversarially reviewed. Verdict on the original "spurious conflict / client-go
retries" claim: **OVERSTATED — corrected below.** The behavior is **by-design
and matches client-go's default `LockKeys`**; it is **not a bug**. The one real
parity gap is a **missing feature**: client-rust has no fair/aggressive locking
(`WakeUpModeForceLock`). Two framings — pick one to post.

Key corrections baked in (do not revert):
- The conflict is **genuine**, not spurious: TiKV returns it because a commit
  landed on the key at `commit_ts > for_update_ts` (incl. a `get_for_update`'s
  own `Op::Lock` commit). The value may be unchanged; the conflict is real.
- **client-go does NOT retry** on `WriteConflict` — it surfaces `ErrWriteConflict`
  just like client-rust. The fresh-`for_update_ts` retry lives in **TiDB**
  (`adapter.go: handlePessimisticLockError`), not the client library.

---

## Framing 1 — follow-up comment on #486 (recommended: existing thread, you already commented)

> Following up with the root-cause mechanics and how client-rust compares to the
> Go stack, since it clarifies that this is by-design rather than a bug.
>
> **Why the waiter still gets a `WriteConflict`.** When `get_for_update` /
> `lock_keys` must wait on another txn's lock, TiKV (default `WakeUpModeNormal`)
> wakes it and returns `WriteConflict` with `reason: PessimisticRetry` whenever
> the key was committed by another txn at `commit_ts` greater than your
> `for_update_ts`. It's a *genuine* conflict on that key — not spurious — and
> client-rust surfaces it as `PessimisticLockError { inner:
> MultipleKeyErrors([KeyError{ .. WriteConflict, reason: PessimisticRetry }]),
> .. }` (`Transaction::pessimistic_lock`; `CollectWithShard::merge` in
> `requests.rs`). There's no internal retry; the caller restarts at a fresh
> `for_update_ts` (pingyu's `a = a + 1` example).
>
> **This is by-design and matches client-go.** Worth stressing to head off a
> common misconception: client-go's `KVTxn.LockKeys` behaves the *same* way — a
> woken waiter's `WriteConflict` becomes `tikverr.ErrWriteConflict` and is
> returned to the caller (`txnkv/transaction/pessimistic.go:
> handleKeyErrorForResolve` → `txnlock.ExtractLockFromKeyErr` →
> `error.ExtractKeyErr`). The "transparent retry with a new `for_update_ts`"
> people attribute to the Go client actually lives one layer up in **TiDB**
> (`pkg/executor/adapter.go: handlePessimisticLockError`, which calls
> `GetStmtForUpdateTS()` and rebuilds the statement). So client-rust isn't
> diverging from client-go's library behavior; there's just no TiDB-equivalent
> driver on top.
>
> **The one real parity gap (a feature, not a bug).** client-go additionally
> offers opt-in *fair/aggressive locking* — `StartAggressiveLocking()`, which
> for single-key `LockKeys` sends `WakeUpModeForceLock`
> (`txnkv/transaction/txn.go`). In that mode TiKV *acquires the lock despite the
> conflict* and returns the new value + `LockedWithConflictTS` instead of an
> error, so the caller proceeds without abandoning the lock attempt. client-rust
> doesn't implement this: `new_pessimistic_lock_request` never sets
> `wake_up_mode` (so it's always `WakeUpModeNormal`) and nothing reads the
> response `results` / `locked_with_conflict_ts` fields. If avoiding the restart
> in this case is desirable, that's a clean enhancement request — happy to file
> one.

---

## Framing 2 — standalone enhancement issue

**Title:** Support fair/aggressive pessimistic locking (`WakeUpModeForceLock`) to lock-with-conflict instead of surfacing `WriteConflict`

**Body:**

> **Today.** A pessimistic `get_for_update` / `lock_keys` that waits on another
> txn's lock and is woken by TiKV gets a genuine `WriteConflict`
> (`reason: PessimisticRetry`) whenever the key was committed by another txn at
> `commit_ts > for_update_ts`. client-rust surfaces it as
> `PessimisticLockError { inner: MultipleKeyErrors([KeyError{ .. WriteConflict
> }]), .. }` with no retry (`Transaction::pessimistic_lock` in
> `src/transaction/transaction.rs`; `new_pessimistic_lock_request` in
> `src/transaction/requests.rs` always uses the default `WakeUpModeNormal`).
> This is correct and matches client-go's default `LockKeys`, so **it is not a
> bug** — the caller is expected to restart at a fresh `for_update_ts`.
>
> **Gap.** client-rust has no equivalent of client-go's opt-in
> *aggressive/fair locking* (`StartAggressiveLocking()` → `WakeUpModeForceLock`
> for single-key locks; `txnkv/transaction/txn.go`). In that mode TiKV acquires
> the lock despite the conflict and returns the latest value +
> `LockedWithConflictTS` (kvproto `PessimisticLockWakeUpMode_WakeUpModeForceLock`;
> response `results` / `locked_with_conflict_ts`), so the caller can continue at
> the higher ts without abandoning the lock. client-rust already carries the
> generated proto (`src/generated/kvrpcpb.rs`) but never sets `wake_up_mode` and
> never reads `results`.
>
> **Proposal.** (1) Add opt-in fair-locking: allow sending `WakeUpModeForceLock`
> and decode `results` / `locked_with_conflict_ts` so a single-key pessimistic
> lock can lock-with-conflict rather than error. Optionally (2) provide a small
> retry-on-`WriteConflict` helper for `WakeUpModeNormal` users, since the
> equivalent driver logic lives in TiDB and is absent from the pure-Rust stack.
> A feature-parity enhancement; current default behavior unchanged.

---

## Related issues

- **#486** (open) — HughPenn's "what causes this WriteConflict"; pingyu explained;
  you already commented with the root cause. Natural home for Framing 1.
- **#487, #482** (open) — duplicates of #486.
- **#328** (closed) — `lock_keys` blocking / `Failed to resolve lock` ergonomics
  (tangential prior art).
- No existing fair-locking / `WakeUpModeForceLock` enhancement request — Framing
  2 would not be a duplicate.
