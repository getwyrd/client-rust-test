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
CLIENT_GO="${CLIENT_GO:-../client-go}"
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
GO_REV=$(pin client_go.rev)
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
if [ -f go/go.mod ]; then
  have=$(awk -v m="$GO_MOD" '$1==m {print $2; exit}' go/go.mod || true)
  [ "$have" = "$GO_VER" ] \
    || bad "go/go.mod requires $GO_MOD '$have' != pins client_go.version '$GO_VER'"

  # The go directive must match the pinned toolchain, for the same reason
  # rust-toolchain.toml must: a result produced by a different compiler than the
  # one the pin names is not the result the pin describes.
  have_go=$(awk '$1=="go" {print $2; exit}' go/go.mod || true)
  [ "$have_go" = "$GO_TC" ] \
    || bad "go/go.mod 'go $have_go' != pins toolchain.go '$GO_TC'"

  # THE ORACLE MUST NOT BE REPLACEABLE. pins.toml puts it plainly: "an oracle you
  # can accidentally edit is not an oracle." A `replace` swaps the pinned,
  # content-addressed module for a working tree, and every claim settled against
  # it is void. This is the static half; provenance.sh checks the resolved build.
  if grep -qE "^[[:space:]]*replace[[:space:]].*$GO_MOD|^[[:space:]]*$GO_MOD[[:space:]]*=>" go/go.mod; then
    bad "go/go.mod REPLACEs $GO_MOD.
       The oracle must be the pinned pseudo-version, content-addressed via go.sum.
       Use a gitignored go.work to hack on client-go locally; CI sets GOWORK=off."
  fi

  # go.sum is what makes the oracle content-addressed. Without it the version is a
  # name, not a fact.
  [ -f go/go.sum ] || bad "go/go.sum is missing — the oracle is not content-addressed without it."

  # A tracked go.work would defeat GOWORK=off for everyone who cloned.
  if git ls-files --error-unmatch go/go.work >/dev/null 2>&1; then
    bad "go/go.work is TRACKED. It must be gitignored — it is the local escape hatch, not a shared setting."
  fi
else
  note "go/go.mod not present yet — skipping (arrives with the Go runner)"
fi

# ── 2b. The client-go sibling, if present, must BE the pinned rev ────────────
# The harness reads client-go's source to state the oracle half of a claim ("this
# is what client-go does here"). Reading a DIFFERENT client-go than the one the
# ledger names makes that half a guess. Nothing checked this before; the sibling
# happened to be on-pin by luck, which is not a property you can rely on twice.
if [ -d "$CLIENT_GO/.git" ]; then
  have=$(git -C "$CLIENT_GO" rev-parse HEAD)
  if [ "$have" = "$GO_REV" ]; then
    note "client-go sibling is at the pinned rev — OK"
  else
    bad "the client-go sibling ($CLIENT_GO) is at $have, but pins client_go.rev is $GO_REV.
       Every oracle claim quotes this tree. Reading a different client-go than the one
       the ledger names makes the oracle half of every claim unverified."
  fi
else
  note "no client-go sibling checkout at $CLIENT_GO — skipping (the pinned module is what CI builds)"
fi

# ── 3. The client-rust pin MUST be an ancestor of upstream's master ──────────
# Without this, the harness could quietly certify a verdict against fork-only
# work that can never be upstreamed.
#
# The upstream repo is NOT always a remote called "upstream". Locally the sibling
# has origin=fork + upstream=tikv; in CI, actions/checkout clones tikv/client-rust
# as ORIGIN and there is no `upstream` remote at all. Keying off the remote's NAME
# meant CI silently took the "cannot verify" branch and still reported "pins OK" —
# the check never ran where it mattered most. So resolve the remote by URL, and
# under PARITY_STRICT (CI) treat "cannot verify" as a FAILURE, not a shrug: an
# unverifiable invariant is not a satisfied one.
#
# Read-only: we never fetch inside the checkout (another session may be using it).
STRICT="${PARITY_STRICT:-0}"
norm_url() { sed -E 's#\.git$##; s#/$##; s#^git@github\.com:#https://github.com/#' <<<"$1"; }

cannot_verify() {
  if [ "$STRICT" = "1" ]; then
    bad "cannot verify that client_rust.rev is an upstream commit ($1).
       Under PARITY_STRICT this is a failure, not a skip: a run that cannot check
       the invariant cannot be evidence for it. Ensure the client-rust checkout has
       a remote pointing at $RUST_UPSTREAM with its default branch fetched
       (in CI: actions/checkout with fetch-depth: 0)."
  else
    note "cannot verify ancestry ($1) — advisory only; CI enforces this."
  fi
}

if [ -d "$CLIENT_RUST/.git" ]; then
  want=$(norm_url "$RUST_UPSTREAM")
  upstream_remote=""
  while read -r name url; do
    [ "$(norm_url "$url")" = "$want" ] && { upstream_remote="$name"; break; }
  done < <(git -C "$CLIENT_RUST" remote -v | awk '$3=="(fetch)"{print $1, $2}')

  if [ -z "$upstream_remote" ]; then
    cannot_verify "no remote points at $RUST_UPSTREAM"
  else
    ref="$upstream_remote/master"
    if ! git -C "$CLIENT_RUST" rev-parse --verify -q "$ref" >/dev/null; then
      cannot_verify "$ref is not fetched (shallow clone?)"
    elif git -C "$CLIENT_RUST" merge-base --is-ancestor "$RUST_REV" "$ref"; then
      note "client_rust.rev is an ancestor of $ref — OK"
    else
      bad "client_rust.rev $RUST_REV is NOT an ancestor of $ref.
       The pin names a commit that is not upstream, so any gap stated against it is
       unfileable and any fix has nowhere to land. Re-pin to an upstream commit."
    fi
  fi
else
  cannot_verify "$CLIENT_RUST is not a git checkout"
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
