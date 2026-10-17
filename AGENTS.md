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
- **One place names a revision: `pins.toml`.** Every other consumer is checked against it
  by `make pins-check`. A loop bundle never edits `pins.toml` (design R3); a pin move is a
  human or `pin-move` bundle.
- **A ledger entry declares a polarity and its fields.** `expect = "diverges"` needs the
  exact predicted fields; an entry a bundle did not declare is not edited by it.
- **Parity is one-directional: Rust ⊇ Go.** Rust-only surface is not a gap
  (`docs/parity-roadmap.md` §1.3).

### Client rules a change to tikv-client must not break (MUST; source: docs/client-rules.md)

- **TS-1/TS-2** — timestamps come from PD's TSO; `commit_ts > start_ts`, always.
- **WR-1..WR-4** — one primary; prewrite all before committing any, primary before
  secondaries; an unknowable primary-commit outcome is *undetermined*, never failure;
  locks of a known-failed transaction are cleaned up, not left to TTL.
- **WR-5..WR-7** (if async commit / 1PC is used) — fall back to 2PC when the server
  declines; set and respect `max_commit_ts`.
- **PL-1/PL-2** — each locking statement carries its own refreshed `for_update_ts`;
  locks of a failed statement are pessimistic-rolled-back promptly.
- **RD-1..RD-3** — read your own writes; never ignore a lock with `start_ts ≤ T`; stale
  reads only where `safe_ts ≥ start_ts`.
- **LR-1..LR-5** — resolve through the primary with an exact tri-state decode; no
  rollback before TTL unless the status is known; escalate with `rollback_if_not_exist`;
  never unilaterally roll back async-commit locks; `ResolveLock` carries the true outcome.
- **RE-1..RE-4** — routing-stale errors refresh the cache from the error's hints and
  retry; back-off-and-resend errors are retried honouring the hint; non-retryable errors
  fail fast; retries are bounded and surface as region errors on exhaustion.
- **ER-1..ER-3** — never misreport an outcome; "absent" is a result, not an error; honour
  `KeyError`'s `locked` / `retryable` / `abort` routing.
- **GC-1/GC-2** — resolve all locks below a safepoint before advancing it; never advance
  past a snapshot a service still reads.
- **RAW-1..RAW-3** — no RawKV beside TxnKV in API V1; a V1 client never sends V1TTL; CAS
  only in atomic mode.

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
