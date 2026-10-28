---
created: 19.10.2026
type: adr
status: Accepted
decided: 20.10.2026
tags:
  - adr
---
# 0003. Never emulate a missing feature

## Context

Some client-go operations have no client-rust counterpart at all. RawKV `Checksum` is the
first one the ledger records (G-0002): client-go sends `kvrpcpb.RawChecksumRequest` and the
server computes a crc64 over the range; client-rust has no such request. A driver could
make the two agree by scanning the range and hashing it client-side. That would checksum
different bytes, under different semantics, with different cost, and the diff would come
out clean. The harness would then report parity for a capability client-rust does not have.

*(Note, 2026-10-26: the example closed as designed — getwyrd/client-rust#6 added
`RawClient::checksum` on the fork's `integration` branch, the Rust driver calls it, and
G-0002 is re-stated as an `agrees` regression guard. The context above is kept as written;
the upstream view, whose client-rust still lacks the method, keeps answering
`unsupported`.)*

The same temptation exists for `LockNoWait`, fair locking, `CommitterProbe`, and every
future gap.

## Decision

A driver whose client lacks a capability answers `unsupported`, with a detail string saying
what is missing. It never builds a stand-in from other calls.

- `unsupported` is a first-class class (observation/v1 §4.2.4): admissible, comparable,
  and projected with its detail.
- A missing capability becomes a ledger entry of kind `capability` (ledger/v1 §4.3), with
  `oracle = "ok"`, `subject = "unsupported"`.
- The gap closes when client-rust gains the capability; the entry then comes out XCONVERGE
  and is flipped to an `agrees` regression guard.

## Consequences

- The capability inventory is honest: every `unsupported` is a real absence.
- Some scenarios can only show a class difference until client-rust implements the
  feature; their evidence fields (such as `checksum`) are kept in the trace but not compared.
- Reviewers treat any driver code that combines client calls into an operation the client
  does not offer as a defect (AGENTS.md, hard conventions).
