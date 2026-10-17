# AGENTS.md — client-rust-test

Context for coding agents working in this repository, including the leaves of the
`client-rust-pdca` loop. Read the README first: it states what the harness is and the
rules that make its verdicts mean something.

## Review rubric & protocol

This section is the standing rubric the loop's builder, reviewer and adversary apply
(`client-rust-pdca` `pdca.toml` `rubric_file`). Its client rules are condensed from
[`docs/client-rules.md`](docs/client-rules.md), which is the normative source: on any
disagreement, that file wins, and this section is stale.

### Hard conventions (MUST)

- **The harness carries no workarounds for client bugs.** A client deficiency is a
  failing test or a ledger divergence, never code in this repo that papers over it.
- **Never emulate a missing feature.** A driver whose client lacks a capability answers
  `unsupported` with a detail string. A scan-and-hash "checksum", a client-side retry the
  client does not do, or any stand-in that makes two clients agree is a defect.
- **Canonicalize the question, never the evidence.** Observations keep `class`, `proto`
  and `native` as captured. A change that drops or normalizes evidence at capture time is
  a defect; widening a projection is the allowed move.
- **`parity-runner` links neither client** (enforced in its `Cargo.toml`). Nothing that
  decides a verdict may depend on `tikv-client` or client-go.
- **The oracle is pinned by module, never replaced.** No `replace` of
  `github.com/tikv/client-go/v2` in `go/go.mod`; no tracked `go/go.work`.
