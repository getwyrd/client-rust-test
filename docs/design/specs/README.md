---
created: 19.10.2026
type: index
tags:
  - spec
---
# Specifications

Normative, versioned contracts for the files and protocols this harness reads and writes.
They use [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119) keywords. A spec is changed in
the same PR as the code it describes; `make spec-check` (`scripts/spec-check.sh`) checks
`ledger.toml`, `scenarios/` and `pins.toml` against them, and CI runs it in `make check`.

The loop's own design (risk classes, the loop's ADRs, accept policy) lives in
[client-rust-pdca](https://github.com/getwyrd/client-rust-pdca/tree/main/docs/design).

| Spec | Covers |
|---|---|
| [parity-protocol/v1](parity-protocol/v1.md) | the runner ↔ driver command protocol (`parity-cmd/v1`) |
| [observation/v1](observation/v1.md) | observations, classes, the projection and the diff; trace and divergence files |
| [scenario/v1](scenario/v1.md) | `scenarios/*.json` |
| [ledger/v1](ledger/v1.md) | `ledger.toml` and its verdicts |
| [provenance/v1](provenance/v1.md) | the provenance stamp, strict / candidate / advisory, `pins.toml` |
| [client-rules/v1](client-rules/v1.md) | what a TiKV client MUST, SHOULD and CAN do |
