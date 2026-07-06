## What & why

<!-- What does this change and why. Link the wyrd proposal obligation / issue. -->

## Kind of change

- [ ] Gate/harness code (`src/`, `tests/`)
- [ ] New or updated finding (`findings/`)
- [ ] Cluster / tooling (`cluster/`, `Makefile`)
- [ ] Docs (`README.md`)

## Checklist

- [ ] `make check` is green (fmt, clippy `-D warnings`, check)
- [ ] Cluster-backed changes verified with `make gate` against a real TiKV/PD cluster
- [ ] Each new test names the proposal obligation (or finding) it verifies
- [ ] The harness carries **no workaround** for a client-rust bug — deficiencies are surfaced as a failing test / finding, not papered over
- [ ] `README.md` "Gate verdict" / findings updated if behavior or evidence changed

## Evidence

<!-- Paste the relevant captured shape / test output, or link the findings/ file. -->
