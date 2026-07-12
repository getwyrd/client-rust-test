#!/usr/bin/env bash
# Wait until PD reports the TiKV store as Up (the point at which the client
# can place data). Mirrors the readiness poll in client-rust's CI.
set -euo pipefail

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
COMPOSE="docker compose --env-file cluster/images.env -f cluster/docker-compose.yml"
tikv_died() {
    command -v docker >/dev/null 2>&1 || return 1
    local state
    # `ps` without -a lists only RUNNING containers, so a container that died
    # reports an empty state — the exact case we are trying to detect. -a is
    # what makes this work at all.
    state=$($COMPOSE ps -a --format '{{.State}}' tikv 2>/dev/null) || return 1
    [ -n "$state" ] && [ "$state" != "running" ]
}

echo "waiting for a TiKV store to register Up ..."
until curl -sf "http://${PD}/pd/api/v1/stores" 2>/dev/null | grep -q '"state_name": *"Up"'; do
    if tikv_died; then
        echo "the TiKV container exited before registering with PD:" >&2
        $COMPOSE logs --no-color --tail 20 tikv >&2 || true
        exit 1
    fi
    if ((SECONDS > DEADLINE)); then
        echo "no TiKV store reached Up within 120s" >&2
        curl -sf "http://${PD}/pd/api/v1/stores" || true
        $COMPOSE logs --no-color --tail 20 tikv >&2 || true
        exit 1
    fi
    sleep 1
done

echo "cluster ready: PD ${PD}, TiKV store Up"
