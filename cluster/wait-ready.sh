#!/usr/bin/env bash
# Wait until PD reports the expected number of TiKV stores as Up (the point at which
# the client can place data). Mirrors the readiness poll in client-rust's CI.
#
# Usage: wait-ready.sh [want]
#   want           how many stores must be Up (default 1; the 3-store profile passes 3)
#   $COMPOSE_FILE  which compose file the death-check inspects
#                  (default cluster/docker-compose.yml; the 3-store target overrides)
set -euo pipefail

WANT="${1:-1}"
PD="${PD_ADDRS:-127.0.0.1:2379}"
PD="${PD%%,*}" # first address is enough for the API poll
DEADLINE=$((SECONDS + 120))

echo "waiting for PD at ${PD} ..."
until curl -sf "http://${PD}/pd/api/v1/version" >/dev/null 2>&1; do
    if ((SECONDS > DEADLINE)); then
        echo "PD did not come up within 120s" >&2
        exit 1
    fi
    sleep 1
done

# A TiKV that died at startup will never register, so polling PD for 120s only
# reports the symptom ("no store reached Up") and hides the cause. If we are
# driving the compose cluster, notice a dead container immediately and print what
# it actually said. (Skipped when pointing $PD_ADDRS at some other cluster.)
COMPOSE="docker compose --env-file cluster/images.env -f ${COMPOSE_FILE:-cluster/docker-compose.yml}"
tikv_died() {
    command -v docker >/dev/null 2>&1 || return 1
    # `ps` without -a lists only RUNNING containers, so a container that died
    # reports an empty state — the exact case we are trying to detect. -a is
    # what makes this work at all. Any service named tikv* counts: the 3-store
    # profile runs tikv0/tikv1/tikv2.
    $COMPOSE ps -a --format '{{.Service}} {{.State}}' 2>/dev/null |
        awk '$1 ~ /^tikv/ && $2 != "running" { found = 1 } END { exit !found }'
}

up_count() {
    curl -sf "http://${PD}/pd/api/v1/stores" 2>/dev/null |
        grep -c '"state_name": *"Up"' || true
}

echo "waiting for ${WANT} TiKV store(s) to register Up ..."
until [ "$(up_count)" -ge "$WANT" ]; do
    if tikv_died; then
        echo "a TiKV container exited before registering with PD:" >&2
        $COMPOSE logs --no-color --tail 20 >&2 || true
        exit 1
    fi
    if ((SECONDS > DEADLINE)); then
        echo "fewer than ${WANT} TiKV store(s) reached Up within 120s" >&2
        curl -sf "http://${PD}/pd/api/v1/stores" || true
        $COMPOSE logs --no-color --tail 20 >&2 || true
        exit 1
    fi
    sleep 1
done

echo "cluster ready: PD ${PD}, ${WANT} TiKV store(s) Up"
