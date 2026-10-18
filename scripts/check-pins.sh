#!/usr/bin/env bash
# Assert that every consumer of a pinned value agrees with pins.toml, and that
# the client-rust pin is on the fork's integration branch, which itself sits on
# upstream history (ADR-0001).
#
# The ancestry checks matter most. A pin built on history upstream lacks can
# never be a baseline for a gap report: the fix would have nowhere to land. A pin
# off the integration branch is work nobody reviewed into the subject. Both are
# cheap, static checks that make those mistakes impossible.
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
RUST_FORK=$(pin client_rust.fork)
RUST_BRANCH=$(pin client_rust.branch)
RUST_BASE=$(pin client_rust.upstream_base)
GO_MOD=$(pin client_go.module)
GO_VER=$(pin client_go.version)
GO_REV=$(pin client_go.rev)
RUST_TC=$(pin toolchain.rust)
GO_TC=$(pin toolchain.go)
note "client_rust.rev  = $RUST_REV ($RUST_BRANCH on $RUST_FORK)"
note "upstream_base    = $RUST_BASE"
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

# ── 3. The client-rust pin is on the fork's branch, and never leaves upstream ──
# ADR-0001: the subject is the fork's `integration` branch. Two ancestry checks keep
# that honest:
#   3a. rev is on the FIRST-PARENT chain of <fork>/<branch> — the pin is a state the
#       branch itself has been at, not a stray fork commit, and not a commit of a PR
#       branch that was later merged in (ancestry alone would accept those: a #557-only
#       tree was never an integration state);
#   3b. upstream_base is EXACTLY where rev meets <upstream>/master: the newest commit on
#       master's first-parent chain that rev contains — the branch was built on real
#       upstream history, so every gap can still be stated
#       (and every fix filed) against tikv/client-rust, and CI's upstream view compares
#       against the upstream commit the subject really sits on. Ancestry alone would
#       let a stale upstream_base pass after integration merged a newer master.
#
# Remotes are resolved by URL, never by name: locally the sibling has origin=fork +
# upstream=tikv, while CI clones the fork as origin and adds tikv by hand. Under
# PARITY_STRICT (CI) "cannot verify" is a FAILURE, not a shrug: an unverifiable
# invariant is not a satisfied one.
#
# Read-only: we never fetch inside the checkout (another session may be using it).
STRICT="${PARITY_STRICT:-0}"
norm_url() { sed -E 's#\.git$##; s#/$##; s#^git@github\.com:#https://github.com/#' <<<"$1"; }

cannot_verify() {
  if [ "$STRICT" = "1" ]; then
    bad "cannot verify the client-rust pin's ancestry ($1).
       Under PARITY_STRICT this is a failure, not a skip: a run that cannot check
       the invariant cannot be evidence for it. Ensure the client-rust checkout has
       remotes pointing at $RUST_FORK (with $RUST_BRANCH fetched) and at
       $RUST_UPSTREAM (with master fetched) — in CI: fetch-depth: 0 plus the
       upstream fetch step."
  else
    note "cannot verify ancestry ($1) — advisory only; CI enforces this."
  fi
}

# The name of the remote whose fetch URL is $1, or nothing.
remote_for() {
  local want name url
  want=$(norm_url "$1")
  while read -r name url; do
    [ "$(norm_url "$url")" = "$want" ] && { echo "$name"; return; }
  done < <(git -C "$CLIENT_RUST" remote -v | awk '$3=="(fetch)"{print $1, $2}')
  return 0   # no match is an empty answer, not a failure: set -e must not abort here
}

# True when ref $1 is fetched; otherwise says it cannot verify. Called directly, never
# in $(...): cannot_verify must set `fail` in THIS shell.
fetched() {
  git -C "$CLIENT_RUST" rev-parse --verify -q "$1" >/dev/null && return 0
  cannot_verify "$1 is not fetched (shallow clone?)"
  return 1
}

