---
created: 19.10.2026
type: adr
status: Proposed
decided:
tags:
  - adr
---
# 0007. client-go is pinned by module, never replaced

## Context

The oracle is only useful if nobody can change it by accident. A sibling checkout wired in
with a `replace` directive, or a `go.work` file, would let a local edit to client-go reach
every result, with nothing in the result showing it. Both are normal Go development tools,
which is what makes them dangerous here.

## Decision

The Go driver requires `github.com/tikv/client-go/v2` at the version `pins.toml` names, as a
module, and nothing may replace it.

- `go/go.mod` carries a `require` of the pinned pseudo-version, content-addressed through
  `go.sum`. `make pins-check` refuses a `replace` of the module and a tracked `go/go.work`.
- A gitignored `go.work` remains the local way to hack on client-go. CI sets `GOWORK=off`,
  so it never reaches a CI result.
- The binary reports what it actually linked: the Go driver's `hello` carries the version
  and a `replaced` flag from `runtime/debug.ReadBuildInfo()` (parity-protocol/v1 §4.2), and
  `ledger-check` refuses a trace whose Go driver reports a replaced or off-pin client-go
  (provenance/v1 §4.2.2).

## Consequences

- An oracle result is reproducible from `pins.toml` alone.
- Testing a client-go change means a local `go.work` and an advisory run; it can never
  settle the ledger.
- Moving the oracle is a pin move, reviewed as one; when it turns `agrees` claims red, those
  claims are re-stated, not the oracle patched.
