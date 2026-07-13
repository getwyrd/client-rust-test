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

# ── The ORACLE's provenance ──────────────────────────────────────────────────
# Ask GO what it RESOLVED, do not re-read what we asked for. Stamping client_go
# straight out of pins.toml would record what the harness BELIEVES, not what it
# BUILT — the exact hole this file's own header complains about for Rust. Go's
# minimal version selection can resolve client-go ABOVE what go.mod requires (any
# dependency may raise it), so `require` is a floor, not a fact.
#
# `go list -m` reports the version actually selected for the build. Under
# PARITY_STRICT a disagreement with the pin is fatal: an oracle that is not the
# pinned oracle cannot settle a ledger claim.
GO_MOD_PIN=$(pin client_go.module)
GO_VER_PIN=$(pin client_go.version)
go_resolved=""
go_replaced=false

if [ -f go/go.mod ] && command -v go >/dev/null 2>&1; then
  # GOWORK=off: a stray go.work must never silently swap the oracle for a sibling
  # checkout you can edit. GOFLAGS=-mod=mod so a cold cache can resolve rather
  # than erroring under the Makefile's -mod=readonly.
  go_resolved=$(cd go && GOWORK=off go list -m -f '{{.Version}}' "$GO_MOD_PIN" 2>/dev/null || true)
  go_replaced=$(cd go && GOWORK=off go list -m -f '{{if .Replace}}true{{else}}false{{end}}' "$GO_MOD_PIN" 2>/dev/null || echo unknown)
fi

if [ -z "$go_resolved" ]; then
  # No Go module, or no toolchain. Record the pin and say plainly that it is
  # unverified — never let an absent check read like a passed one.
  go_resolved="$GO_VER_PIN"
  go_verified=false
else
  go_verified=true
fi

# AN UNVERIFIED ORACLE IS NOT AN ON-PIN ORACLE.
#
# This once copied the pinned version into `go_resolved` and left `matches_pin=true`,
# so a strict run with no Go toolchain recorded a perfectly on-pin-looking oracle it
# had never actually looked at. That is the absent-check-reads-as-a-pass failure this
# whole file exists to prevent, sitting inside the file itself.
#
# check-pins.sh already says it: "an unverifiable invariant is not a satisfied one."
go_on_pin=true
if [ "$go_verified" = true ]; then
  [ "$go_resolved" = "$GO_VER_PIN" ] || go_on_pin=false
  [ "$go_replaced" = "false" ] || go_on_pin=false
else
  go_on_pin=false
fi

# ── The CLUSTER's provenance ─────────────────────────────────────────────────
# Ask the RUNNING CLUSTER what it is. Do not re-read what we asked for.
#
# Copying cluster.pd_image out of pins.toml would record what the harness BELIEVES,
# not what it EXERCISED — the same hole this file's header rightly complains about
# for Rust, and the same one the client_go stamp above closes. It is worth spelling
# out why it matters here: $PD_ADDRS can point anywhere. `make ledger` does not
# depend on `cluster-up`, so a run against a hand-started TiKV of some other version
# would otherwise be stamped with the pinned image digests and published as a pinned
# run. Every behavioural claim in the ledger is a claim about a CLIENT talking to a
# SPECIFIC TiKV; certifying one against an unknown server is not evidence.
#
# PD's HTTP API is the ground truth (the same source crates/harness uses for region
# layout). The image DIGEST cannot be recovered from a running cluster, so what is
# checked is the version it reports — which is what a claim actually depends on.
PD_ONE="${PD_ADDRS:-127.0.0.1:2379}"
PD_ONE="${PD_ONE%%,*}"
TIKV_VER_PIN=$(pin cluster.tikv_version)

pd_version=""
tikv_versions=""
cluster_verified=false
cluster_on_pin=true

