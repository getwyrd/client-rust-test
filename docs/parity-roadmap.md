# Bringing client-rust to client-go's feature scope — the closure roadmap

**Date:** 2026-07-14

| | repository | revision under comparison |
|---|---|---|
| SUBJECT | [tikv/client-rust](https://github.com/tikv/client-rust) | `e53837dc6d94f8de4be090571dca0198dc993b9d` (= PR #530, 2026-06-28) |
| ORACLE | [tikv/client-go](https://github.com/tikv/client-go) v2 | `v2.0.8-0.20260708122311-01bd8f99f4da` (kvproto pin 2026-06-22) |

These are `pins.toml`'s pins — this document and the harness deliberately describe the
same world. Citations are `file:line` at those revisions; resolve them as
`https://github.com/tikv/client-rust/blob/e53837dc.../<file>#L<line>` (likewise for
client-go at rev `01bd8f99f4da`). Bare `#NNN` references are tikv/client-rust issues/PRs.

## 0. Method, and what this document is

The parity harness in this repository can now *measure* behavioural gaps between the two
clients (`ledger.toml`, `scenarios/`). This document is the other half: a prioritized plan
for *closing* them — bringing client-rust up to everything client-go has, **dependencies
first, then low-risk/high-impact**. It rests on three full-source sweeps (transaction
layer; rawkv/region/transport/PD/GC; dependencies/protos/observability/config/testability)
plus a review of the upstream tracker, all performed at the pins above.

Ground rules, carried over from the harness:

- **Parity is one-directional: `Rust ⊇ Go`.** client-go is the oracle only. Rust-only
  surface is recorded (§1.3) so it is never mistaken for a gap or re-litigated.
- **Never emulate a missing feature.** A gap is closed upstream in client-rust, or it
  stays an honest `unsupported`.
- **Every closure is machine-verified** (§8): a scenario plus a ledger claim that goes
  loudly red the day the behaviour changes — in either direction.
- **The cluster floor is stated, not assumed.** Upstream CI encodes TiKV v8.5.5
  (client-rust `ci.yml` `TIKV_VERSION`); every Phase 2–4 feature must state its minimum
  TiKV/PD version and how it degrades on older clusters. The version-support policy
  itself is a gap to file upstream (§9) — the oracle has already moved past APIs the
  vendored protos carry (see §5.8).

The normative weight behind this ordering — what a TiKV client MUST, SHOULD, and CAN
do, graded rule-by-rule against the kvproto contract — is codified in
[client-rules.md](client-rules.md); MUST violations outrank everything below.

Upstream context: client-rust has **no roadmap**. #506 ("Roadmap") is a prospective user
*asking* for one — open since 2025-11 with zero replies. The maintainers are demonstrably
active again (a steady merge streak Jan–Jul 2026: #518–#548), so investment lands on
fertile ground, and this document is written so §2 can seed the answer to #506.

## 1. State of play

### 1.1 Already at or near parity — no work needed

- **RawKV** is near-parity: Get/BatchGet/Put/BatchPut/Delete/BatchDelete/DeleteRange,
  Scan/ReverseScan (+key-only variants), CompareAndSwap (`raw/client.rs:684`), TTL
  (`raw/client.rs:300,332`), atomic-CAS mode (`:204`), column families (`:157`). Only
  `RawChecksum` is missing (§5.6), plus the V1TTL api-version mode. Rust even carries a
  raw coprocessor endpoint client-go lacks (`raw/client.rs:707`).
- **Keyspace / API v2** is genuine parity: `request/keyspace.rs` codecs are wired into
  both raw and txn paths (`raw/client.rs:236`, `transaction/transaction.rs:139`).
- **TSO batching** exists (`pd/timestamp.rs:37`, batch ≤ 64) with PD leader failover
  (`pd/cluster.rs:143`).
- **Core txn machinery**: optimistic + pessimistic lifecycle, 2PC with primary retry on
  `commit_ts_expired`, background secondary commit, heartbeat/TTL keep-alive
  (`transaction.rs:947`), scan_locks/resolve_locks/batch-resolve (#519/#524),
  CheckTxnStatus + CheckSecondaryLocks plumbing, UnsafeDestroyRange.

Several open upstream issues are **stale — the feature has since shipped** and they
should be closed or updated (§9): #330 (CAS exists), #375/#373/#370 (TTL exists), and
#500 (`batch_put` has size-split at 16 KB per batch since #501 —
`raw/requests.rs:45,219-221`).

### 1.2 Known behavioural findings, and where they stand

| finding | upstream | state |
|---|---|---|
| Orphaned lock never resolved (harness ledger **G-0001**) | #543, fix PR **#544** | in flight |
| Pessimistic rollback leaves prewrite locks | #545, fix PR **#547** | in flight |
| No contextual logging | #267, fix PR #548 | **merged 2026-07-11** (post-pin) |
| `try_join_all` detaches JoinHandles | #534, fix PR #535 | in flight (not ours) |

### 1.3 Rust-only surface — explicitly NOT gaps

Sync client wrappers (`SyncTransactionClient` et al.), RAII drop-check
(`CheckLevel::{Panic,Warn,None}`), the `HeartbeatOption` knob, explicit read-only
transactions, the raw coprocessor endpoint, and the keyspace `api-v2-no-prefix` mode.
Recorded here once so no future audit counts them as divergence to fix.

## 2. The measured distance

The headline inventory. Everything below appears again inside a phase with evidence.

| area | client-go (oracle) | client-rust (subject) |
|---|---|---|
| transport | BatchCommands streaming + pool of 4 conns/store (`client_batch.go`, `conn_pool.go:60`, default at `config/client.go:193`) | one unary RPC per op over one channel (`store/request.rs:38-45`) |
| routing | replica selector, follower/stale read, forwarding, health/slow-score | leader-only, no health tracking |
| pessimistic locking | full `LockCtx` (17 fields) + fair locking | `wait_timeout` hardcoded, no lock-wait control at all |
| commit protocols | async commit + 1PC + fallbacks + pipelined DML | flags exist; unsafe, no fallbacks (§5.3) |
| error taxonomy | ~26 sentinel errors + ~18 typed structs (`error/error.go`) | one enum; server errors mostly opaque `KeyError` |
| region-error handling | ~22 kinds (`region_request.go:1862-2203`) | ~7 kinds (`request/plan.rs:288-340`) |
| backoff | ~18 per-error tuned configs (`config/retry/config.go`) | 4 generic presets (`backoff.rs:10-13`) |
| GC | service safepoints + GC controller | bare `UpdateGcSafePoint` only |
| metrics | ~92 families (`metrics/metrics.go`) | 9 families (`src/stats.rs:81-135`) |
| config | ~40 knobs incl. TLS CN verify, keepalive, batching | 6 fields (`src/config.rs:22-29`); keepalive hardcoded |
| testability | ~132 probe fns exported from non-test files | mocks are `#[cfg(test)]`-only (`lib.rs:116`) |
| proto vintage | kvproto 2026-06-22 | vendored late-2023/early-2024, unversioned (§4, W2) |

## 3. Phase 0 — land what is already in flight *(days; zero new code)*

1. **Shepherd #544 to merge** (orphaned-lock resolution; a single match-arm widening).
   The harness already proves the fix: applying it flips G-0001 to **XCONVERGE**, which
   is the designed signal to move the pin and re-state the ledger entry as a regression
   guard (`expect = "agrees"`).
2. **Shepherd #547 to merge** (pessimistic rollback clears prewrite locks; #545).
3. **Support #535** (JoinSet instead of `try_join_all`; #534) — review, test, +1.
4. On the next pin move, pick up **#548** (contextual logging) — already merged upstream.

## 4. Phase 1 — dependency & toolchain modernization *(the mandated starting point)*

The current dependency posture, measured against latest stable (crates.io, 2026-07):

| crate | pinned | latest | migration note |
|---|---|---|---|
| tonic | 0.12 | **0.14.6** | codegen split into `tonic-prost-build`; TLS feature rework — the risk hotspot |
| prost | 0.13 | **0.14.4** | lockstep with tonic 0.14; mostly source-compatible |
| tonic-build (proto-build/) | **0.10** | 0.14.6 | two minors behind the tonic 0.12 runtime it generates for (four behind latest) |
| thiserror | 1 | **2.0.x** | attribute changes; touches every error enum, mechanically |
| rand | 0.8 | **≥ 0.9** | `gen`→`random`, `thread_rng`→`rng`; contained |
| prometheus | 0.13 | **0.14** | minor-breaking |
| fail | 0.4 | 0.5.1 | small; upstream itself dormant since 2022 |
| derive-new | 0.5 | 0.7 | trivial |
| tempfile | **`= 3.14.0`** | 3.27 | exact pin whose stated reason is gone (below) |
| dev-deps | clap **2**, serial_test 0.5, reqwest 0.11, env_logger 0.10, rstest 0.18 | clap 4.6, 3.5, 0.13, 0.11, 0.26 | dev-only; clap 2 predates 2019 |

Plus: `lazy_static` (→ `std::sync::LazyLock`), `async-recursion` (→ native, Rust ≥ 1.77),
`take_mut` (unmaintained; → `std::mem::replace` patterns). Edition **2021** with MSRV
**1.93.0** — an unnecessarily high MSRV *for a library*, which blocks downstream
consumers and has already forced allow-workarounds in the proto wrapper module
(`src/proto.rs:8-13`).

Five PR-sized workstreams. W1 and W2 are deliberately **independent**: the kvproto
refresh runs on the *existing* tonic-build 0.10 pipeline (`make generate` → commit →
drift gate), so the low-risk, high-value proto work is not gated behind the gRPC
migration — and neither workstream gates Phase 2.

**W1 — kvproto refresh + provenance marker** · risk: low · one PR
The vendored protos are late-2023/early-2024 vintage (`proto/errorpb.proto` and
`proto/pdpb.proto` last touched 2023-12-28) with one hand-patched 2026 addition (#519),
and **no record of which kvproto revision they came from**. Re-vendor at client-go's
kvproto pin (2026-06-22), regenerate, and **add a `proto/VERSION` marker** so vintage is
never again guesswork. Measured honestly: the staleness is load-bearing for exactly two
things — pipelined DML (`FlushRequest`/`FlushResponse`/`BufferBatchGetRequest`) and
`HealthFeedback`; plus pdpb's missing `BatchScanRegions` and the GC-controller API the
oracle now uses (§5.8). Everything else this roadmap needs (`wake_up_mode`, replica/
stale read, `wait_timeout`, `assertion_level`, `max_commit_ts`,
`resource_control_context` — even the `BatchCommands` RPC, `tikvpb.proto:91`) is
**already in the vendored protos**, which is why Phase 2 does not wait for this PR. The
diff is large but mechanical; integration tests cover the wire.
*Unblocks: pipelined DML, health-aware routing, GC-controller parity (Phase 3/4);
nothing in Phase 2.*

**W2 — gRPC stack alignment** · risk: medium · one PR
`tonic 0.12→0.14` + `prost 0.13→0.14` + `tonic-build 0.10→tonic-prost-build 0.14`
(proto-build/). This aligns a codegen/runtime version skew — generated code produced by
tonic-build 0.10/prost ~0.12, consumed by a tonic 0.12/prost 0.13 runtime — which is
bookkeeping rather than breakage (the tree compiles under `-Dwarnings`; the drift gate
is green), so nothing else waits on it. The TLS feature rework in 0.14 is the risk
concentration — mitigated by the fact that #541 just rebuilt the TLS stack
(2026-06-22), so that code is fresh and its tests current. client-rust's own
generated-code drift gate (its `.github/workflows/ci.yml:30-32`) catches codegen
surprises.
*Unblocks: a modern, supported gRPC stack.*

**W3 — contained breaking bumps** · risk: low · one or two PRs
`thiserror 2`, `rand 0.9+`, `prometheus 0.14`, `fail 0.5`, `derive-new 0.7`; drop the
`tempfile = 3.14.0` exact pin — #532 pinned it *for rust-toolchain 1.84.1*, and #530 then
moved the toolchain to 1.93, so the pin's justification no longer exists. Refresh
dev-deps (clap 4, serial_test 3, reqwest 0.13, env_logger 0.11, rstest 0.26).

**W4 — edition 2024 + an MSRV policy** · risk: low, but needs a maintainer decision
Migrate edition 2021→2024, and **decouple MSRV from the dev toolchain**: keep
`rust-toolchain.toml` current for development, set `rust-version` to something a
library's consumers can meet, and **land the MSRV CI job first** so the floor is
tested rather than asserted. The source itself needs nothing newer than ~1.82
(#530's only feature use is `iter::repeat_n`) and the edition-2024 minimum is 1.85 —
but the floor is set by W2's dependency set, and that number is already known:
`tonic` 0.14.6 and `tonic-prost-build` 0.14.6 both declare `rust-version = 1.88`
(prost 0.14.4: 1.85). So the working target is **1.88** once W2 lands — five stable
releases below the current 1.93 pin and a reasonable library MSRV — with the CI job
reporting the exact floor as dependencies move. Replace `lazy_static` with
`LazyLock`, retire `async-recursion` and `take_mut` in the same pass.

**W5 — CI & release hygiene** · risk: nil
`arduino/setup-protoc@v1`→v3; the MSRV job from W4; a CHANGELOG; a tagged-release
workflow. The published crate (0.4.0, 2026-02-07) shipped every stale pin in the table
above — release cadence is part of the parity story, because users compare *releases*,
not branches. Propose 0.5.0 at the end of Phase 1.

## 5. Phase 2 — low-risk, high-impact plumbing

Everything here uses proto fields **already present** in the vendored protos, and —
with one flagged exception (5.4's selector core) — changes no architecture. Ordered by
impact-per-risk. Each item names its verification; note that every scenario below first
needs its driver commands added to the parity protocol (§8, prerequisite 0).

One policy applies across the phase: several items grow the public API, and 5.1 begins
with a breaking change — so the work lands under the 0.5.0 breaking-change budget W5
already proposes, with deprecations (not removals) for superseded surface.

**5.1 Typed error taxonomy** — the single highest-leverage usability fix.
Gap: client-go exposes ~26 sentinel errors and ~18 typed structs (`error/error.go` —
`ErrWriteConflict:167`, `ErrDeadlock:128`, `ErrKeyExist:147`, `ErrTxnTooLarge:217`,
`ErrAssertionFailed:296`, …); client-rust collapses nearly all server per-key errors
into an opaque `Error::KeyError` (`common/errors.rs:86`), so callers cannot detect a
deadlock to retry it or a write conflict to surface it — see the confusion in #486/#487.
Work: mapping in `errors.rs` over kvproto payloads the client already receives — but it
**begins with a breaking change**: `Error` is not `#[non_exhaustive]`
(`common/errors.rs:26-28`), so adding variants breaks every downstream exhaustive
`match`. Step one is marking it `#[non_exhaustive]` (the crate already applies exactly
this pattern to `Config`, with a comment explaining why), then the taxonomy lands under
the 0.5.0 budget.
Verify: `scenarios/write-conflict-error-shape.json`, `scenarios/deadlock-error-shape.json`
(`expect = "agrees"` on `class` only — the native taxonomy is recorded in every trace
regardless, no opt-in needed; opting `native.type` into an agrees-claim would fail
forever, since Go's `%T` strings and Rust's variant names never textually match).

**5.2 The `LockCtx` surface for pessimistic locks.**
Gap: the request's `wait_timeout` is hardcoded (`transaction/requests.rs:413`) — there is
no no-wait, no bounded wait, no `CheckExistence`, no `LockOnlyIfExists`, no
`ReturnValues` outside `get_for_update`, no killed/interrupt flag, and `is_first_lock` is
always `false` (`requests.rs:412`); client-go carries all of it in `LockCtx`
(`kv/kv.go:61`) and `pessimistic.go:174-213`. Related ask: #483.
Work: a `LockOptions` builder on `lock_keys`; plumb existing proto fields.
Verify: `scenarios/lock-wait-nowait.json`, `scenarios/lock-wait-timeout.json` — today
behavioural divergences (oracle times out / errors fast; subject blocks), flipping to
`expect = "agrees"` as they land.

**5.3 Async-commit safety fix** — bugfix-grade; do even before the full feature.
Gap: with `use_async_commit` set, a `min_commit_ts == 0` prewrite response (TiKV's
fallback signal) does **not** panic at the FIXME'd `unwrap()`
(`transaction.rs:1289-1290`) — it flows through `.max().map(Timestamp::from_version)`
(`transaction.rs:1364-1371`, no zero-filter) and the transaction **reports success at a
garbage zero commit_ts** while the background secondary commit fails as a swallowed
`log::warn` (`:1303-1306`). With mixed responses, `max()` silently discards the
fallback signal from the region that demanded it. A silent-consistency bug, strictly
worse than the crash the FIXME implies — and it breaks the `commit_ts > start_ts`
invariant (rule TS-2). client-go degrades to 2PC instead (`prewrite.go:616`). 1PC
likewise returns `Error::OnePcFailure` (`transaction.rs:1356`) instead of falling back.
Work: the fallback is genuinely small — the `else` branch at `transaction.rs:1291-1302`
*is* the 2PC path, so "on `min_commit_ts == 0`, take it" is a few lines, same for 1PC.
Setting `max_commit_ts` correctly (`transaction.rs:1340` FIXME; oracle computes it at
`2pc.go:2143`) needs the Phase-3 commit-ts machinery and moves there — a naive value is
exactly the unsafety being fixed.
Verify: the wrong-commit-ts outcome is *observable*, so this gets a differential
scenario now — `scenarios/async-commit-fallback.json`, `expect = "diverges"` today
(oracle degrades cleanly; subject mis-commits), re-stated as the `expect = "agrees"`
regression guard once the fix lands.

**5.4 Basic replica read + stale read + isolation level** — the phase's one exception
to "no architectural change"; risk: medium.
Gap: none of `Context.replica_read`, `stale_read`, or `isolation_level` is ever set;
snapshots have no `SetReplicaRead`/`SetIsStalenessReadOnly`/`SetIsolationLevel`
equivalents (`txnsnapshot/snapshot.go:982,989,1053`). This blocks read scale-out
entirely.
Work: the Context flags are a plumb, but the feature's *dominant expected error* is
`DataIsNotReady` — which today falls into the generic invalidate-and-retry arm
(`request/plan.rs:335-339`), silently downgrading the read to the leader or spinning on
the same lagging follower. So 5.4's blocking prerequisite is a **replica-selector
core**: a `data_is_not_ready` arm plus next-replica-and-leader-fallback selection in
the retry path. That selector work is real routing machinery; health/slow-score and
load-aware routing still stay in Phase 4, which extends this core.
Verify: needs a **3-store cluster profile** in `cluster/` (harness prerequisite, §8);
`scenarios/follower-read-basic.json`, `scenarios/stale-read-basic.json`.

**5.5 Snapshot/read knobs.** `scan batch size`, `not_fill_cache`, `priority`, KV read
timeout (`max_execution_duration_ms`, already in the vendored Context —
`busy_threshold_ms` is a different knob: load-based `ServerIsBusy` rejection, which
belongs to Phase 4's routing) — each a one-field plumb (`snapshot.go:967,977,999,1248`
for the oracle's shape). Verify: capability claims.

**5.6 Raw `Checksum`.** The one real RawKV gap (`rawkv/rawkv.go:615`; `CmdRawChecksum`
absent from `store/request.rs:75-86`). Single request type.
Verify: **landed** — `scenarios/raw-checksum.json` + ledger **G-0002**, the canonical
capability claim (`expect = "diverges"`, `class: oracle=ok / subject=unsupported`,
declared field-complete with its `unsupported.detail` row). Flips to XCONVERGE the day
client-rust gains the request.

**5.7 Read-path lock resolution parity.** Re-enable resolve-lock-lite (`requests.rs:229`
pins `txn_size = MAX` with a "currently disabled" comment; oracle threshold at
`lock_resolver.go:68`) and resolve async-commit locks on the *read* path — today
`lock.rs:117` just parks them in `live_locks`, so reads stall behind healthy
async-commit transactions until TTL; the machinery (CheckSecondaryLocks,
`lock.rs:336-380`) already exists but is only reachable from the GC path.
Verify: `scenarios/async-commit-lock-read.json` (oracle resolves and reads; subject
blocks) — a G-claim in the G-0001 mould.

**5.8 Service GC safepoints** — correctness on shared clusters.
Gap: client-rust can only move the *cluster* GC safepoint (`UpdateGcSafePoint`,
`pd/cluster.rs:88`); it exposes no way to register a *service* safepoint, so a
long-lived client-rust reader on a shared cluster cannot protect its snapshot from GC.
Stated carefully, because the oracle has moved here: at the pin, client-go's own GC
drives the newer keyspace-scoped **GC controller** (`tikv/gc.go:71` →
`GetGCInternalController(...).AdvanceTxnSafePoint(...)`), an API **absent from our
vendored pdpb** (needs W1) — while `UpdateServiceGCSafePoint` remains on the PD client
as an *explicitly caller-invoked* capability (and is already in the vendored
`pdpb.proto:80`). Neither client auto-registers snapshots; the gap is that client-rust
cannot register one at all. Phase 2 scope: expose service-safepoint registration
(proto-ready today); GC-controller parity is Phase 3/W1 territory.
Verify: PD-observed check (needs the small runner extension noted in §8).

**5.9 Known-bug cluster.** #512 (reverse scan misses keys across region boundaries — a
correctness bug in shipped surface), triage #528. (#500 turned out to be already fixed
at the pin — `batch_put` size-splits at 16 KB since #501, `raw/requests.rs:45,219-221`;
it moved to §9's close-as-stale list.)
Verify: `scenarios/reverse-scan-region-boundary.json` (`expect = "diverges"` today).

## 6. Phase 3 — medium machinery

- **Complete async commit + 1PC** (#287, open since ~2021): fallback paths from 5.3 plus
  a `minCommitTsManager` (`2pc.go:1202`), commit-ts calculation, and the
  causal-consistency toggle (`txn.go:507`; Rust is always-linearizable today, i.e. one
  extra TSO round-trip per commit that Go can skip).
- **TTL-manager parity** (#312): keep-alive that also advances `min_commit_ts`
  (`2pc.go:1299`), a max-lifetime cap, and heartbeat start timing.
- **Stale-read machinery**: `GetMinTS` (a PD-client call) and `StoreSafeTS` (a tikvrpc
  request) plumbing, plus `ValidateReadTS` and the low-resolution/stale-timestamp
  helpers on the Oracle interface proper (`oracle/oracle.go:52-82`) — the difference
  between 5.4's basic flag and staleness you can trust.
- **Txn-option long tail** (each small; wire fields already vendored where applicable):
  assertion level (`txn.go:565`), per-request priority, disk-full opt
  (`txn.go:529`), commit callback (`txn.go:448`), request-source tagging
  (`util/request_source.go:74`), an RPC-interceptor hook (`txn.go:431`).
- **Backoff & region-error taxonomy**: ~18 tuned configs vs 4 presets
  (`config/retry/config.go:122-140` vs `backoff.rs:10-13`); ~22 region-error kinds vs ~7
  (`region_request.go:1862-2203` vs `request/plan.rs:288-340`), including disk-full and
  data-not-ready handling — and the rule-RE-2 misclassification (`ServerIsBusy`/
  `MaxTimestampNotSynced` surfaced to callers instead of backed off and retried; see
  [client-rules.md](client-rules.md), a MUST violation). Upstream asks: #310, #336, #337.
- **Region cache**: background reload/TTL (`region_cache.go:828,1809`), batched region
  loading (`ScanRegions`; `BatchScanRegions` needs W1), bounded-staleness invalidation.
  This — not the passive lookup cache that exists today (`region_cache.rs`, its own
  `// TODO: does it need TTL?`) — is the substance of upstream #299.
- **Config & TLS**: grow `Config` from 6 fields toward the oracle's ~40 (keepalive —
  today hardcoded at `common/security.rs:141-146` — window sizes, batch/async-commit
  knobs, region-cache TTL, store limits); a TLS CN allow-list knob (`ClusterVerifyCN`,
  declared at `config/security.go:50` — enforcement sits with the TLS-config consumer)
  and per-handshake cert reload via the `GetClientCertificate` callbacks
  (`config/security.go:97-102`; Rust reloads on reconnect only, `security.rs:99-134`,
  and has no CN check).
- **Observability**: from 9 metric families (`stats.rs:81-135`) toward the oracle's ~92
  (`metrics/metrics.go`) — backoff, region-cache, lock-resolver, txn-duration families
  first, batch-client families with Phase 4; adopt `tracing` spans (client-rust has
  zero tracing today; 97 bare `log` statements).
- **MemDB parity**: staging/nested writes (#489), memory limits (#311), Len/Size
  introspection (`txn.go:1938,1943`).
- **Exported test probes** — the force multiplier. client-go ships ~132 probe functions
  from non-test files (`txnkv/transaction/test_probe.go` et al.); client-rust's mocks
  are `#[cfg(test)]`-gated (`lib.rs:116`), which is *why* all deterministic state
  manufacture in this harness is Go-only. A feature-gated `test-utils` surface
  (prewrite-only, committer controls) makes every future scenario cheaper and should be
  filed upstream as a finding in its own right.

## 7. Phase 4 — architectural projects *(each warrants its own design doc)*

- **BatchCommands transport + per-store connection pool** (#442; supersede stale PR
  #363): the biggest scalability gap — one unary RPC per op today vs the oracle's
  4-conn pool and batch send/recv loops with ~20 dedicated metrics. The `BatchCommands`
  RPC is already in the vendored protos (`tikvpb.proto:91`); the work is entirely
  client-side (send/recv tasks, wait/overload policy, per-store state).
- **Store health, slow-score & the full replica selector**: liveness probing
  (`store_cache.go:607-693`), slow-score (`slow_score.go`), `HealthFeedback` (needs W1),
  forwarding/proxy routing, load-based replica selection — extends the selector core
  5.4 introduces into production-grade routing.
- **Fair locking** (#546, `WakeUpModeForceLock`): proto-ready already; the client-side
  aggressive-locking state machine (`txn.go:1124-1253`) is the work. Builds on 5.2.
- **Pipelined DML** (needs W1): `Flush`/`BufferBatchGet` machinery
  (`pipelined_flush.go`), membuf tiers, throttling.
- **Resource control**: RU accounting/throttling interceptor
  (`client_interceptor.go:27-248`) — honestly a *project, not a gap*: parity means
  porting PD's resource-manager client and a cost model.
- **Decide-with-upstream scope**: coprocessor/MPP/flashback/MVCC-introspection/
  lock-observer GC/SplitRegion-and-scatter (`tikvrpc.go` Cmd list), plus the TiDB-domain
  txn hooks (binlog, schema amender/lease, KV filter) — plausibly TiDB-domain rather
  than general-client surface. File the scope question; do not silently adopt or reject.

## 8. Verification — how every closure is proven

The mechanism exists and has fired in anger (G-0001). Per item:

- **Behavioural gap**: a scenario under `scenarios/` + a `[[gap]]` entry in
  `ledger.toml` with `expect = "diverges"` and the divergence declared field-by-field.
  While open it must reproduce (**XDIVERGE**); the day upstream fixes it,
  **XCONVERGE** hard-fails the gate, forcing the pin forward and the entry re-stated as
  `expect = "agrees"` — a permanent regression guard. Divergence *in an undeclared way*
  is **WRONG DIVERGENCE** (evidence of nothing).
- **Capability gap** (feature absent, not misbehaving): same schema — the subject's
  driver answers `class: unsupported` (a first-class, comparable observation; drivers
  never emulate), declared `oracle=ok / subject=unsupported` — and, like every claim,
  **field-complete**: an `unsupported` observation also projects `unsupported.detail`
  (and the ok side its `value`), and an undeclared row is a WRONG DIVERGENCE. G-0001's
  five declared rows are the discipline to copy. Optionally tag these
  `kind = "capability"` for reporting; the verdict rules need no change.
- **Bugs whose failure is silent** (5.3's wrong-commit-ts is the type specimen): still
  differential evidence — the trace records what each client *reported*, so a subject
  that claims success where the oracle degrades is an observable, declarable
  divergence. Only a *dead* driver (`driver_error`) is inadmissible.

Harness prerequisites this roadmap imposes (themselves pin-stated):

0. **the driver protocol grows with every item**: the parity `Command` enum
   (`crates/parity-proto/src/command.rs`) started with exactly the 12 commands the
   first scenario needed. Each item's verification starts by adding its command(s) to
   `parity-proto` **and both drivers**; a scenario naming an unknown op fails at load.
   This is deliberate (drivers stay minimal) but it means "write the scenario" is
   never the whole cost. First slice landed with G-0002: `open_raw_client`,
   `raw_put`, `raw_checksum`;
1. a **3-store cluster profile** in `cluster/` for replica/stale-read scenarios (5.4)
   — **landed** (`cluster/docker-compose-3store.yml`, `make cluster-up-3store`,
   3 replicas per region); the scenarios that use it are 5.4's;
2. a **PD-observation step** for the runner (safepoint checks, 5.8) — consistent with
   the existing rule that PD preconditions belong to the runner, never to drivers;
3. once upstream probes exist (§6), a Rust orphan-factory so interesting scenarios stop
   being Go-setup-only.

## 9. Upstream engagement

1. **Answer #506** with §2 and the phase structure — it has waited eight months.
2. **File what is missing** — every MUST violation first: the async-commit
   wrong-commit-ts bug (5.3); the RE-2 retry misclassification (`ServerIsBusy`/
   `MaxTimestampNotSynced` surfaced to callers instead of backed off and retried —
   §6, [client-rules.md](client-rules.md) RE-2); service GC safepoints (5.8). Then:
   typed error taxonomy (5.1); lock-wait control (5.2, or widen #483); replica/stale
   read umbrella (5.4); a TiKV/PD version-support policy (§0); the MSRV/edition
   policy question (W4); the no-importable-probes finding (§6).
3. **Close or update the stale**: #330, #375, #373, #370 (shipped: CAS/TTL), #500
   (fixed by #501; issue left open), #363 (superseded by the Phase-4 batch design).
   Leave #299 open — the passive cache exists, but the reload/TTL substance of the
   issue is Phase-3 work (§6).
4. **Keep the ledger and this document in lockstep**: every filed issue gets a G-entry
   the moment its scenario exists; every merged fix moves the pin. The two artifacts
   describe one world, at one revision, on purpose.
