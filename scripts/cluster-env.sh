#!/usr/bin/env bash
# Generate cluster/images.env (gitignored) from pins.toml.
#
# Compose interpolates ${PD_IMAGE}/${TIKV_IMAGE} rather than hardcoding a tag, so
# the images have exactly one source of truth. The Makefile passes --env-file
# EXPLICITLY: Compose v2's implicit .env lookup is relative to the invocation
# directory, not the compose file's, so relying on it breaks the moment anyone
# runs make from a subdirectory.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
PINS="${PINS:-pins.toml}"

pin() { python3 -c "
import tomllib,pathlib
d=tomllib.loads(pathlib.Path('$PINS').read_text())
cur=d
key='$1'
if key.startswith('cluster.versions.'):
    rest=key[len('cluster.versions.'):]
    ver,field=rest.rsplit('.',1)
    cur=d['cluster']['versions'][ver][field]
else:
    for k in key.split('.'):
        cur=cur[k]
print(cur)"; }

# Optional argument: a TiKV version from the matrix ([cluster.versions] in pins.toml).
# Absent, or equal to [cluster].tikv_version, means the default pin.
VERSION="${1:-}"
DEFAULT="$(pin cluster.tikv_version)"
if [ -z "$VERSION" ] || [ "$VERSION" = "$DEFAULT" ]; then
  PD_IMAGE="$(pin cluster.pd_image)"
  TIKV_IMAGE="$(pin cluster.tikv_image)"
  VERSION="$DEFAULT"
else
  PD_IMAGE="$(pin "cluster.versions.$VERSION.pd_image" 2>/dev/null)" || {
    echo "cluster-env.sh: TiKV version '$VERSION' is not pinned in pins.toml [cluster.versions]" >&2
    exit 2
  }
  TIKV_IMAGE="$(pin "cluster.versions.$VERSION.tikv_image")"
fi

cat <<EOF
# GENERATED from pins.toml by scripts/cluster-env.sh — do not edit, do not commit.
TIKV_VERSION=$VERSION
PD_IMAGE=$PD_IMAGE
TIKV_IMAGE=$TIKV_IMAGE
EOF
