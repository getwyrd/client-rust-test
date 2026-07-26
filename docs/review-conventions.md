# Conventions distilled from upstream review

**Purpose.** When a maintainer's review — or a review tool — surfaces a *general* practice
rather than a one-off fix, record it here, so we discover it once instead of once per PR.
This is the engineering-practice companion to [client-rules.md](client-rules.md): that
document says how a *client* must behave (a contract graded against client-go); this one
says how *we* should build and submit changes to client-rust, as learned from the people
who review them.

**How to use.** Skim this before writing or reviewing a client-rust change — tests
especially. When a review lands a durable pattern, add an entry in the same PR. Each entry
carries its *why* and its *caveat*, because the reason a convention exists is what tells you
when it stops applying. IDs are stable (`T-1` stays `T-1`); new conventions append.

**Sources.** In order of authority: (1) a TiKV maintainer's review comment on one of our
PRs; (2) a review tool (codex / coderabbit) finding that generalises; (3) a pattern we hit
twice and don't want to hit a third time. Every entry cites where it came from.

---

## T — Testing against a shared cluster

### T-1 — Scope state assertions to a test's own keys, and keep multi-key tests multi-region

**Convention.** An integration test that asserts on cluster-global state (e.g. *"N locks
remain"*) must bound its observation to keys it owns — never the whole keyspace. A
single-key test scans that exact key. A multi-key test takes a disjoint **band** of the key
space and scans only that band; and the band must be a *large fraction* of the space (not a
single leading byte) so it still spans several regions under `MULTI_REGION`.

**Origin.** [tikv/client-rust#551](https://github.com/tikv/client-rust/pull/551) (fix for
tikv/client-rust#525) — the key-scoping idea is @pingyu's review suggestion; the
region-coverage caveat came from a codex review of the same PR that caught the first,
too-narrow implementation.

**Why — two forces, both of which must hold:**

- *Residue immunity.* Serial tests share one cluster. Async-commit locks resolve in the
  background and drain at an unpredictable rate, so a whole-keyspace lock scan in test B can
  catch test A's still-draining locks and over-count — the observed #525 symptom was `575`
  locks where `512` were expected. Counting only your own keys removes the race at its
  source; a *bounded* scan is better still, because residue outside the scanned range is
  never even fetched from the server.
- *Region coverage.* The residue fix must not shrink what the test exercises. The
  cleanup / resolve-lock paths are region-aware — they iterate regions and retry on region
  errors — which is exactly why the harness pre-splits into ~40 regions under `MULTI_REGION`.
  A one-byte key prefix collapses all of a test's keys into `[prefix, prefix+1)`; with ~40
  regions spread across the 4-byte key space that interval usually falls inside a **single**
  region, silently disabling the multi-region coverage the test was written to have. A band
  of `1 / N` of the key space (N = number of isolated cases) keeps each case on several
  regions while still isolating it. The tension is real: contiguous, disjoint, *and* wide
  ranges can only ever give each of N cases about `regions / N` regions — so keep N no larger
  than the isolation actually requires.

**How to apply.** `client-rust/tests/failpoint_tests.rs` is the worked example:
`band_bounds` / `band_range_bytes` tile the u32 key space; `count_locks_in_band` /
`wait_for_locks_count_in_band` scope multi-key cases; `count_locks_of_key` scopes single-key
cases. Keys are generated with `rng.gen_range(lo..hi)` so they stay in-band yet spread across
it, and the scan range is handed straight to `scan_locks`, which bounds the scan server-side.

**Scope.** Flake-hardening only — no behavioural claim, so no `ledger.toml` entry. The
parity-runner analogue is the harness's own serial-isolation discipline for scenarios that
mutate shared cluster state.

---

## P — Process (how we submit)

### P-1 — One concern per PR

**Convention.** A PR carries a single concern. When a change grows a second one — a
mechanical re-vendor that also happens to adjust behaviour, a bugfix that also renames a
type — split it, and let the follow-ups depend on the first rather than ride along inside it.

**Origin.** [tikv/client-rust#550](https://github.com/tikv/client-rust/pull/550) —
@pingyu asked to split the `errorpb.UndeterminedResult` and shared-lock handling out of the
kvproto re-vendor so the PR did only the re-vendor. We split them
(`split/kvproto-revendor` → `split/undetermined-result` → `split/shared-locks`) and #550
merged as the re-vendor alone.

**Why.** A focused diff is reviewable and revertable; a maintainer can `/lgtm` a mechanical
re-vendor in one pass but must re-derive semantics if behaviour is folded in. Under
prow/tide, review load per PR is real friction — a bundled PR stalls on its riskiest hunk.
It also keeps the ledger honest: a behavioural change lands in a PR whose scenario proves it,
not buried in a vendoring commit.

**How to apply.** Scope a PR to what its title claims. If a reviewer says "split this," they
are almost always right — the cost of splitting is a few dependent branches; the cost of not
is a stalled review and a muddied history.
