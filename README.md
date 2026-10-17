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

The gate lives in the `wyrd-gate` crate — one suite in this workspace, and the
template for the client-parity suites to come:

- [`crates/wyrd-gate/src/traits.rs`](crates/wyrd-gate/src/traits.rs) — the
  `MetadataStore` contract, **vendored verbatim** from wyrd
  (`crates/traits/src/lib.rs` @ 7009c2e). Copied, not depended on, so the harness
  stays standalone; M4's premise is that this surface is frozen, and any needed
  re-copy would itself be a gate finding.
- [`crates/wyrd-gate/src/lib.rs`](crates/wyrd-gate/src/lib.rs) —
  `TikvMetadataStore`, the exact `WriteBatch → one TiKV transaction` translation
  the future `metadata-tikv` crate will use: pessimistic `get_for_update` by
  default, optimistic + `lock_keys` as the measured alternative, write-conflict →
  `Ok(Conflict)` classification, paged native prefix scan, rollback discipline.
- [`crates/wyrd-gate/tests/gate.rs`](crates/wyrd-gate/tests/gate.rs) — the gate
  proper; every test names the proposal obligation it verifies.
- [`crates/harness`](crates/harness) — client-agnostic fixtures: `$PD_ADDRS`, and
  PD's region layout as ground truth, so the gate's cross-region obligations are
  *asserted* rather than assumed. Nothing here knows what a `MetadataStore` is,
  which is what lets a future differential runner share it.

## The parity harness

The gate can only ever say **"client-rust fails."** It can never say **"and client-go
succeeds"** — which is the claim that makes a gap a *gap*, and the claim every upstream
issue needs. So the harness also drives **client-go as an oracle**, through the same
scenarios, and diffs what the two clients observed.

```
scenarios/*.json          declarative; no conditionals, no loops
        │
        ▼
crates/parity-runner  ──NDJSON──▶  crates/rust-driver  → tikv-client   (the SUBJECT)
  owns the total order──NDJSON──▶  go/driver           → client-go     (the ORACLE)
        │
        ▼
results/traces/*.json ──▶ projection ──▶ diff ──▶ ledger.toml ──▶ XDIVERGE / XCONVERGE
```

Both clients wrap the **same protobuf** — `tikv-client`'s `Error::KeyError` and
client-go's `ErrWriteConflict` are thin wrappers over the same `kvrpcpb` messages — so
the vocabulary the two are compared in is not invented, it is **the server's**. Each
observation keeps three layers: the canonical `class` (always compared), the `proto`
(presence compared), and the client's *own* error taxonomy in `native` (never compared
by default, always kept as evidence). Nothing is normalized at capture; the projection
decides what a given claim looks at. **Canonicalize the question, never the evidence** —
a projection can be widened later, but evidence discarded at capture is gone.

