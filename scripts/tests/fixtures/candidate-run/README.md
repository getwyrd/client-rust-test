Real results from a candidate-mode run (`PARITY_MODE=candidate PARITY_GAP=G-0002 make
ledger`, client-rust at the pin 385a1a3 plus one uncommitted line, on a leased v8.5.5
cluster; G-0001 all-Go against all-Rust, and G-0003). Recorded on the pin bump's branch
with `origin/main` pointed at the bump's pin commit, so the run's base already names the
new pin and ledger-check judged it. `test_candidate_mode.py` copies them into a scratch harness, rewrites the two
digests to match the scratch trees, and then changes one field at a time.