if [ -d "$CLIENT_RUST/.git" ]; then
  fork_remote=$(remote_for "$RUST_FORK")
  upstream_remote=$(remote_for "$RUST_UPSTREAM")
  [ -n "$fork_remote" ] || cannot_verify "no remote points at $RUST_FORK"
  [ -n "$upstream_remote" ] || cannot_verify "no remote points at $RUST_UPSTREAM"

  branch_ref="$fork_remote/$RUST_BRANCH"
  if [ -n "$fork_remote" ] && fetched "$branch_ref"; then
    rev_full=$(git -C "$CLIENT_RUST" rev-parse --verify -q "$RUST_REV^{commit}") || rev_full=$RUST_REV
    # grep -c, not -q: -q exits at the first match and pipefail would see rev-list's SIGPIPE.
    if git -C "$CLIENT_RUST" rev-list --first-parent "$branch_ref" | grep -cxF "$rev_full" >/dev/null; then
      note "client_rust.rev is on $branch_ref (first-parent) — OK"
    else
      bad "client_rust.rev $RUST_REV is NOT on the first-parent chain of $branch_ref.
       ADR-0001: the subject is a state of the fork's $RUST_BRANCH branch. A pin anywhere
       else — a PR branch's commit included — is a tree $RUST_BRANCH never was. Merge the
       work into $RUST_BRANCH, then pin the merge."
    fi
  fi

  master_ref="$upstream_remote/master"
  if [ -n "$upstream_remote" ] && fetched "$master_ref"; then
    # The newest commit on master's FIRST-PARENT chain that rev contains — not plain
    # `merge-base`, whose best common ancestor can be a PR tip that upstream merged with a
    # merge commit: a side-branch state master never was. Process substitution, not a
    # pipe, so awk's early exit cannot trip pipefail.
    base=$(awk 'NR == FNR { in_rev[$0]; next } ($0 in in_rev) { print; exit }' \
      <(git -C "$CLIENT_RUST" rev-list "$RUST_REV" 2>/dev/null) \
      <(git -C "$CLIENT_RUST" rev-list --first-parent "$master_ref" 2>/dev/null))
    if [ "$base" = "$RUST_BASE" ]; then
      note "client_rust.upstream_base is where rev meets $master_ref (first-parent) — OK"
    elif [ -z "$base" ]; then
      bad "client_rust.rev $RUST_REV shares no history with $master_ref (or is not in the
       checkout). The fork's branch must be built on upstream history."
    else
      bad "client_rust.upstream_base is $RUST_BASE, but rev meets $master_ref at $base.
       Either $RUST_BRANCH merged a newer upstream master and upstream_base was not moved
       with it, or upstream_base is not on upstream history at all. Set upstream_base to
       $base, as a reviewed change."
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

# ── 5. every matrix version is pinned by tag AND digest ──────────────────────
# A floating tag in [cluster.versions] would make a matrix verdict unreproducible —
# the same reason the default pin carries its digest.
bad_versions=$(python3 -c "
import tomllib,pathlib,re
d=tomllib.loads(pathlib.Path('$PINS').read_text())
for v,row in (d.get('cluster',{}).get('versions') or {}).items():
    for f,repo in (('pd_image','pd'),('tikv_image','tikv')):
        img=row.get(f,'')
        if not re.fullmatch(r'pingcap/'+repo+':'+re.escape(v)+r'@sha256:[0-9a-f]{64}', img):
            print(f'{v}.{f}={img!r}')
")
if [ -n "$bad_versions" ]; then
  bad "pins.toml [cluster.versions]: pd_image must be pingcap/pd:<version>@sha256:<digest> and tikv_image pingcap/tikv:<version>@sha256:<digest>:
$bad_versions"
else
  note "cluster version matrix: $(python3 -c "
import tomllib,pathlib
d=tomllib.loads(pathlib.Path('$PINS').read_text())
print(', '.join([d['cluster']['tikv_version']+' (default)']+sorted((d['cluster'].get('versions') or {}))))")"
fi

[ "$fail" -eq 0 ] && { echo "pins OK"; exit 0; }
exit 1
