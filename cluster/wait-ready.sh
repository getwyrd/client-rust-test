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

echo "waiting for a TiKV store to register Up ..."
until curl -sf "http://${PD}/pd/api/v1/stores" 2>/dev/null | grep -q '"state_name": *"Up"'; do
    if ((SECONDS > DEADLINE)); then
        echo "no TiKV store reached Up within 120s" >&2
        curl -sf "http://${PD}/pd/api/v1/stores" || true
        exit 1
    fi
    sleep 1
done

echo "cluster ready: PD ${PD}, TiKV store Up"
