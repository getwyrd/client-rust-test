# client-rust-test

The **wyrd M4 evaluation gate** for [`tikv-client`](../client-rust)
(tikv/client-rust): a standalone test harness that prototypes wyrd's
`MetadataStore` contract over the client's transactional API and drives it
against a real TiKV/PD cluster.

## Why this exists

Wyrd's Milestone 4 (proposal 0015 in the wyrd repo,
`docs/design/proposals/accepted/0015-milestone-4-production-metadata-backend-revised.md`)
swaps the embedded redb metadata backend for TiKV behind the unchanged
`MetadataStore` seam — resting the production-durability tier on the
**pre-1.0** `tikv-client` crate. The proposal names that maturity risk and
requires an *evaluation gate* before the dependency is committed:

- confirm the **locking-read** entry point (`get_for_update` / `lock_keys`),
- confirm the **write-conflict error path** (`Error::KeyError` wrapping a
  `WriteConflict`) is distinguishable from genuine faults,
- confirm the client's futures are **`Send`** behind the object-safe trait,
- confirm values are stored **byte-identically** (the trait's CAS is
  value-equality over the whole record),
- confirm the **multi-key atomic `commit(WriteBatch)`** mapping holds under
  real contention.

This repo is that gate:

- [`src/traits.rs`](src/traits.rs) — the `MetadataStore` contract, **vendored
  verbatim** from wyrd (`crates/traits/src/lib.rs` @ 7009c2e). Copied, not
  depended on, so the harness stays standalone; M4's premise is that this
  surface is frozen, and any needed re-copy would itself be a gate finding.
- [`src/lib.rs`](src/lib.rs) — `TikvMetadataStore`, the exact
  `WriteBatch → one TiKV transaction` translation the future `metadata-tikv`
  crate will use: pessimistic `get_for_update` by default, optimistic +
  `lock_keys` as the measured alternative, write-conflict → `Ok(Conflict)`
  classification, paged native prefix scan, rollback discipline.
- [`tests/gate.rs`](tests/gate.rs) — the gate proper; every test names the
  proposal obligation it verifies.

## Layout

Developed against a sibling checkout of the crate under test (a path
dependency, so local work on client-rust is what the gate exercises):

```
wyrd/
├── client-rust/        # tikv/client-rust — the crate under test
└── client-rust-test/   # this harness (depends only on ../client-rust)
```

Toolchain is pinned to 1.93.0, matching client-rust.

## Usage

```sh
make gate           # one-shot: cluster up, wait ready, run everything
make cluster-down   # tear down the cluster and its data

# individually:
make cluster-up     # docker-compose PD + TiKV v8.5.5 (client-rust's CI pin)
make unit-test      # cluster-free tests (prefix math, error classification)
make gate-test      # the cluster-backed gate ($PD_ADDRS, default 127.0.0.1:2379)
make check          # fmt --check, clippy -D warnings, cargo check
```

The cluster ([`cluster/`](cluster/)) is a throwaway single-node PD + TiKV with
client-rust CI's aggressive region-split thresholds, so multi-key transactions
genuinely span Raft regions — the property M4 depends on.

## Gate verdict (v8.5.5 cluster, client-rust `master` @ e53837d)

> Run against an unmodified client-rust checkout; confirm with
> `git -C ../client-rust describe --tags --always --dirty` (must read
> `e53837d`, no `-dirty`). The one-line fix that makes `d6` pass lives only in
> `findings/fix-check-txn-status-wrapper.patch`, never applied to the tree
> under test.
>
> **Design principle: the harness relies on client-rust; it carries no
> workarounds for client-rust bugs.** Where the client is deficient, that is a
> finding expressed as a **failing test**, fixed in client-rust — not papered
> over here. So on unfixed `e53837d` the gate is **16 green + `d6` red**: `d6`
> asserts the *correct* behavior (an orphaned lock must be resolved) and turns
> green when the #519 fix lands. Verified both ways: **17/17 green** against
> `e53837d + the fix` (`findings/gate-evidence.txt`,
> `findings/fix-check-txn-status-wrapper.patch`), where `d6`'s output shows the
> orphan being resolved after TTL; `d6` red against pristine `e53837d`.

**Confirmed working** — the proposal's mapping is implementable as specified:

| Obligation | Evidence |
|---|---|
| Multi-key all-or-nothing commit, `Conflict` as `Ok` with zero side effects | `a3`, `b1`–`b3` |
| Exactly-one-winner version CAS under contention; losers never `Err` | `c1`, `c2` (8 tasks × 4 rounds, both lock modes) |
| Write-skew on read-only precondition keys is real, and both `get_for_update` (pessimistic) and `lock_keys` (optimistic) close it | `c3` |
| Write-conflict error shape: `KeyError { conflict: Some(WriteConflict) }`, classifiable vs faults | `d1`, `d5` |
| `get_for_update` reads latest committed value (not the start snapshot) | `d2` |
| Byte-identical value storage (CAS soundness) | `a2` |
| Paged native prefix scan; adjacent-prefix and `0xFF` boundary hygiene | `d3`, `d4` |
| Futures are `Send`; store works behind object-safe `dyn MetadataStore` | compile + `a1` |

**Findings** — one genuine client-rust bug and two behavior gaps. The harness
does not work around any of them; each is surfaced by a test and belongs
fixed in client-rust.