That layering is not decoration. `G-0001`'s root cause *was* a native-taxonomy bug —
`check_txn_status` matched `Error::ExtractedErrors` but only ever received
`Error::MultipleKeyErrors` (until tikv/client-rust#561) — and both map to the same canonical class. A class-only diff
would call the two clients identical and the bug would be invisible.

`ledger.toml` declares each gap **field by field**, and `scripts/ledger-check.sh`
generalizes `gate-verdict.sh` from *"a substring in stdout"* to *"a field in a trace"*:

| declared | observed | verdict |
|---|---|---|
| `diverges` | diverged as declared | **XDIVERGE** — the gap is still open ✅ |
| `diverges` | **agreed** | **XCONVERGE** — hard fail: the gap closed; pin/README/ledger are stale |
| `diverges` | diverged *differently* | **WRONG DIVERGENCE** — hard fail: evidence of nothing |
| `agrees` | diverged | **NEW GAP** — hard fail: file it, or fix it |

A run is refused outright unless its provenance is admissible: `PARITY_STRICT`, the crate
under test on-pin and clean, and — reported by the Go driver from its *own*
`debug.ReadBuildInfo()` — client-go at the pinned version and **not `replace`d**. This is
where `pins.toml`'s *"an oracle you can accidentally edit is not an oracle"* stops being
a comment.

```sh
make parity    # run every scenario against both clients, report the diff
make ledger    # ...and check the diff against its declared expectation
```

## Layout

A Cargo workspace, developed against a sibling checkout of the crate under test (a
path dependency, so local work on client-rust is what the gate exercises):

```
wyrd/
├── client-rust/        # tikv/client-rust — the SUBJECT under test
└── client-rust-test/   # this harness
    ├── crates/
    │   ├── harness/        # client-agnostic: $PD_ADDRS, PD region ground truth
    │   ├── wyrd-gate/      # the M4 evaluation gate (one suite among those to come)
    │   ├── parity-proto/   # the protocol + observation schema. NO client deps.
    │   ├── rust-driver/    # drives tikv-client  (the subject)
    │   └── parity-runner/  # the comparator. Links NEITHER client, on purpose.
    ├── go/                 # drives client-go (the ORACLE), at the pinned module
    ├── scenarios/          # one scenario per claim, two clients, one diff
    ├── cluster/            # throwaway PD + TiKV profiles (digest-pinned), leased by scripts/cluster-broker
    ├── scripts/            # pins, provenance, the verdicts
    ├── findings/           # the harness's output: write-ups, repros, evidence
    ├── docs/               # the roadmap, the client rules, the upstream issue map
    ├── ledger.toml         # every gap, with a declared expectation
    └── pins.toml           # what is under test — the single source of truth
```

`parity-runner` links neither client, and that is enforced in its `Cargo.toml`: the thing
that decides a verdict must not be able to reach for the crate it is adjudicating.

Toolchain is pinned to 1.93.0, matching client-rust.

**Which revision is under test?** The dependency is a path dep, and Cargo records
no source or rev for one — so `Cargo.lock` pins *nothing*. `pins.toml` names the
revision, and every run stamps what was actually exercised
(`results/provenance.json`). `PARITY_STRICT=1` (which CI sets) refuses to run at all
when the two disagree, so a result can never quietly be about a different tree than
it claims.

**Which client-rust?** The fork's `integration` branch
([ADR-0001](docs/design/adr/0001-pin-follows-the-integration-branch.md)): upstream
tikv/client-rust plus the getwyrd PRs still waiting upstream. `make pins-check` asserts
the pin is a commit of that branch and that its `upstream_base` is on upstream
`master`, so every gap is still stated and filed against tikv/client-rust. CI also runs
the parity diff at `upstream_base` — informational, never gating — to show which gaps
are still open upstream.

## Usage

```sh
make gate           # one-shot: cluster up, wait ready, run everything
make cluster-down   # tear down the cluster and its data

# individually:
make cluster-up     # PD + TiKV v8.5.5 (client-rust's CI pin) on the classic ports, via the broker
make unit-test      # cluster-free tests (prefix math, error classification)
make verdict        # the gate, checked against the verdict EXPECTED at the pin
make gate-test      # the cluster-backed gate, raw ($PD_ADDRS, default 127.0.0.1:2379)
make failpoint-test # the failpoint proof (opt-in `failpoints` feature)
make pins-check     # pins.toml agrees with everything; the pin is on integration, on upstream
make check          # fmt --check, clippy -D warnings, cargo check, broker tests
```

`make verdict` is what CI runs, and it is the one to trust. Plain pass/fail is
meaningless here: the harness carries no workarounds for client bugs, so a
deficiency is a **failing test**, and the verdict declares which tests are expected red
at the pinned revision. It requires each expected-red test to fail **on its own
assertion**, and fails loudly if one ever passes — that is how you find out the day a gap
is fixed at the pin (upstream, or in a fork PR on `integration`). At the current pin the expected-red list is empty: the two findings that
kept `d6` and `d7` red are fixed upstream, and both tests now guard against a regression.

The cluster ([`cluster/`](cluster/)) is a throwaway single-node PD + TiKV with
client-rust CI's aggressive region-split thresholds, so multi-key transactions
genuinely span Raft regions — the property M4 depends on.

### More than one cluster: the broker

[`scripts/cluster-broker`](scripts/cluster-broker) owns every cluster's lifecycle, so
several runs can share one host. A lease is a whole cluster on its own block of ports
with its own data volumes; the only thing a test needs from it is `$PD_ADDRS`.

```sh
./scripts/cluster-broker lease single --ttl 2h          # {"lease_id": …, "pd_addrs": "127.0.0.1:32000", …}
./scripts/cluster-broker lease 3store --tikv v8.5.8     # any version pinned in pins.toml [cluster.versions]
PD_ADDRS=127.0.0.1:32000 make verdict                   # gates never touch Docker
./scripts/cluster-broker list                           # leases, slots, expiry
./scripts/cluster-broker release <id>                   # compose down -v + forget
./scripts/cluster-broker gc                             # release expired leases; report orphans
```

`make cluster-up` is the lease `manual-single` on slot 0 (the classic ports, no expiry),
so CI and `$PD_ADDRS`'s default are unchanged; broker-allocated leases take slots 1..4.
Clusters keep host networking: PD and TiKV advertise one address to peers and clients
alike, and with a bridge network no single address reaches both, so isolation comes from
ports, not networks. Profiles: `single`, `3store` (`chaos` arrives with the chaos tier).
State lives in `$PARITY_BROKER_DIR` (default `~/.local/state/parity-broker`).

## Gate verdict (v8.5.5 cluster, client-rust `integration` @ b10bc69)

> At `b10bc69` (the fork's `integration`, 2026-10-17: upstream `master` @ `3408d95` plus
> tikv/client-rust#557, #551 and #558, and the fork's `AGENTS.md`) the gate is **all
> green**, `d6` and `d7` included, and the ledger holds: G-0001 is FIXED and asserted to
> **agree**, G-0002 still diverges as declared. The upstream view at `3408d95` agrees
> with it. Confirm the checkout with
> `git -C ../client-rust describe --tags --always --dirty`. What changed since the
> previous pin: the subject moved from upstream `master` to `integration` (ADR-0001);
> the three fork PRs moved no verdict and no ledger claim.
> Evidence: [`findings/gate-evidence-b10bc69.txt`](findings/gate-evidence-b10bc69.txt)
> (full `make verdict` and `make ledger` output at this pin, and the upstream view).

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

1. **Bug (regression in #519) — an orphaned lock is never resolved; filed as #543 — FIXED upstream by #561** (our #544 closed as superseded)
   (`d6`, **red** on `e53837d`, green since `3408d95`; ledger **`G-0001`**, now FIXED and asserted to agree;
   [findings/](findings/txn-not-found-lock-resolution.md)). **Now also stated against
   the oracle**: `scenarios/orphaned-lock-resolution.json` manufactures one orphan with
   client-go's `CommitterProbe` and hands it to *each* client to resolve. client-go's
   read succeeds and its resolver clears the lock (0 remaining); at `e53837d`
   client-rust's read failed with `MultipleKeyErrors` and the lock stayed (1 remaining),
   and since `3408d95` the two agree. Because
   both runs share one setup, the divergence is attributable to the **reader alone** —
   which `d6` cannot claim, since it must manufacture the orphan through the public API
   with a region split and a racing txn, and so cannot tell *"I failed to build the
   orphan"* apart from *"the client failed to resolve it"*. Verified both ways at
   `e53837d`: applying [the fix](findings/fix-check-txn-status-wrapper.patch) made the
   divergence vanish entirely, and `ledger-check` then failed with **XCONVERGE**, as it had
   to while G-0001 was declared `diverges` (it is now `agrees`, and that outcome HOLDS). A lock on a
   secondary key whose primary was never written (crash between per-region
   prewrites, or a failed commit's residue) makes the key unreadable **and
   unwritable** forever: `MultipleKeyErrors([KeyError { txn_not_found }])`,
   unchanged by TTL expiry. At `e53837d` the heal path (`rollback_if_not_exist`
   escalation in `get_txn_status_from_lock`) was dead code: `check_txn_status` matched
   `Error::ExtractedErrors`, but its plan shape could only deliver the key error
   as `Error::MultipleKeyErrors` — the sibling call sites with the opposite
   adapter order are why their identical match arms *did* work. Introduced by
   `7d80f59` (#519); the pre-#519 resolver recovered this state via legacy
   `Cleanup`; fixed by #561, whose wider `resolve_locks` rework includes the same
   match-arm change. `d6` asserts the correct behavior (the orphan must be resolved
   on read after TTL), so it failed until the fix landed; the one-line fix
   ([findings/fix-check-txn-status-wrapper.patch](findings/fix-check-txn-status-wrapper.patch))
   makes it pass, validated twice (post-TTL self-heal; 63/63 upstream unit
   tests green). The harness carries **no janitor** — resolving orphaned locks
   is the client's responsibility. (A production backend forced to ship before
   the fix releases could run client-rust's own `cleanup_locks` maintenance
   API from a custodian, but that belongs in the backend/custodian, not in
   this evaluation harness.)
2. **Failed commits leave prewrite locks — one bug + one gap** (adversarially
   reviewed, confirmed against TiKV server source, **empirically reproduced**
   by `failpoint_gate.rs::d7` / `make failpoint-test`; filed as #545; **the bug half is FIXED by #547** (`d7` green since `3408d95`); write-up in
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
     Still open: tracked as getwyrd/client-rust-test#60.

   The store here uses the API correctly — `rollback()` after every failed
   commit, skipped on `UndeterminedError` — and relies on **client-rust** to
   resolve the residue (which, since #561, self-heals on the next read; finding
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

### History: the verdict at `3408d95`

> The last pin on upstream `master` (tikv/client-rust#561, 2026-10-17), before the
> subject moved to `integration`. The gate was **all green**, `d6` and `d7` included, and
> the ledger held: G-0001 FIXED and asserted to agree, G-0002 diverging as declared.
> Against `e53837d`, the orphaned-lock fix had landed inside #561's `resolve_locks`
> rework (our #544 closed as superseded), and the pessimistic-rollback fix as #547.
> Evidence: [`findings/gate-evidence-3408d95.txt`](findings/gate-evidence-3408d95.txt).

### History: the verdict at `e53837d`

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
