---
created: 19.10.2026
type: adr
status: Proposed
decided:
tags:
  - adr
---
# 0006. TiKV v8.5.5 is the CI floor

## Context

Every behavioural claim is half server behaviour: lock resolution, prewrite residue and
conflict shapes come from TiKV and PD as much as from the client. A claim stated against an
unnamed cluster version cannot be reproduced, and one stated against a mutable image tag
cannot either (`pingcap/tikv:v8.5.5` was re-pushed on 2026-08-12).

client-rust's own CI runs against TiKV v8.5.5 (`TIKV_VERSION` in its `ci.yml`). client-go
at the pin has already moved past some APIs the vendored protos carry, so newer servers do
not simply add behaviour.

## Decision

Every ledger claim is stated against TiKV and PD v8.5.5, the version client-rust's CI uses.

- `pins.toml [cluster]` pins v8.5.5 by tag and digest. `provenance/v1` §4.5 states the
  shape, and `make spec-check` (part of `make check`) refuses an image without a digest.
- A strict run against any other observed version is refused (provenance/v1 §4.2.2).
- Other versions are pinned the same way under `[cluster.versions]` (`make pins-check`
  refuses one without a digest) and leased with
  `scripts/cluster-broker lease <profile> --tikv <version>`, for example
  `scripts/cluster-broker lease single --tikv v8.5.8`. A verdict from one is evidence
  about that version only, never about the ledger.

## Consequences

- Moving the floor is a pin move: it re-states every claim, and is reviewed as one.
- A feature that needs a newer server must say so and say how it behaves on v8.5.5
  (`docs/parity-roadmap.md`, ground rules).
- The version matrix (the wyrd deployment version, nightly) adds evidence without changing
  what the ledger claims.
