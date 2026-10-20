---
created: 19.10.2026
type: adr
status: Accepted
decided: 20.10.2026
tags:
  - adr
---
# 0004. The runner links neither client

## Context

The runner decides every verdict: it drives the scenario, projects the traces and writes
the divergence report that `ledger-check` judges. If the runner could link `tikv-client`,
it could use the client's types, call it for a convenience, or share a bug with it, and the
verdict would then say as much about the runner as about the client. The same holds for
client-go and for the protocol crate the runner depends on.

## Decision

`parity-runner` and `parity-proto` depend on neither client. Each client is reached only
through its driver process, over the protocol (parity-protocol/v1).

- The absence is enforced in each crate's `Cargo.toml`, not just intended: both carry a
  comment explaining why `tikv-client` is missing, and a change that adds it is a change
  to this decision.
- Each driver owns its half of the error mapping (`crates/rust-driver/src/mapping.rs`,
  `go/driver/observation.go`), because a mapping from a client's errors depends on that
  client.
- `parity-proto` is the contract, written down in parity-protocol/v1 and observation/v1.

## Consequences

- A runner bug cannot hide inside a client, or the other way round.
- Adding a command means changing three places: `parity-proto`, the Rust driver and the Go
  driver (parity-protocol/v1 §5).
- The loop treats every crate the runner links as part of the judge
  (client-rust-pdca, T3-ledger): a bundle that touches one cannot be measured by T3.
