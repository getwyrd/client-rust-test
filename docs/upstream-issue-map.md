# Upstream issue map — tikv/client-rust's tracker against the roadmap

**Date:** 2026-07-14 · sources: the 72 open issues on tikv/client-rust as of this date,
[parity-roadmap.md](parity-roadmap.md), and the backlog milestones in this repo
(getwyrd/client-rust-test issues #7–#41, one milestone per roadmap phase).

Unlike the roadmap's `file:line` citations, an issue tracker is not pinnable — this map
is a dated snapshot. Re-derive it when the tracker moves; the backlog issue bodies carry
the same upstream references and are the living copy.

## 1. The mapping

Upstream issues/PRs are written fully qualified as `tikv/client-rust#NNN` so they link to
the right tracker when this doc is rendered here; a bare `#NN` is this repo's backlog.

### Phase 0 — in flight / engagement

| upstream | roadmap | ours |
|---|---|---|
| tikv/client-rust#543 orphaned lock (fix PR tikv/client-rust#544 in flight) | §3.1 | #7 |
| tikv/client-rust#545 pessimistic rollback (fix PR tikv/client-rust#547 in flight) | §3.2 | #8 |
| tikv/client-rust#534 detached JoinHandles (PR tikv/client-rust#535, not ours) | §3.3 | #9 |
| tikv/client-rust#267 logging — fixed by tikv/client-rust#548, issue still open | §3.4 (pin move closes it) | #10 |
| tikv/client-rust#506 the roadmap ask; the stale set (§2 below) | §9.1 / §9.3 | #11 |

### Phase 1 — dependencies

No upstream counterparts: the dependency modernization is this initiative's own
contribution. Upstream-hygiene issues that sit *near* W5 (their repo's CI/test
housekeeping, not ours to own): tikv/client-rust#448 (API-v1 integration tests),
tikv/client-rust#425 (cargo make), tikv/client-rust#285 (unit-test coverage),
tikv/client-rust#93 (oracle unit tests), tikv/client-rust#74 (nightly proptest CI).

### Phase 2

| upstream | roadmap | ours |
|---|---|---|
| **tikv/client-rust#238 "improve error handling"** (open since 0.2!); tikv/client-rust#486 / tikv/client-rust#487 / tikv/client-rust#482 as user-confusion evidence | §5.1 typed errors | #17 |
| tikv/client-rust#483 non-exclusive get_for_update; tikv/client-rust#308 pessimistic-lock caching (adjacent) | §5.2 LockCtx | #18 |
| — (to file; tikv/client-rust#497 is adjacent as a 2PC-atomicity question) | §5.3 async-commit safety | #19 |
| — (to file) | §5.4 replica/stale read | #20 |
| — | §5.5 snapshot knobs | #21 |
| — (to file; gap already machine-checked as G-0002) | §5.6 raw checksum | #22 |
| **tikv/client-rust#111** resolve specific keys; **tikv/client-rust#235** why no pessimistic-lock resolution; **tikv/client-rust#208** user control over resolution; tikv/client-rust#315 (triage) | §5.7 read-path resolution | #23 |
| **tikv/client-rust#180 "Support GC"** (2019) | §5.8 service safepoints | #24 |
| tikv/client-rust#512 reverse scan; tikv/client-rust#528 (triage); old raw bugs tikv/client-rust#380 / tikv/client-rust#331 / tikv/client-rust#377 / tikv/client-rust#488 as triage-adjacent | §5.9 known bugs | #25 |

### Phase 3

| upstream | roadmap | ours |
|---|---|---|
| tikv/client-rust#287 async commit + 1PC; **tikv/client-rust#189 large transactions** | complete async commit | #26 |
| tikv/client-rust#312 heartbeat timing (+tikv/client-rust#189 overlap) | TTL manager | #27 |
| (PR tikv/client-rust#540 TSO-v2 discovery is adjacent) | stale-read machinery | #28 |
| — | txn-option long tail | #29 |
| tikv/client-rust#310, tikv/client-rust#336, tikv/client-rust#337, tikv/client-rust#405 (backon); **tikv/client-rust#498** — a differential bug report ("works in Go, fails in Rust": Store Not Match / Not Leader) | backoff & region-error taxonomy | #30 |
| tikv/client-rust#299 (+PR tikv/client-rust#445 stale-cache fix) | region cache | #31 |
| tikv/client-rust#472 TLS; tikv/client-rust#382 msg size (+PR tikv/client-rust#414 custom DNS) | config & TLS | #32 |
| (PRs tikv/client-rust#470 optional-prometheus, tikv/client-rust#427 scan tracing) | observability | #33 |
| tikv/client-rust#489 MemDB staging; tikv/client-rust#311 memory limits | MemDB parity | #34 |
| **tikv/client-rust#284 fault injection / nemesis**; tikv/client-rust#389, tikv/client-rust#516, tikv/client-rust#525 (failpoint flakiness) | exported test probes | #35 |

### Phase 4

| upstream | roadmap | ours |
|---|---|---|
| tikv/client-rust#442 BatchCommands; tikv/client-rust#475 more channels per store; tikv/client-rust#288 benchmarking (acceptance criteria); stale PR tikv/client-rust#363 (superseded) | transport + conn pool | #36 |
| tikv/client-rust#493 offline-store connection refused (+tikv/client-rust#498 overlap) | store health & selector | #37 |
| tikv/client-rust#546 fair locking | fair locking | #38 |
| — | pipelined DML | #39 |
| — | resource control | #40 |
| tikv/client-rust#334 WASM (+PRs tikv/client-rust#536 API-V3 routing, tikv/client-rust#387 split_region) | decide-with-upstream | #41 |

## 2. Finding: five more stale-closable issues than §9.3 lists

Roadmap §9.3 names tikv/client-rust#330, tikv/client-rust#375, tikv/client-rust#373,
tikv/client-rust#370, tikv/client-rust#500, tikv/client-rust#363. The tracker sweep adds:

| issue | shipped by | note |
|---|---|---|
| tikv/client-rust#359 KeySpace | **tikv/client-rust#439** (the implementation; tikv/client-rust#518 / tikv/client-rust#522 later refined no-prefix mode) | API-v2 keyspace is genuine parity (roadmap §1.1) |
| tikv/client-rust#209 CheckTxnStatus over Cleanup | tikv/client-rust#519 | the resolver rework did exactly this |
| tikv/client-rust#451 raw put_with_ttl broken in 0.3 | TTL support since | verify on current, then close |
| tikv/client-rust#283 parallelize multi-region | RetryableMultiRegion | verify the ask is fully covered, then close |
| tikv/client-rust#369 raft entry too large | tikv/client-rust#390 (txn) + tikv/client-rust#501 (raw) | verify-first — same family as tikv/client-rust#500 |

Checked and **NOT** closable — the sweep's own corrections:

- **tikv/client-rust#289 Synchronous API** — only the transactional half shipped
  (tikv/client-rust#517); the raw-client half (tikv/client-rust#301) was closed unmerged
  and no `SyncRawClient` exists. Keep open (or retitle to the raw half). *A close request
  was posted before this correction — a follow-up comment should retract it.*
- **tikv/client-rust#239 rollback-on-drop** — not shipped: `CheckLevel` selects
  panic/warn/none on drop-without-commit; `Transaction::drop` performs no rollback.
  Related to the drop-check in roadmap §1.3, but the asked-for behavior is different.
  Keep open.

(tikv/client-rust#382 message-size may also be closable since tikv/client-rust#520 made
the decode limit configurable — verify whether the *send* side was the complaint.)

## 3. Finding: several planned filings should be revivals, not new issues

Landing our §9.2 filings on existing, ancient issues gives them history, watchers, and
maintainer context. The queue re-maps as:

| roadmap filing (§9.2) | instead of a new issue |
|---|---|
| typed error taxonomy (5.1) | comment on and revive **tikv/client-rust#238** |
| service GC safepoints (5.8) | **tikv/client-rust#180** |
| TiKV/PD version-support policy (§0) | **tikv/client-rust#286** (+tikv/client-rust#447 as evidence) |
| exported test probes (§6) | **tikv/client-rust#284** — the parity harness is itself the strongest exhibit |
| lock-wait control (5.2) | widen **tikv/client-rust#483** (roadmap already says so) |

Genuinely new filings remaining: the async-commit wrong-commit-ts bug (5.3, MUST),
the RE-2 retry misclassification (MUST), the replica/stale-read umbrella (5.4),
raw checksum (5.6 / G-0002), and the MSRV/edition policy question (W4).

## 4. Finding: upstream issues with no roadmap home

- **Worth adding to the Phase-3 long tail**: tikv/client-rust#531 (disable TSO for raw
  clients — raw ops need no txn timestamps; a real efficiency question) and
  tikv/client-rust#327 (more batch operations on the transactional client).
- **Questions the parity documents can now answer**: tikv/client-rust#497 (partial
  mutations on node crash — answered by rules WR-2/WR-3: prewrite-then-commit-primary
  ordering and the undetermined class) and tikv/client-rust#235 (pessimistic lock
  resolution — answered by rules LR-1..4 and roadmap §5.7).
- **Upstream hygiene / out of scope for parity**: tikv/client-rust#425,
  tikv/client-rust#448, tikv/client-rust#285, tikv/client-rust#194, tikv/client-rust#93,
  tikv/client-rust#74, tikv/client-rust#246, tikv/client-rust#17, tikv/client-rust#321,
  tikv/client-rust#374, tikv/client-rust#376 — triage or leave to upstream's own
  housekeeping.

## 5. Keeping this honest

This map is a snapshot of a moving tracker. When acting on it: verify an issue's state
before commenting or closing (several "stale" calls above are verify-first); when a
filing lands, record the issue URL in the corresponding `ledger.toml` entry and backlog
issue; and when the tracker changes shape, re-derive this document rather than patching
it — the derivation (open issues × roadmap items) is cheap, and a stale map is worse
than none.