if pd_json=$(curl -sf --max-time 5 "http://${PD_ONE}/pd/api/v1/version" 2>/dev/null); then
  pd_version=$(python3 -c "import json,sys; print(json.loads(sys.argv[1]).get('version',''))" "$pd_json")
  if stores_json=$(curl -sf --max-time 5 "http://${PD_ONE}/pd/api/v1/stores" 2>/dev/null); then
    # Every store that is Up, by version. A mixed-version cluster is itself a red flag.
    tikv_versions=$(python3 -c "
import json,sys
d = json.loads(sys.argv[1])
vs = sorted({s['store'].get('version','?') for s in d.get('stores', [])
             if s['store'].get('state_name') == 'Up'})
print(','.join(vs))" "$stores_json")
    cluster_verified=true
  fi
fi

if [ "$cluster_verified" = true ]; then
  # PD reports "v8.5.5"; TiKV stores report "8.5.5". Compare without the leading v.
  want="${TIKV_VER_PIN#v}"
  [ "${pd_version#v}" = "$want" ] || cluster_on_pin=false
  [ -n "$tikv_versions" ] || cluster_on_pin=false
  for v in ${tikv_versions//,/ }; do
    [ "${v#v}" = "$want" ] || cluster_on_pin=false
  done
else
  cluster_on_pin=false
fi

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
  "client_go": {
    "module": "$GO_MOD_PIN",
    "version": "$go_resolved",
    "pinned_version": "$GO_VER_PIN",
    "replaced": $( [ "$go_replaced" = true ] && echo true || echo false ),
    "verified": $go_verified,
    "matches_pin": $go_on_pin
  },
  "cluster": {
    "pd_addr": "$PD_ONE",
    "pinned_version": "$TIKV_VER_PIN",
    "pinned_pd_image": "$(pin cluster.pd_image)",
    "pinned_tikv_image": "$(pin cluster.tikv_image)",
    "observed_pd_version": "$pd_version",
    "observed_tikv_versions": "$tikv_versions",
    "verified": $cluster_verified,
    "matches_pin": $cluster_on_pin
  },
  "toolchain": { "rust": "$(rustc -V 2>/dev/null || echo unknown)", "go": "$(go version 2>/dev/null || echo absent)" },
  "harness": { "rev": "$(git rev-parse HEAD)", "dirty": $( [ -n "$(git status --porcelain)" ] && echo true || echo false ) }
}
EOF

echo "provenance -> $OUT"
echo "  client-rust: $describe ($branch)${dirty:+}"
[ "$dirty" = true ] && echo "  client-rust: DIRTY"

if [ "$go_verified" = true ]; then
  echo "  client-go:   $go_resolved (resolved by go list -m)"
else
  echo "  client-go:   $go_resolved (UNVERIFIED — no go/ module or no go toolchain)"
fi

if [ "$cluster_verified" = true ]; then
  echo "  cluster:     PD $pd_version, TiKV $tikv_versions (asked $PD_ONE)"
else
  echo "  cluster:     UNVERIFIED — no PD reachable at $PD_ONE"
fi

# ── The cluster gate ─────────────────────────────────────────────────────────
# Under PARITY_STRICT, a cluster we could not identify is a REFUSAL, not a shrug.
# check-pins.sh already states the principle: "an unverifiable invariant is not a
# satisfied one." A behavioural claim is a claim about a client talking to a
# specific TiKV; against an unknown server it certifies nothing.
if [ "$cluster_on_pin" = false ]; then
  if [ "$cluster_verified" = false ]; then
    why="no PD reachable at $PD_ONE, so the cluster under test cannot be identified"
  else
    why="the cluster is PD $pd_version / TiKV [$tikv_versions], but the pin names $TIKV_VER_PIN"
  fi
  if [ "$STRICT" = "1" ]; then
    cat >&2 <<EOF

REFUSING TO RUN: $why.

Every behavioural claim in the ledger is a claim about a CLIENT talking to a SPECIFIC
TiKV. Lock resolution, prewrite residue and conflict shapes are all server behaviour as
much as client behaviour, so a result gathered against an unknown or different server is
not evidence for the pinned world — it merely looks like it.

Run \`make cluster-up\` (digest-pinned images from pins.toml), or point \$PD_ADDRS at a
cluster of the pinned version, or re-pin cluster.tikv_version (a reviewed change: it
re-states every ledger claim).
EOF
    exit 1
  fi
  echo "  cluster:     OFF PIN — $why (advisory; CI refuses this)" >&2
fi

# ── The oracle gate ──────────────────────────────────────────────────────────
# An oracle you can accidentally edit is not an oracle (pins.toml). That has been
# a comment; here it becomes a check. A `replace` — however it got there, usually a
# stray go.work — silently swaps the pinned, content-addressed oracle for a working
# tree someone can change, and every parity claim settled against it is void.
if [ "$go_on_pin" = false ]; then
  if [ "$go_verified" = false ]; then
    why="the oracle could not be verified (no go/go.mod, or no go toolchain), so which client-go would run is unknown"
  elif [ "$go_replaced" = true ]; then
    why="client-go is REPLACED — the oracle is a local tree, not the pinned module"
  else
    why="client-go resolved to $go_resolved, but the pin names $GO_VER_PIN"
  fi
  if [ "$STRICT" = "1" ]; then
    cat >&2 <<EOF

REFUSING TO RUN: $why.

The oracle defines what CORRECT means here. A run against a different client-go —
or against one someone can edit — proves nothing about client-rust, because the
baseline itself is unknown. Unset GOWORK/go.work, or re-pin client_go in pins.toml
(a reviewed change: it re-states every ledger claim).
EOF
    exit 1
  fi
  echo "  client-go:   OFF PIN — $why (advisory; CI refuses this)" >&2
fi

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
