---
created: 17.10.2026
type: index
tags:
  - adr
---
# Architecture Decision Records

Numbered records of the harness's significant decisions and *why* they were made. An ADR is
not edited after acceptance; a later ADR supersedes it, and this index records the
supersession. The format follows pdca-harness's lightweight Nygard template (its ADR-0001).
These are the harness's own ADRs; client-rust-pdca keeps the loop instance's, including the
loop's view of this harness (candidate provenance, the cluster broker, ORACLE-WRONG).

## Index

| # | Title | Decided | Status |
|---|-------|---------|--------|
| [0001](0001-pin-follows-the-integration-branch.md) | The client-rust pin follows the fork's `integration` branch | 20.10.2026 | Accepted |
| [0002](0002-parity-is-one-directional.md) | Parity is one-directional: Rust ⊇ Go | — | Proposed |
| [0003](0003-never-emulate-a-missing-feature.md) | Never emulate a missing feature | — | Proposed |
| [0004](0004-the-runner-links-neither-client.md) | The runner links neither client | — | Proposed |
| [0005](0005-no-workarounds-in-the-harness.md) | The harness carries no workarounds for client bugs | — | Proposed |
| [0006](0006-tikv-v8-5-5-is-the-ci-floor.md) | TiKV v8.5.5 is the CI floor | — | Proposed |
| [0007](0007-client-go-is-pinned-by-module.md) | client-go is pinned by module, never replaced | — | Proposed |