- **One place names a revision: `pins.toml`.** `make pins-check` checks only these
  consumers against it: `rust-toolchain.toml`, `go/go.mod` (the client-go require, the `go`
  directive, no `replace`), `go/go.sum` present, no tracked `go/go.work`, the sibling
  client-go checkout's revision (when one exists), the client-rust pin's ancestry (on the
  fork's `integration` branch, with `upstream_base` on upstream master — ADR-0001), and that `cluster/docker-compose.yml` takes its images from
  `cluster/images.env` instead of a hard-coded tag (the 3-store compose file is not
  checked). It does **not** check revisions quoted in
  prose: `README.md`'s gate-verdict heading and evidence must be updated by hand on a pin
  move, and `docs/parity-roadmap.md`, `docs/client-rules.md` and
  `docs/upstream-issue-map.md` are dated snapshots that name the revision they were written
  at — re-derive them, never edit only the revision. A loop bundle never edits `pins.toml`
  (design R3); a pin move is a human or `pin-move` bundle.
- **A ledger entry declares a polarity and its fields.** `expect = "diverges"` needs the
  exact predicted fields; an entry a bundle did not declare is not edited by it.
- **Parity is one-directional: Rust ⊇ Go.** Rust-only surface is not a gap
  (`docs/parity-roadmap.md` §1.3).

### Client rules a change to tikv-client must not break (MUST; source: docs/client-rules.md)

One line per rule, kept to the source's conditions. `*` marks a conditional MUST: it binds
only if the client uses that capability. Where a line and the source differ, the source
wins and this line is a defect.

- **TS-1** — every `start_ts` and `commit_ts` comes from PD's TSO, or from a
  TiKV-sanctioned derivation such as `min_commit_ts` inference — never a local clock.
- **TS-2** — `commit_ts > start_ts`, always.
- **WR-1** — exactly one primary key; its lock is the source of truth for the transaction,
  and every secondary lock points to it.
- **WR-2** — 2PC: prewrite everything before committing anything; commit the primary
  before any secondary; until the primary commit is durable the transaction stays
  rollbackable. Async commit / 1PC\*: once every prewrite has succeeded *as async commit*
  (each response carries a non-zero `min_commit_ts`; a zero means TiKV declined — WR-5) or,
  for 1PC, once a non-zero `one_pc_commit_ts` is returned (zero means TiKV declined), the
  transaction IS committed and must never be rolled back.
- **WR-3** — if the outcome at the commit point is unknowable — 2PC: the primary commit;
  async commit / 1PC\*: any prewrite — because an RPC was lost after send or TiKV returned
  `UndeterminedResult`, report *undetermined*, never clean failure, and do no cleanup. A
  later error on a retry does not make an earlier lost attempt determined; only a success,
  an answer proving the earlier attempt failed (the primary lock is still there, e.g.
  `CommitTsExpired`), or a confirmed fallback (an *async-commit* prewrite answered
  `min_commit_ts == 0`: TiKV declined, so the transaction cannot have committed as async
  commit and no commit was sent) does. A 1PC decline, or a 2PC re-prewrite after a lost
  async-commit/1PC attempt, does not.
- **WR-4** — when the client knows a transaction failed, its prewritten and pessimistic
  locks are actively cleaned up, not abandoned to TTL.
- **WR-5\*** — async commit: list **all** secondaries in the primary's prewrite; adopt the
  server-derived commit ts; fall back to 2PC when the server declines (a
  `min_commit_ts == 0` response).
- **WR-6\*** — 1PC only for single-region transactions; when the server declines, fall
  back to 2PC.
- **WR-7\*** — 1PC / async commit: set and respect `max_commit_ts`; on
  `CommitTsTooLarge`, abort or fall back.
- **PL-1** — each locking statement carries its own `for_update_ts`, refreshed from TSO on
  conflict retry (statement retry, not transaction retry).
- **PL-2** — locks acquired for a statement that then fails are pessimistic-rolled-back
  promptly, not left to block others for a TTL.
- **RD-1** — a transaction reads its own uncommitted writes.
- **RD-2** — a read at ts T never ignores a lock with `start_ts ≤ T`: back off or clean up
  the lock, then retry.
- **RD-3\*** — stale read only against replicas whose `safe_ts ≥ start_ts`; on
  `DataIsNotReady`, retry elsewhere or fall back to the leader — never serve the read
  anyway.
- **LR-1** — a foreign lock is resolved through its primary: `CheckTxnStatus` on the primary
  decides. Locked is `lock_ttl > 0`, committed is `commit_version > 0`, and both zero is
  rolled back **only** when the response's `action` confirms a rollback record (`NoAction`,
  `LockNotExistRollback`, `TtlExpireRollback`). With `LockNotExistDoNothing` or
  `TtlExpirePessimisticRollback` (a pessimistic lock) TiKV writes no record and the
  transaction may still be alive — it can move its primary (pingcap/tidb#42937) — so the
  status is undetermined: never cached, never resolved as rolled back (client-go
  `TxnStatus.IsRolledBack` / `IsStatusDetermined`). Those actions only occur because
  `CheckTxnStatus` for a pessimistic lock sets `resolving_pessimistic_lock`; that lock is
  then cleaned with `PessimisticRollback` of the lock itself, never with `ResolveLock`.
- **LR-2** — never roll back a lock before its TTL expires unless the transaction's status
  is already known.
- **LR-3** — an expired primary with no lock and no write record is escalated with
  `rollback_if_not_exist`, leaving a rollback tombstone.
- **LR-4** — async-commit locks are never unilaterally rolled back: check all secondaries
  (`CheckSecondaryLocks`), then commit or roll back by the actual outcome.
- **LR-5** — `ResolveLock` carries the transaction's true outcome: `commit_version == 0`
  rolls back, `> 0` commits at that ts. Never guessed.
- **RE-1** — routing-stale errors (`NotLeader`, `EpochNotMatch`, `StoreNotMatch`,
  `RegionNotFound`, `StaleCommand`, `MismatchPeerId`) refresh the region cache using the
  hints in the error and retry internally; they never surface as data errors.
- **RE-2** — back-off-and-resend errors (`ServerIsBusy`, honouring its `backoff_ms`;
  `MaxTimestampNotSynced`) are retried with backoff.
- **RE-3** — non-retryable errors fail fast (`FlashbackInProgress`: fail ASAP, never
  blind-retry).
- **RE-4** — retries are bounded and backed off; on exhaustion the region error surfaces as
  a region error, distinct from data errors.
- **ER-1** — never misreport an outcome: failure is not success, unknown is not known, and
  an unrecognized server error stays an error.
- **ER-2** — "key absent" is a result, not an error, and is distinguishable from "read
  failed".
- **ER-3** — honour `KeyError` routing: `locked` → back off / resolve and retry;
  `retryable` → the client may restart the txn; `abort` → the client aborts the txn.
- **GC-1** — before advancing a GC safepoint, scan and resolve **all** locks with
  `start_ts ≤ safepoint` (at or below it, the boundary included).
- **GC-2** — never advance the cluster safepoint past a snapshot some service still reads;
  on shared clusters that means service safepoints or the GC controller.
- **RAW-1** — no RawKV alongside TxnKV in API V1; API V2's keyspace prefixes make
  coexistence safe.
- **RAW-2** — a V1 client never sends V1TTL.
- **RAW-3** — CAS only in atomic mode, and atomic and non-atomic writes are never mixed on
  the same keys.

### Recurring defect classes (MUST check when the diff touches the surface)

- **Vacuous assertion** — a test that passes without binding the property (e.g. asserts
  the call returned, not what it returned). Mutants on the diff catch these.
- **Converging by the wrong mechanism** — a gap that closes because both sides now fail
  the same way, or because the subject computes locally what the oracle asks the server
  for. Check that the subject sends the same kvproto request the oracle sends.
- **Evidence loss** — a projection or driver change that removes a field from a trace.
- **Unbounded await** — a new await on a network path without a deadline.
- **Keys or values in logs** — log lines carry `start_ts` and identifiers, never user
  key or value bytes.

### Reviewer protocol

- Cite `path:line` on the target tree for every finding.
- Do not raise findings about `Signed-off-by` or commit metadata.
- A finding answered by a tracked issue is settled; do not re-raise it.
