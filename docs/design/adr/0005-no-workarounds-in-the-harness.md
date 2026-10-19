---
created: 19.10.2026
type: adr
status: Proposed
decided:
tags:
  - adr
---
# 0005. The harness carries no workarounds for client bugs

## Context

This repository started as the wyrd M4 evaluation gate: a set of tests against the
`tikv-client` API wyrd would rely on. When a test hit a client bug (the orphaned lock that
became G-0001, `gate::d6`), the quick fix was to retry, catch the error, or reshape the
test until it passed. Each such change would have made the gate green and the client no
better, and the next reader would have no way to tell.

## Decision

The harness relies on client-rust as it is. A client deficiency is a finding, expressed as a
failing test or a ledger divergence, and fixed in client-rust. It is never papered over here.

- A gate test asserts the correct behaviour. While the client is wrong, the test is red,
  and `scripts/gate-verdict.sh` declares it expected-red at the pin.
- A parity scenario records what each client does. The driver never retries, swallows or
  reclassifies an error to make the two agree (observation/v1 §4.1.2).
- A proposed fix lives in client-rust (a fork PR on `integration`, ADR-0001, and upstream),
  never as a patch the harness applies to the tree under test.

## Consequences

- Red is informative: an expected-red test or an XDIVERGE entry is a known, filed bug.
- Both verdicts fail loudly when a fix lands (an unexpected pass, XCONVERGE), so the
  declarations cannot go stale without someone noticing.
- Correct client usage is not a workaround. A caller that restarts a transaction at a fresh
  timestamp after a write conflict is doing what the API asks.
