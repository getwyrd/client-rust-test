#!/usr/bin/env bash
# Check ledger.toml, scenarios/ and pins.toml against docs/design/specs/.
#
# Two halves. The runner checks each scenario on its own, with the same loader that runs
# it (scenario/v1 §4.4), so this script keeps no second list of ops or fields that could
# drift. spec-check.py then checks what spans files: each ledger entry against its
# scenario, the ledger's and pins.toml's shapes. No cluster.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

cargo run -q -p parity-runner -- --check scenarios/*.json
python3 -I scripts/spec-check.py .
