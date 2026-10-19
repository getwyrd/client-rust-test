---
created: 17.10.2026
type: adr
status: Accepted
decided: 20.10.2026
tags:
  - adr
---
# 0001. The client-rust pin follows the fork's `integration` branch

## Context

Until now `pins.toml` pinned a commit of upstream tikv/client-rust `master`, and
`check-pins.sh` refused any pin that was not an ancestor of upstream `master`. The rule kept
every gap fileable: a gap measured on upstream code is a gap upstream has, and its fix has
somewhere to land.

The PDCA loop (client-rust-loop-design) now needs to build on its own fixes before upstream
merges them. Upstream review takes weeks. Three getwyrd PRs (#557 error variants, #551
failpoint lock residue, #558 TSO deadline) are open upstream, and the next bundles depend
on them. With an upstream-only pin, the harness grades code the loop no longer works on.
It also cannot see whether a fork PR closes a gap until upstream merges it.

getwyrd/client-rust has a protected `integration` branch: upstream `master` with those PRs
merged in. Its branch protection disables force-pushes and requires a pull request, but
requires no status check and does not bind admins: the three PR merges were pushed
directly, and no CI runs on the branch. What keeps a pin sound is therefore this
harness's own checks and its gate, not the branch.

## Decision

The subject is a commit of the fork's `integration` branch. `pins.toml [client_rust]` gains
`branch = "integration"` and `upstream_base`, where `integration` meets upstream `master`.
`check-pins.sh` replaces the single ancestry check with two:

1. `rev` is on the first-parent chain of `<fork>/integration`, so the pin is a state the
   branch has been at: never a stray fork commit, and never a commit of a PR branch
   that was merged in later (ancestry alone would accept one, e.g. a #557-only tree);
2. `upstream_base` is the newest commit on upstream `master`'s first-parent chain that
   `rev` contains, so the branch is built on upstream history, the base is never stale,
   and it is always a state `master` has been at (plain `merge-base` could return a
   carried PR's tip, should upstream merge that PR with a merge commit).

Under `PARITY_STRICT` (CI), any check that cannot run is a failure, as before.

The pin is one specific commit, bumped by hand as a reviewed step, like an upstream pin
move. `integration` is updated by merges only and is never force-pushed, so every past
pin stays reachable and past evidence stays reproducible. Branch protection disables
force-pushes for everyone but admins; for admins it is a rule we keep. When `integration` merges a newer upstream `master`, `upstream_base` moves
in the same pin bump. An automatic last-known-good pin is later work (WP-13).

The gate and the ledger judge the pinned `integration` commit. A second CI job, run on
every PR but never gating, runs the parity diff at `upstream_base` and uploads it. It shows
which gaps are still open upstream. It is not ledger evidence: it runs with
`PARITY_STRICT=0`, and `ledger-check` refuses such a run.

A gap fixed only on `integration` converges in the gating run. Its ledger entry is restated
as for any fix (`status = "FIXED"`, `expect = "agrees"`), and `upstream_pr` names the
still-open upstream PR. The upstream view keeps showing the divergence until that PR
merges. Gaps are still filed against tikv/client-rust, and fixes are still offered there.

Where the fork's API differs from `upstream_base`, the harness has to compile against both
for the upstream view. Today the only difference is #557 (`ExtractedErrors` was replaced by
`MultipleRegionErrors`). The `rust-driver` feature `pre-557` swaps those match arms, and only
the upstream view enables it. Once `upstream_base` contains #557, the feature build fails to
compile, and the feature is deleted.

Such switches are confined and capped. All of the Rust driver's code that depends on a
client-rust API the two trees spell differently lives in one module, so a switch never
spreads through the driver. At most two switches exist at a time. A third API difference
does not get a switch: the upstream view reports the scenarios it cannot build as
`not buildable upstream` and compares the rest. The upstream view is never ledger
evidence, so a scenario it cannot build costs nothing in the ledger. The list stays short
because every bundle is also an upstream pull request and a public API change needs a
human's approval (client-rust-pdca ADR-0015, design WP-14).

## Consequences

- The harness grades the code the loop is building on, and the effect of a fork PR on the
  ledger is visible before upstream merges it.
- The fork, unmerged upstream PRs included, is now part of what a verdict depends on. A
  verdict names an `integration` commit, not an upstream one, and the README says which
  upstream commit it sits on.
- Every API difference between `integration` and `upstream_base` costs a feature switch in
  the Rust driver until upstream catches up, up to the cap of two. Past the cap, the
  upstream view loses the scenarios that need the newer API until upstream catches up.
- Moving the switched arms into one module is a follow-up to this ADR; today the
  `pre-557` arms sit in `crates/rust-driver/src/mapping.rs`.
- The upstream view is an extra cluster job per PR.
- `../client-rust` must carry the fork as a remote with `integration` fetched, beside
  upstream with `master` fetched, or `make pins-check` cannot verify the pin (advisory
  locally, a failure in CI).
