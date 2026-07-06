# The M4 evaluation gate for tikv-client (client-rust). `make gate` is the
# one-shot: stand up the throwaway cluster and run everything against it.

export RUSTFLAGS = -Dwarnings
export PD_ADDRS ?= 127.0.0.1:2379

COMPOSE := docker compose -f cluster/docker-compose.yml

.PHONY: default check unit-test gate-test gate cluster-up cluster-down cluster-logs clean

default: check

check:
	cargo fmt -- --check
	cargo clippy --all-targets --features integration-tests -- -D warnings
	cargo check --all-targets --features integration-tests

# The cluster-free tests: prefix arithmetic, conflict classification, and the
# compile-time Send/object-safety obligations (checked by building at all).
unit-test:
	cargo test --lib

# The gate proper — needs a reachable cluster ($PD_ADDRS). `--show-output`
# surfaces each test's captured "observed shape" evidence even when it passes,
# so the one-shot run records the raw error shapes the findings cite.
gate-test:
	cargo test --features integration-tests --test gate -- --show-output

# The failpoint proof of finding 2 (pessimistic rollback leaves prewrite locks).
# Separate binary, single-threaded: the `after-prewrite` failpoint is
# process-global, so it must not run alongside `gate`'s parallel commits.
failpoint-test:
	cargo test --features integration-tests --test failpoint_gate -- --show-output --test-threads=1

# One-shot: cluster up, wait ready, run the full gate. Leaves the cluster
# running for iteration; `make cluster-down` tears it down (and its data).
gate: cluster-up unit-test gate-test

cluster-up:
	$(COMPOSE) up -d
	./cluster/wait-ready.sh

cluster-down:
	$(COMPOSE) down -v

cluster-logs:
	$(COMPOSE) logs --tail 100

clean:
	cargo clean
