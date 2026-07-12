#!/usr/bin/env bash
# Assert that every consumer of a pinned value agrees with pins.toml, and that
# the client-rust pin is an ancestor of upstream/master.
#
# The ancestry check is the load-bearing one. A pin that is not upstream can
# never be a baseline for a gap report: the "gap" might only exist on a fork
# branch, and the fix would have nowhere to land. This is a cheap, static check
# that makes that class of mistake impossible.
#
# READ-ONLY with respect to ../client-rust. It never fetches, checks out, or
# writes anything there — another session may be working in that tree.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

PINS="${PINS:-pins.toml}"
CLIENT_RUST="${CLIENT_RUST:-../client-rust}"
fail=0

note() { printf '  %s\n' "$*"; }
bad()  { printf 'FAIL: %s\n' "$*" >&2; fail=1; }

# tomllib is stdlib from Python 3.11; ubuntu-24.04 runners ship 3.12. No extra
# tool to install, and no third parser to keep in sync.
pin() { python3 -c "
import sys,tomllib,pathlib
d=tomllib.loads(pathlib.Path('$PINS').read_text())
cur=d
for k in '$1'.split('.'):
    cur=cur[k]
print(cur)"; }

echo "pins.toml:"
RUST_REV=$(pin client_rust.rev)
RUST_UPSTREAM=$(pin client_rust.upstream)
GO_MOD=$(pin client_go.module)
GO_VER=$(pin client_go.version)
RUST_TC=$(pin toolchain.rust)
GO_TC=$(pin toolchain.go)
note "client_rust.rev  = $RUST_REV"
note "client_go        = $GO_MOD@$GO_VER"
note "toolchain        = rust $RUST_TC / go $GO_TC"

# ── 1. rust-toolchain.toml must match the pinned Rust toolchain ──────────────
if [ -f rust-toolchain.toml ]; then
  have=$(python3 -c "
import tomllib,pathlib
print(tomllib.loads(pathlib.Path('rust-toolchain.toml').read_text())['toolchain']['channel'])")
  [ "$have" = "$RUST_TC" ] \
    || bad "rust-toolchain.toml channel '$have' != pins toolchain.rust '$RUST_TC'"
fi

# ── 2. go.mod must require the pinned client-go, exactly ────────────────────
# Only once the Go module exists (it arrives with the Go runner, Phase 2).
if [ -f go/go.mod ]; then
  have=$(awk -v m="$GO_MOD" '$1==m {print $2; exit}' go/go.mod || true)
  [ "$have" = "$GO_VER" ] \
    || bad "go/go.mod requires $GO_MOD '$have' != pins client_go.version '$GO_VER'"
else
  note "go/go.mod not present yet — skipping (arrives with the Go runner)"
fi

# ── 3. The client-rust pin MUST be an ancestor of upstream/master ────────────
# Without this, the harness could quietly certify a verdict against fork-only
# work. We read the sibling's existing remote-tracking ref; we never fetch.
if [ -d "$CLIENT_RUST/.git" ]; then
  if git -C "$CLIENT_RUST" rev-parse --verify -q upstream/master >/dev/null; then
    if git -C "$CLIENT_RUST" merge-base --is-ancestor "$RUST_REV" upstream/master; then
      note "client_rust.rev is an ancestor of upstream/master — OK"
    else
      bad "client_rust.rev $RUST_REV is NOT an ancestor of upstream/master.
       The pin names a commit that is not upstream, so any gap stated against it
       is unfileable and any fix has nowhere to land. Re-pin to an upstream commit.
       (If upstream/master is merely stale, refresh it in YOUR OWN clone — never
       fetch inside a checkout another session is using.)"
    fi
  else
    note "no upstream/master ref in $CLIENT_RUST — cannot verify ancestry."
    note "add it with:  git -C $CLIENT_RUST remote add upstream $RUST_UPSTREAM"
  fi
else
  note "$CLIENT_RUST is not a git checkout — skipping ancestry check"
fi

# ── 4. compose must consume the pinned images, not hardcode a tag ────────────
if grep -qE '^\s*image:\s*pingcap/' cluster/docker-compose.yml 2>/dev/null; then
  bad "cluster/docker-compose.yml hardcodes an image tag.
       It must interpolate \${PD_IMAGE} / \${TIKV_IMAGE} from cluster/images.env,
       which scripts/cluster-env.sh generates from pins.toml. A floating tag is
       mutable, so a verdict signed against it is not reproducible."
fi

[ "$fail" -eq 0 ] && { echo "pins OK"; exit 0; }
exit 1
