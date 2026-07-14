# TiKV client rules — MUST / SHOULD / CAN

**Date:** 2026-07-14 · evaluated at the same pins as [parity-roadmap.md](parity-roadmap.md)
(client-rust `e53837d`, client-go `v2.0.8-0.20260708122311`, kvproto as vendored in
client-rust's `proto/`).

## 0. What this is, and where the rules come from

A normative baseline for a TiKV client, in three tiers. The roadmap says *what to build,
in what order*; this document says *what each gap weighs*.

- **MUST** — violating it can corrupt data, break atomicity or isolation, strand locks,
  wedge GC, or misreport an outcome. A MUST violation is a bug to file upstream
  immediately, and a `ledger.toml` G-entry the moment a scenario reproduces it.
- **SHOULD** — production quality demands it; skipping is legitimate only with a
  documented reason. SHOULD gaps are roadmap Phase 2–3 material.
- **CAN** — an optional capability. Absence is honest: the client answers
  `unsupported`, and the harness records it as a capability claim
  (`oracle=ok / subject=unsupported`). A client MUST NOT pretend to a capability it
  lacks — but lacking it breaks no rule.

**Sources, in order of authority.** (1) **kvproto** — TiKV's own wire contract; its
comments are the server telling clients what to do, and every rule that can be grounded
there is, cited as `kvrpcpb.proto:line` / `errorpb.proto:line` against the copy vendored
in client-rust (kvproto is the TiKV project's interface repository; there is no separate
TiKV server checkout in this workspace, and none is needed — the contract *is* the
interface). (2) **client-go** — the reference implementation; where the contract is
implicit, the oracle's behaviour at the pin defines it. client-go complies with every
rule below unless a row says otherwise — that is precisely what makes it the oracle.
(3) **client-rust** — the graded subject: ✅ complies · ⚠️ partial · ❌ gap · n/a.
Grades link to the roadmap section or ledger entry that carries the work.

Rule ids are stable (`WR-3` stays `WR-3`); new rules append.

## TS — Timestamps & ordering

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| TS-1 | MUST | Every `start_ts` and `commit_ts` comes from PD's TSO (or a TiKV-sanctioned derivation such as `min_commit_ts` inference) — never a local clock. Percolator's ordering rests on it. | `PrewriteRequest.start_version` / oracle `oracles/pd.go` | ✅ `pd/timestamp.rs` |
| TS-2 | MUST | `commit_ts > start_ts`, always. The contract states it verbatim. | `kvrpcpb.proto` CommitRequest: *"Must be greater than `start_version`"* | ⚠️ TSO path guarantees it, but the async-commit fallback defect commits at ts 0 (WR-5) — violated on that path |
| TS-3 | SHOULD | Batch TSO requests; one round-trip per timestamp does not survive load. | oracle PD-client batching | ✅ `pd/timestamp.rs:37` (batch ≤ 64) |
| TS-4 | CAN | Low-resolution / stale timestamp sources for staleness-tolerant reads (constraints in RD-3). | oracle `oracle/oracle.go:52-82` | ❌ — roadmap §6 |

## WR — Writes & two-phase commit

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| WR-1 | MUST | Pick exactly one primary key; its lock is *"the source of truth for the state of a transaction"*; all secondary locks point to it. | `kvrpcpb.proto` PrewriteRequest.primary_lock comment (verbatim) | ✅ |
| WR-2 | MUST | Prewrite everything before committing anything; commit the primary before any secondary. Until the primary commit is durable the transaction must remain rollbackable — this is WR-1's consequence. | oracle `2pc.go` execute ordering | ✅ `transaction.rs:1414` then background secondaries `:1303` |
| WR-3 | MUST | If the primary commit's outcome is unknowable (e.g. RPC timeout after send), report **undetermined** — never clean failure. The data may be committed. | oracle `2pc.go:2415` setUndeterminedErr | ✅ `UndeterminedError`, `transaction.rs:1295/1406` |
| WR-4 | MUST | When the client *knows* a transaction failed, its prewritten/pessimistic locks must be actively cleaned up, not abandoned to TTL. | oracle `2pc.go:1643` + `cleanup.go` | ⚠️ explicit `rollback()` only; a failed pessimistic commit left prewrite locks behind — #545, fix #547 in flight |
| WR-5 | MUST* | Async commit: list all secondaries in the primary's prewrite; adopt the server-derived commit ts; **fall back to 2PC when the server declines** (`min_commit_ts == 0` response). | `kvrpcpb.proto` PrewriteRequest: *"`secondaries` should be set as the key list of all secondary locks"*; oracle `prewrite.go:616` | ❌ no fallback — a `min_commit_ts == 0` response flows unfiltered through `.max()` (`transaction.rs:1364-1371`) into a **silent commit at ts 0** (the FIXME'd `unwrap()` at `:1290` succeeds); roadmap §5.3 |
| WR-6 | MUST* | 1PC: only for single-region transactions; when the server declines, fall back to 2PC. | `kvrpcpb.proto` try_one_pc comment | ⚠️ disables 1PC when multi-region (`requests.rs:283`) ✅, but returns `OnePcFailure` instead of falling back (`transaction.rs:1356`) |
| WR-7 | MUST* | 1PC/async commit: set and respect `max_commit_ts` (schema-change consistency bound); on `CommitTsTooLarge`, abort or fall back. | `kvrpcpb.proto` max_commit_ts comment; KeyError.commit_ts_too_large | ❌ never set — `transaction.rs:1340` FIXME; roadmap §5.3 |
| WR-8 | SHOULD | Keep long transactions alive: heartbeat the primary's TTL (`TxnHeartBeat`) while the transaction is open. | `kvrpcpb.proto` TxnHeartBeatRequest; oracle ttlManager `2pc.go:1256` | ✅ on by default (`transaction.rs:1119`); parity details (min_commit_ts advance) roadmap §6, #312 |
| WR-9 | SHOULD | Pre-split writes that would exceed raft entry limits; an oversized batch retried unchanged fails unchanged. | `errorpb.proto:105-113` RaftEntryTooLarge | ✅ txn prewrite/commit batch-split (#390); raw batches size-split at 16 KB since #501 (`raw/requests.rs:45,219-221`) — upstream #500 is stale |

\* conditional MUST: binds only if the client uses the capability at all.

## PL — Pessimistic locking

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| PL-1 | MUST | Each locking statement carries its own `for_update_ts`, refreshed from TSO on conflict retry — statement retry, not transaction retry. | `kvrpcpb.proto:177-182` (the `SELECT … FOR UPDATE` comment) | ✅ `transaction.rs:846` push_for_update_ts |
| PL-2 | MUST | Locks acquired for a statement that then fails are pessimistic-rolled-back promptly, not left to block others for a TTL. | oracle `txn.go:1863` asyncPessimisticRollback | ⚠️ rolls back the failed request's own keys (`transaction.rs:868/896`); lifecycle gaps tracked with WR-4 |
| PL-3 | SHOULD | Expose lock-wait control. The wire is explicit: *"0 means using default timeout in TiKV. Negative means no wait."* Callers need both. | `kvrpcpb.proto:185-187` | ❌ hardcodes `0` (server default) — `requests.rs:413`; no no-wait, no bound; roadmap §5.2 |
| PL-4 | SHOULD | Set `is_first_lock` when true — it lets the server skip deadlock detection it cannot otherwise skip. | `kvrpcpb.proto:183-184` | ❌ always `false` — `requests.rs:412` |
| PL-5 | SHOULD | Surface `Deadlock` distinctly: the contract designates it for *single-statement* rollback, so callers must be able to catch it and retry the statement. | `kvrpcpb.proto` KeyError.deadlock comment | ⚠️ arrives inside opaque `KeyError` — roadmap §5.1 |
| PL-6 | CAN | `return_values`, `check_existence`, `lock_only_if_exists` on lock requests. | `kvrpcpb.proto:191-204` | ⚠️ return_values via `get_for_update` only; others ❌ — roadmap §5.2 |
| PL-7 | CAN | Fair locking (`wake_up_mode = WakeUpModeForceLock`). | `kvrpcpb.proto:205-206` | ❌ — #546, roadmap §7 |

## RD — Reads & snapshots

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| RD-1 | MUST | A transaction reads its own uncommitted writes (local buffer merged over the snapshot). | oracle membuf semantics | ✅ `buffer.rs:54/73` |
| RD-2 | MUST | A read at ts T never *ignores* a lock with `start_ts ≤ T`: *"Client should backoff or cleanup the lock then retry."* Skipping it forfeits snapshot isolation. | `kvrpcpb.proto` KeyError.locked comment (verbatim) | ✅ waits/resolves — efficiency caveat in LR-4 |
| RD-3 | MUST* | Stale read only against replicas whose `safe_ts ≥ start_ts`; on `DataIsNotReady`, retry elsewhere or fall back to leader — never serve the read anyway. | `kvrpcpb.proto:847-849`; `errorpb.proto:141-146` | n/a — capability absent (RD-5) |
| RD-4 | CAN | Follower / replica read (`Context.replica_read`). | `kvrpcpb.proto:834` | ❌ — roadmap §5.4 |
| RD-5 | CAN | Stale read (`Context.stale_read`, constraints in RD-3). | `kvrpcpb.proto:849` | ❌ — roadmap §5.4 |
| RD-6 | CAN | Isolation levels beyond SI (RC, RCCheckTS). | `kvrpcpb.proto` IsolationLevel enum | ❌ always SI — roadmap §5.4 |

## LR — Lock resolution

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| LR-1 | MUST | A foreign lock is resolved through its **primary**: `CheckTxnStatus` on the primary decides, and the tri-state decode is exact — locked (`lock_ttl > 0`), committed (`commit_version > 0`), rolled back (both zero). | `kvrpcpb.proto:332-335` (response comment) | ✅ `lock.rs:426/527` |
| LR-2 | MUST | Never roll back a lock before its TTL expires unless the transaction's status is already known — `caller_start_ts`/`current_ts` exist to make expiry checks honest. | `kvrpcpb.proto:302-306` | ✅ |
| LR-3 | MUST | An expired primary with no lock and no write record is escalated with `rollback_if_not_exist`, leaving a rollback tombstone — otherwise the orphan is immortal. | `kvrpcpb.proto:307-309`; oracle `lock_resolver.go:1001` | ❌ at pin — escalation is dead code (**G-0001**, #543); fix #544 in flight |
| LR-4 | MUST | Async-commit locks are never unilaterally rolled back: resolve by checking all secondaries (`CheckSecondaryLocks`), then commit or roll back *by the actual outcome*. | `kvrpcpb.proto` LockInfo.use_async_commit/secondaries; oracle `lock_resolver.go:1271` | ⚠️ correct half (never unilateral) ✅; resolving half exists only on the GC path (`lock.rs:336-380`) — reads just wait (`lock.rs:117`); roadmap §5.7 |
| LR-5 | MUST | `ResolveLock` carries the transaction's true outcome: `commit_version == 0` rolls back, `> 0` commits at that ts. Never guessed. | `kvrpcpb.proto:492-494` | ✅ |
| LR-6 | SHOULD | Set `verify_is_primary` on every `CheckTxnStatus` — *"For new versions, this field should always be set to true"* (guards a changed-primary corner, tidb#42937). | `kvrpcpb.proto:318-323` | ✅ `requests.rs:648` |
| LR-7 | SHOULD | Cache resolved transaction statuses for the duration of a resolve pass. | oracle `lock_resolver.go:250` | ✅ `lock.rs:234` |
| LR-8 | CAN | Lite resolve (skip full-region resolve below a lock-count threshold). | oracle `lock_resolver.go:68` | ❌ deliberately disabled — `requests.rs:229`; roadmap §5.7 |

## RE — Region errors, cache & retry

The server's routing errors are **facts about the cluster, not about the data**. The
contract sorts them into three classes, and the class dictates the reaction.

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| RE-1 | MUST | Routing-stale errors (`NotLeader`, `EpochNotMatch`, `StoreNotMatch`, `RegionNotFound`, `StaleCommand`, `MismatchPeerId`) → refresh the region cache **using the hints in the error** (`NotLeader.leader`, `EpochNotMatch.current_regions`) and retry internally. They must not surface as data errors. | `errorpb.proto:15-22,45-52,81-87,99-103,164-169` | ✅ core five handled (`request/plan.rs:288-326`) |
| RE-2 | MUST | Backoff-and-resend errors (`ServerIsBusy` — which even *suggests* `backoff_ms` — and `MaxTimestampNotSynced`: *"client can backoff and resend"*) → retry with backoff, honouring the hint. | `errorpb.proto:89-97,115-120` | ❌ surfaced to the caller immediately (`request/plan.rs:327-331`), grouped with genuinely fatal errors |
| RE-3 | MUST | Non-retryable errors are failed fast: `FlashbackInProgress` is marked *"non-retryable, the request should fail ASAP"*. Blind-retrying it inverts the contract. | `errorpb.proto:193-195` | ⚠️ unmatched variants fall through to invalidate-and-retry (`request/plan.rs:332-338`) — backwards for exactly this class |
| RE-4 | MUST | Retries are bounded and backed off; on exhaustion the region error surfaces *as* a region error (distinct from data errors). | oracle `region_request.go` | ✅ bounded backoff presets |
| RE-5 | SHOULD | Backoff is tuned per error class (a region miss and a disk-full event do not deserve the same curve). | oracle `config/retry/config.go:122-140` (~18 configs) | ❌ 4 generic presets (`backoff.rs:10-13`) — roadmap §6 |
| RE-6 | SHOULD | The region cache refreshes proactively (TTL / background reload), not only on error. | oracle `region_cache.go:828,1809` | ❌ on-error only — roadmap §6 |
| RE-7 | CAN | Bucket awareness (*"client should update the buckets version and retry"*), load-based replica routing, leader-isolated forwarding. | `errorpb.proto:31-36`; oracle `replica_selector.go` | ❌ — roadmap §7 |

## ER — Error reporting to the caller

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| ER-1 | MUST | Never misreport an outcome: failure is not success, unknown is not known (WR-3), and an *unrecognized* server error stays an error — it never becomes `ok` or `not found`. | contract-wide | ✅ structurally |
| ER-2 | MUST | "Key absent" is a result, not an error, and is distinguishable from "read failed". | Get semantics | ✅ `Ok(None)` model |
| ER-3 | MUST | Honour the `KeyError` routing semantics: `locked` → backoff/resolve + retry; `retryable` → *"client may restart the txn"*; `abort` → *"client should abort the txn"*. | `kvrpcpb.proto` KeyError field comments (verbatim) | ⚠️ `locked` machinery ✅; retryable-vs-abort is not surfaced as a distinction — roadmap §5.1 |
| ER-4 | SHOULD | Expose typed errors for programmatic handling (write conflict, deadlock, already-exists, txn-too-large…). Users steering on error *strings* is the failure mode. | oracle `error/error.go` (~26 sentinels + ~18 types) | ❌ one enum, opaque `KeyError` (`common/errors.rs:28`) — #486/#487 show the cost; roadmap §5.1 |

## GC — Garbage collection & safepoints

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| GC-1 | MUST | Before advancing a GC safepoint: scan and resolve **all** locks with `start_ts ≤ safepoint`. Advancing first can collect the data needed to ever resolve them. | `kvrpcpb.proto` ScanLockRequest.max_version; oracle `tikv/gc.go` flow | ✅ `client.rs:263` gc() resolves then advances |
| GC-2 | MUST | Never advance the *cluster* safepoint past a snapshot some service still reads. On shared clusters that means service safepoints (or the newer GC controller), not the bare cluster knob. | oracle GC via controller: `tikv/gc.go:71` → `GetGCInternalController(...).AdvanceTxnSafePoint`; `UpdateServiceGCSafePoint` on the PD client (vendored `pdpb.proto:80`) | ❌ only `UpdateGcSafePoint` (`pd/cluster.rs:88`) — roadmap §5.8 |
| GC-3 | SHOULD | *Expose* service-safepoint registration so a long-lived snapshot holder can protect itself — neither client registers snapshots automatically; the rule is that the capability must exist. | PD-client `UpdateServiceGCSafePoint` (caller-invoked; oracle exposes its PD client) | ❌ no way to register one at all — roadmap §5.8 |

## RAW — RawKV

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| RAW-1 | MUST | No RawKV alongside TxnKV in API V1: *"`V1` … is not safe to use RawKV along with the others."* V2's keyspace prefixes (`r`/`x`) are what makes coexistence safe. | `kvrpcpb.proto` APIVersion enum comment (verbatim) | ✅ V2 codecs (`request/keyspace.rs`); V1 carries the same caveat as the oracle |
| RAW-2 | MUST | A V1 client never sends V1TTL: *"V1 client should always send `V1`."* | `kvrpcpb.proto` APIVersion V1TTL comment | ✅ (no V1TTL mode at all — see RAW-4) |
| RAW-3 | MUST | Atomic operations (CAS) only in atomic mode, and atomic/non-atomic writes are not mixed on the same keys — CAS's guarantee evaporates otherwise. | oracle `rawkv.go:185` SetAtomicForCAS | ✅ `assert_atomic()` guard (`raw/client.rs:691`) |
| RAW-4 | CAN | TTL, `Checksum`, the V1TTL api-version mode. | oracle `rawkv.go:615,365` | TTL ✅ · Checksum ❌ (**G-0002**, machine-checked; roadmap §5.6) · V1TTL mode ❌ |

## NET — Transport & identification

| id | level | rule | evidence | client-rust @ pin |
|---|---|---|---|---|
| NET-1 | SHOULD | Batch RPCs per store at scale (`BatchCommands` streaming); one unary call per op collapses under load. | `tikvpb.proto:91`; oracle `client_batch.go` | ❌ unary per request (`store/request.rs:38-45`) — #442, roadmap §7 |
| NET-2 | SHOULD | gRPC keepalive on store connections, so dead peers are detected rather than waited on. | oracle `config/client.go:194-195` | ✅ present (10s/3s) though hardcoded (`common/security.rs:141-146`) — configurability is roadmap §6 |
| NET-3 | SHOULD | Identify workloads on shared clusters: `request_source`, resource-group context. Unlabelled traffic is invisible to server-side attribution and governance. | `kvrpcpb.proto` Context fields; oracle `util/request_source.go` | ❌ — roadmap §6/§7 |
| NET-4 | CAN | Compression, window tuning, connection pooling, forwarding. | oracle `config/client.go` | gzip ✅ (`store/client.rs:41`); rest ❌ — roadmap §7 |

## Scoreboard, and how to use this document

client-rust at the pin, across 56 rules (31 MUST — one n/a — 15 SHOULD, 10 CAN):

|  | MUST (incl. conditional) | SHOULD | CAN |
|---|---|---|---|
| ✅ complies | 18 | 6 | — |
| ⚠️ partial / mixed | 7 (TS-2, WR-4, WR-6, PL-2, LR-4, RE-3, ER-3) | 1 (PL-5) | 3 (PL-6, RAW-4, NET-4) |
| ❌ gap | **5 (WR-5, WR-7, LR-3, RE-2, GC-2)** | 8 | 7 |
| n/a (capability absent) | 1 (RD-3) | — | — |

The five MUST-❌ rows are the priority queue, and the roadmap carries all five:
WR-5/WR-7 (roadmap §5.3), LR-3 (G-0001, fix #544 in flight), GC-2 (roadmap §5.8), and
RE-2 — a finding surfaced by writing this document: `ServerIsBusy` (which ships a
suggested `backoff_ms`) and `MaxTimestampNotSynced` ("client can backoff and resend")
are propagated to the caller instead of retried (`request/plan.rs:327-331`). RE-2 is
already folded into roadmap §6's retry-taxonomy work; it still needs its upstream
issue.

When a new divergence appears in the harness, grade it here first: a MUST violation
outranks any SHOULD, and a CAN gap is not a bug at all — it is an `unsupported`
capability claim waiting for a scenario.

Rules are re-graded whenever the pins move; the grades above are claims about
`e53837d`, not about client-rust in general.