1. **Bug (regression in #519) — an orphaned lock is never resolved; filing-ready**
   (`d6`, currently **red** on `e53837d`;
   [findings/](findings/txn-not-found-lock-resolution.md)). A lock on a
   secondary key whose primary was never written (crash between per-region
   prewrites, or a failed commit's residue) makes the key unreadable **and
   unwritable** forever: `MultipleKeyErrors([KeyError { txn_not_found }])`,
   unchanged by TTL expiry. The heal path (`rollback_if_not_exist` escalation
   in `get_txn_status_from_lock`) is dead code: `check_txn_status` matches
   `Error::ExtractedErrors`, but its plan shape can only deliver the key error
   as `Error::MultipleKeyErrors` — the sibling call sites with the opposite
   adapter order are why their identical match arms *do* work. Introduced by
   `7d80f59` (#519); the pre-#519 resolver recovered this state via legacy
   `Cleanup`. `d6` asserts the correct behavior (the orphan must be resolved
   on read after TTL) and so fails until the fix lands; the one-line fix
   ([findings/fix-check-txn-status-wrapper.patch](findings/fix-check-txn-status-wrapper.patch))
   makes it pass, validated twice (post-TTL self-heal; 63/63 upstream unit
   tests green). The harness carries **no janitor** — resolving orphaned locks
   is the client's responsibility. (A production backend forced to ship before
   the fix releases could run client-rust's own `cleanup_locks` maintenance
   API from a custodian, but that belongs in the backend/custodian, not in
   this evaluation harness.)
2. **Failed commits leave prewrite locks — one bug + one gap** (adversarially
   reviewed, confirmed against TiKV server source, **empirically reproduced**
   by `failpoint_gate.rs::d7` / `make failpoint-test`; filing-ready in
   [findings/pessimistic-rollback-leaves-prewrite-locks.md](findings/pessimistic-rollback-leaves-prewrite-locks.md)).
   Two halves:
   - **Bug (pessimistic-specific):** after a failed 2PC commit, even the
     caller's `Transaction::rollback()` leaves the already-placed prewrite
     locks behind and **reports `Ok`** — `Committer::rollback` sends
     `PessimisticRollback`, which TiKV applies only to `LockType::Pessimistic`
     locks and silently skips prewritten Put/Delete 2PC locks
     (`tikv/tikv pessimistic_rollback.rs`, with its own unit test). Optimistic
     mode is clean (`BatchRollback` clears them). Aggravated by the
     auto-heartbeat, which keeps *extending* the orphans' TTL until the txn is
     rolled back/dropped.
   - **Gap vs client-go:** `Committer::commit` does no proactive cleanup on
     failure; client-go's `twoPhaseCommitter.execute` defers a best-effort
     `cleanup()` (a `BatchRollback` over all keys — which *does* clear
     pessimistic prewrite locks), skipped only when committed/undetermined.

   The store here uses the API correctly — `rollback()` after every failed
   commit, skipped on `UndeterminedError` — and relies on **client-rust** to
   resolve the residue (which, with #543, self-heals on the next read; finding
   1). Complementary to #543 (prevent-at-source vs cure-on-read), no duplicate
   (closest: #528, #235, #313).
3. **Woken pessimistic lock waiters surface a `WriteConflict` — by-design, not
   a bug** (`d5`, `c3` phase 3; adversarially reviewed). A waiting
   `get_for_update` woken by another txn's commit receives a **genuine**
   `WriteConflict` (`reason: PessimisticRetry`) whenever a commit landed on the
   key at `commit_ts > for_update_ts` — including the `Op::Lock`-only commit a
   `get_for_update` itself writes, so the *value* can be unchanged yet the
   conflict is real. This is **correct** and **matches client-go's default
   `LockKeys`** (which also surfaces `ErrWriteConflict`; the transparent
   retry-with-fresh-`for_update_ts` lives in **TiDB**, not the client). So it
   is *correct usage*, not a workaround: the caller restarts at a fresh
   `for_update_ts`, exactly what the trait's `CommitOutcome::Conflict` contract
   requires. The one genuine parity gap is a **feature, not a bug**:
   client-rust lacks client-go's opt-in *fair/aggressive locking*
   (`WakeUpModeForceLock` — `new_pessimistic_lock_request` never sets
   `wake_up_mode`), which would lock-with-conflict instead of erroring and
   spare rename-heavy workloads the restart. Existing thread:
   tikv/client-rust#486.

(A fourth candidate — "`snapshot()` needs explicit `.read_only()` or drops
panic" — did **not** survive verification: `TransactionClient::snapshot`
applies `.read_only()` internally, client.rs:230. Retracted.)

## Prerequisites

- Rust 1.93.0 (pinned by `rust-toolchain.toml`)
- Docker with the compose plugin (for the local cluster), or any reachable
  TiKV ≥ v5.0 cluster via `$PD_ADDRS` (comma separated)

## Contributing

`main` is protected: changes land via pull request (force-push and deletion
are blocked, linear history required). Enable the version-controlled pre-push
hook once per clone so `make check` runs before anything leaves your machine:

```sh
git config core.hooksPath .githooks   # runs make check on push; bypass with --no-verify
```

## License

Apache-2.0 — see [LICENSE](LICENSE).
