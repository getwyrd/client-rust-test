# Upstream issue map — tikv/client-rust's tracker against the roadmap

**Date:** 2026-07-14 · sources: the 72 open issues on tikv/client-rust as of this date,
[parity-roadmap.md](parity-roadmap.md), and the backlog milestones in this repo
(getwyrd/client-rust-test issues #7–#41, one milestone per roadmap phase).

Unlike the roadmap's `file:line` citations, an issue tracker is not pinnable — this map
is a dated snapshot. Re-derive it when the tracker moves; the backlog issue bodies carry
the same upstream references and are the living copy.

## 1. The mapping

Bare `#NNN` below is tikv/client-rust; `ours #NN` is this repo's backlog.

### Phase 0 — in flight / engagement

| upstream | roadmap | ours |
|---|---|---|
| #543 orphaned lock (fix PR #544 in flight) | §3.1 | #7 |
| #545 pessimistic rollback (fix PR #547 in flight) | §3.2 | #8 |
| #534 detached JoinHandles (PR #535, not ours) | §3.3 | #9 |
| #267 logging — fixed by #548, issue still open | §3.4 (pin move closes it) | #10 |
| #506 the roadmap ask; the stale set (§2 below) | §9.1 / §9.3 | #11 |

### Phase 1 — dependencies

No upstream counterparts: the dependency modernization is this initiative's own
contribution. Upstream-hygiene issues that sit *near* W5 (their repo's CI/test
housekeeping, not ours to own): #448 (API-v1 integration tests), #425 (cargo make),
#285 (unit-test coverage), #93 (oracle unit tests), #74 (nightly proptest CI).

### Phase 2

| upstream | roadmap | ours |
|---|---|---|
| **#238 "improve error handling"** (open since 0.2!); #486/#487/#482 as user-confusion evidence | §5.1 typed errors | #17 |
| #483 non-exclusive get_for_update; #308 pessimistic-lock caching (adjacent) | §5.2 LockCtx | #18 |
| — (to file; #497 is adjacent as a 2PC-atomicity question) | §5.3 async-commit safety | #19 |
| — (to file) | §5.4 replica/stale read | #20 |
| — | §5.5 snapshot knobs | #21 |
| — (to file; gap already machine-checked as G-0002) | §5.6 raw checksum | #22 |
| **#111** resolve specific keys; **#235** why no pessimistic-lock resolution; **#208** user control over resolution; #315 (triage) | §5.7 read-path resolution | #23 |
| **#180 "Support GC"** (2019) | §5.8 service safepoints | #24 |
| #512 reverse scan; #528 (triage); old raw bugs #380/#331/#377/#488 as triage-adjacent | §5.9 known bugs | #25 |

### Phase 3

| upstream | roadmap | ours |
|---|---|---|
| #287 async commit + 1PC; **#189 large transactions** | complete async commit | #26 |
| #312 heartbeat timing (+#189 overlap) | TTL manager | #27 |
| (PR #540 TSO-v2 discovery is adjacent) | stale-read machinery | #28 |
| — | txn-option long tail | #29 |
| #310, #336, #337, #405 (backon); **#498** — a differential bug report ("works in Go, fails in Rust": Store Not Match / Not Leader) | backoff & region-error taxonomy | #30 |
| #299 (+PR #445 stale-cache fix) | region cache | #31 |
| #472 TLS; #382 msg size (+PR #414 custom DNS) | config & TLS | #32 |
| (PRs #470 optional-prometheus, #427 scan tracing) | observability | #33 |
| #489 MemDB staging; #311 memory limits | MemDB parity | #34 |
| **#284 fault injection / nemesis**; #389, #516, #525 (failpoint flakiness) | exported test probes | #35 |

### Phase 4

| upstream | roadmap | ours |
|---|---|---|
| #442 BatchCommands; #475 more channels per store; #288 benchmarking (acceptance criteria); stale PR #363 (superseded) | transport + conn pool | #36 |
| #493 offline-store connection refused (+#498 overlap) | store health & selector | #37 |
| #546 fair locking | fair locking | #38 |
| — | pipelined DML | #39 |
| — | resource control | #40 |
| #334 WASM (+PRs #536 API-V3 routing, #387 split_region) | decide-with-upstream | #41 |

## 2. Finding: five more stale-closable issues than §9.3 lists

Roadmap §9.3 names #330, #375, #373, #370, #500, #363. The tracker sweep adds:

| issue | shipped by | note |
|---|---|---|
| #359 KeySpace | **#439** (the implementation; #518/#522 later refined no-prefix mode) | API-v2 keyspace is genuine parity (roadmap §1.1) |
| #209 CheckTxnStatus over Cleanup | #519 | the resolver rework did exactly this |
| #451 raw put_with_ttl broken in 0.3 | TTL support since | verify on current, then close |
| #283 parallelize multi-region | RetryableMultiRegion | verify the ask is fully covered, then close |
| #369 raft entry too large | #390 (txn) + #501 (raw) | verify-first — same family as #500 |

Checked and **NOT** closable — the sweep's own corrections:

- **#289 Synchronous API** — only the transactional half shipped (#517); the raw-client
  half (#301) was closed unmerged and no `SyncRawClient` exists. Keep open (or retitle
  to the raw half). *A close request was posted before this correction — a follow-up
  comment should retract it.*
- **#239 rollback-on-drop** — not shipped: `CheckLevel` selects panic/warn/none on
  drop-without-commit; `Transaction::drop` performs no rollback. Related to the
  drop-check in roadmap §1.3, but the asked-for behavior is different. Keep open.

(#382 message-size may also be closable since #520 made the decode limit configurable —
verify whether the *send* side was the complaint.)

## 3. Finding: several planned filings should be revivals, not new issues

Landing our §9.2 filings on existing, ancient issues gives them history, watchers, and
maintainer context. The queue re-maps as:

| roadmap filing (§9.2) | instead of a new issue |
|---|---|
| typed error taxonomy (5.1) | comment on and revive **#238** |
| service GC safepoints (5.8) | **#180** |
| TiKV/PD version-support policy (§0) | **#286** (+#447 as evidence) |
| exported test probes (§6) | **#284** — the parity harness is itself the strongest exhibit |
| lock-wait control (5.2) | widen **#483** (roadmap already says so) |

Genuinely new filings remaining: the async-commit wrong-commit-ts bug (5.3, MUST),
the RE-2 retry misclassification (MUST), the replica/stale-read umbrella (5.4),
raw checksum (5.6 / G-0002), and the MSRV/edition policy question (W4).

## 4. Finding: upstream issues with no roadmap home

- **Worth adding to the Phase-3 long tail**: #531 (disable TSO for raw clients — raw
  ops need no txn timestamps; a real efficiency question) and #327 (more batch
  operations on the transactional client).
- **Questions the parity documents can now answer**: #497 (partial mutations on node
  crash — answered by rules WR-2/WR-3: prewrite-then-commit-primary ordering and the
  undetermined class) and #235 (pessimistic lock resolution — answered by rules LR-1..4
  and roadmap §5.7).
- **Upstream hygiene / out of scope for parity**: #425, #448, #285, #194, #93, #74,
  #246, #17, #321, #374, #376 — triage or leave to upstream's own housekeeping.

## 5. Keeping this honest

This map is a snapshot of a moving tracker. When acting on it: verify an issue's state
before commenting or closing (several "stale" calls above are verify-first); when a
filing lands, record the issue URL in the corresponding `ledger.toml` entry and backlog
issue; and when the tracker changes shape, re-derive this document rather than patching
it — the derivation (open issues × roadmap items) is cheap, and a stale map is worse
than none.
