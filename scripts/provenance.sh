#!/usr/bin/env bash
# Record WHAT WAS ACTUALLY TESTED, and — under PARITY_STRICT — refuse to run at
# all when that is not what pins.toml says.
#
# This exists because Cargo cannot pin a path dependency. `tikv-client = { path
# = "../client-rust" }` resolves to whatever is in that directory, and Cargo.lock
# records no source or rev for it. So the build itself can never tell you which
# revision of the crate under test you just exercised. Today the sibling checkout
# is a CLEAN tree three commits ahead of upstream — which looks perfectly
# trustworthy and is not the baseline at all.
#
# The fix is not to rewrite the dependency (that would break the whole point of
# the repo: pointing the harness at a local branch to prove a fix). The fix is to
# RECORD and ASSERT:
#
#   PARITY_STRICT=1  -> abort before a single test runs if we are off-pin.
#   PARITY_STRICT=0  -> proceed, but stamp the truth and say so loudly.
#
# Downstream, `ledger-check` refuses any result whose provenance says
# strict:false. A ledger claim can only ever be settled by a pinned run.
#
# READ-ONLY with respect to ../client-rust.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

PINS="${PINS:-pins.toml}"
CLIENT_RUST="${CLIENT_RUST:-../client-rust}"
OUT="${1:-results/provenance.json}"
STRICT="${PARITY_STRICT:-0}"

pin() { python3 -c "
import tomllib,pathlib
d=tomllib.loads(pathlib.Path('$PINS').read_text())
cur=d
for k in '$1'.split('.'):
    cur=cur[k]
print(cur)"; }

PIN_REV=$(pin client_rust.rev)

if [ -d "$CLIENT_RUST/.git" ]; then
  rev=$(git -C "$CLIENT_RUST" rev-parse HEAD)
  describe=$(git -C "$CLIENT_RUST" describe --tags --always --dirty 2>/dev/null || echo unknown)
  branch=$(git -C "$CLIENT_RUST" rev-parse --abbrev-ref HEAD 2>/dev/null || echo detached)
  if [ -n "$(git -C "$CLIENT_RUST" status --porcelain)" ]; then dirty=true; else dirty=false; fi
else
  rev=unknown; describe=unknown; branch=unknown; dirty=true
fi

if [ "$rev" = "$PIN_REV" ] && [ "$dirty" = false ]; then on_pin=true; else on_pin=false; fi

mkdir -p "$(dirname "$OUT")"
cat > "$OUT" <<EOF
{
  "schema": "parity-provenance/v1",
  "captured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "strict": $( [ "$STRICT" = "1" ] && echo true || echo false ),
  "client_rust": {
    "path": "$CLIENT_RUST",
    "rev": "$rev",
    "branch": "$branch",
    "describe": "$describe",
    "dirty": $dirty,
    "pinned_rev": "$PIN_REV",
    "matches_pin": $on_pin
  },
  "client_go": { "module": "$(pin client_go.module)", "version": "$(pin client_go.version)" },
  "cluster": { "pd_image": "$(pin cluster.pd_image)", "tikv_image": "$(pin cluster.tikv_image)" },
  "toolchain": { "rust": "$(rustc -V 2>/dev/null || echo unknown)", "go": "$(go version 2>/dev/null || echo absent)" },
  "harness": { "rev": "$(git rev-parse HEAD)", "dirty": $( [ -n "$(git status --porcelain)" ] && echo true || echo false ) }
}
EOF

echo "provenance -> $OUT"
echo "  client-rust: $describe ($branch)${dirty:+}"
[ "$dirty" = true ] && echo "  client-rust: DIRTY"

if [ "$on_pin" = true ]; then
  echo "  on pin ($PIN_REV) — this run is admissible as evidence."
  exit 0
fi

if [ "$STRICT" = "1" ]; then
  cat >&2 <<EOF

REFUSING TO RUN: the crate under test is not at the pinned revision.

  pinned  : $PIN_REV
  actual  : $rev ($branch)$( [ "$dirty" = true ] && echo ", DIRTY" )

A result produced here would look identical to a pinned one but mean something
different, so it is not evidence and must not be published. Either re-pin
pins.toml (a reviewed change — it re-states every ledger claim), or run without
PARITY_STRICT to iterate locally.
EOF
  exit 1
fi

cat >&2 <<EOF

  ┌─ ADVISORY RUN ───────────────────────────────────────────────────────────┐
    The crate under test is NOT at the pinned revision.
      pinned: $PIN_REV
      actual: $rev ($branch)$( [ "$dirty" = true ] && echo ", DIRTY" )
    Fine for iterating on a fix. NOT admissible as evidence: the ledger rejects
    any result whose provenance says strict:false.
  └──────────────────────────────────────────────────────────────────────────┘
EOF
