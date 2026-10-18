Real results from a candidate-mode run (`PARITY_MODE=candidate PARITY_GAP=G-0002 make
ledger`, client-rust at the pin b10bc69 plus one uncommitted line, on a leased v8.5.5
cluster). `test_candidate_mode.py` copies them into a scratch harness, rewrites the two
digests to match the scratch trees, and then changes one field at a time.
