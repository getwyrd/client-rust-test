#!/usr/bin/env bash
# Run the suites and check them against the verdict EXPECTED at the pinned revision.
#
# A plain pass/fail signal is meaningless for this repo. The harness carries no
# workarounds for client bugs: where the client is deficient, that is a finding
# expressed as a FAILING TEST that asserts the CORRECT behavior. So a test can be
# expected-to-fail, and a suite that is "all green" would actually mean the
# expectations are stale.
#
# Hence the table below and its two rules:
#
#   XFAIL  an expected-red test failed  -> correct; the gap is still open.
#   XPASS  an expected-red test PASSED  -> HARD FAILURE. The gap closed upstream,
#          so the pin, the README verdict, and (later) the ledger are all stale.
#
# The XPASS rule is the important one. Without it the harness would quietly rot
# into "these are red forever" and nobody would notice the day a fix landed.
#
# This is a deliberately small stand-in for the parity ledger, which generalizes
# it to every gap with a declared expectation and machine-checked evidence.
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

# ─── The expected-red table: "<test-binary>|<test-name>|<why>" ────────────────
# Each entry asserts the CORRECT behavior and so is red until its fix merges.
# Both fixes are already filed upstream; when they land, XPASS fires here.
XFAIL=(
  "gate|d6_orphaned_lock_must_be_resolved_by_client_rust|#519 regression: an orphaned lock is never resolved (fix: tikv/client-rust#544)"
  "failpoint_gate|d7_pessimistic_rollback_leaves_prewrite_locks|#545: pessimistic rollback leaves prewrite locks (fix: tikv/client-rust#547)"
)

rc=0
# failpoint_gate must be single-threaded: the `fail` registry is process-global.
flags_for() { [ "$1" = failpoint_gate ] && echo "--test-threads=1" || echo ""; }

# ─── 1. Everything that is expected to PASS ──────────────────────────────────
for bin in gate failpoint_gate; do
  skips=()
  for row in "${XFAIL[@]}"; do
    IFS='|' read -r b t _ <<<"$row"
    [ "$b" = "$bin" ] && skips+=(--skip "$t")
  done
  echo "═══ $bin: the tests expected to pass ═══"
  # shellcheck disable=SC2046
  if cargo test --features integration-tests --test "$bin" -- --show-output \
       $(flags_for "$bin") "${skips[@]}"; then
    echo "OK: $bin is green apart from its expected-red tests."
  else
    echo "FAIL: a test expected to PASS did not, in $bin." >&2
    rc=1
  fi
  echo
done

# ─── 2. Each expected-red test, individually ─────────────────────────────────
for row in "${XFAIL[@]}"; do
  IFS='|' read -r bin test why <<<"$row"
  echo "═══ XFAIL: $bin::$test ═══"
  echo "    expected red — $why"
  # shellcheck disable=SC2046
  if cargo test --features integration-tests --test "$bin" -- --show-output \
       $(flags_for "$bin") --exact "$test"; then
    cat >&2 <<EOF

XPASS — $test PASSED, and it is declared expected-to-fail.

  $why

The gap appears to be CLOSED at the pinned revision. That is good news, and it is
a hard failure on purpose: the pin, the README's verdict, and the ledger entry for
this gap are now stale and must be re-stated.

  1. confirm the fix merged upstream (tikv/client-rust)
  2. bump pins.toml client_rust.rev to a commit that contains it
  3. drop this row from the XFAIL table and flip the ledger entry to FIXED
EOF
    rc=1
  else
    echo "XFAIL as expected: the gap is still open."
  fi
  echo
done

if [ "$rc" -eq 0 ]; then
  echo "VERDICT: as expected at the pinned revision (all green except the declared XFAILs)."
else
  echo "VERDICT: does NOT match the expectation for the pinned revision." >&2
fi
exit "$rc"
