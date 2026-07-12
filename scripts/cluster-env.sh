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
for k in '$1'.split('.'):
    cur=cur[k]
print(cur)"; }

cat <<EOF
# GENERATED from pins.toml by scripts/cluster-env.sh — do not edit, do not commit.
PD_IMAGE=$(pin cluster.pd_image)
TIKV_IMAGE=$(pin cluster.tikv_image)
EOF
