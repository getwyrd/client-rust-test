---
created: 19.10.2026
type: adr
status: Accepted
decided: 20.10.2026
tags:
  - adr
---
# 0002. Parity is one-directional: Rust ⊇ Go

## Context

The harness compares two clients. A comparison can be read two ways: client-rust should do
what client-go does, or the two should do the same things. The second reading would make
every Rust-only feature a gap: the sync wrappers, `CheckLevel` drop checks,
`HeartbeatOption`, explicit read-only transactions, the raw coprocessor endpoint, the
keyspace `api-v2-no-prefix` mode. Closing such a "gap" would mean removing a working
feature from client-rust, or asking client-go to add one. Neither is this harness's job.

client-go is TiKV's reference client. It is the oracle because its behaviour at the pin is
the best available statement of what the server expects from a client, where kvproto leaves
it implicit (client-rules/v1 §0).

## Decision

Parity is one-directional: client-rust must do what client-go does (`Rust ⊇ Go`), and
nothing is required in the other direction.

- client-go is the oracle only. A ledger entry states what client-go does and what
  client-rust must do (ledger/v1 §4.2 `expectation`).
- Rust-only surface is not a gap. It is recorded once, in `docs/parity-roadmap.md` §1.3, so
  no audit counts it again.
- When client-go itself is wrong, that is not a reason for client-rust to copy it. A human
  declares it (ORACLE-WRONG, ledger/v1 §4.6) and reports it to client-go upstream.

## Consequences

- No scenario binds a role to Rust where only Rust has the operation: there would be no
  oracle run to compare against.
- A gap closes only by changing client-rust. The harness never "closes" one by narrowing
  what it compares.
- client-go's errors are not the harness's to fix. They are reported upstream, and the
  ledger records them as declared, never by editing the oracle (ADR-0007).
