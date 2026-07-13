# The TiKV client parity harness. `make gate` is the one-shot: stand up the
# throwaway cluster and run everything against it.
#
# Every run stamps results/provenance.json with the revision of the crate under
# test that was ACTUALLY exercised. Set PARITY_STRICT=1 (CI does) to refuse to
# run at all when that is not the revision pins.toml names.

export RUSTFLAGS = -Dwarnings
export PD_ADDRS ?= 127.0.0.1:2379

# PARITY_STRICT=1 turns an off-pin run from a warning into a hard stop.
# Default 0 so local iteration against a work-in-progress client-rust branch
# still works — that is the whole reason the dependency is a path dep.
export PARITY_STRICT ?= 0

COMPOSE := docker compose --env-file cluster/images.env -f cluster/docker-compose.yml

.PHONY: default check pins-check provenance unit-test gate-test verdict failpoint-test gate \
        cluster-up cluster-down cluster-logs clean

default: check

# --all-features would switch on `failpoints` for the whole workspace, compiling
# fault injection into every member. Name the features instead.
CARGO_FEATURES := --features wyrd-gate/integration-tests,wyrd-gate/failpoints

check: pins-check
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets $(CARGO_FEATURES) -- -D warnings
	cargo check --workspace --all-targets $(CARGO_FEATURES)

# Static, cluster-free, sub-second: pins.toml agrees with rust-toolchain.toml,
# go.mod and compose — and the client-rust pin is an ancestor of upstream/master,
# so a gap stated against it is actually fileable upstream.
pins-check:
	./scripts/check-pins.sh

# What am I about to test? Writes results/provenance.json; aborts under
# PARITY_STRICT if the crate under test is off-pin.
provenance:
	./scripts/provenance.sh

# The cluster-free tests: prefix arithmetic, conflict classification, and the
# compile-time Send/object-safety obligations (checked by building at all).
unit-test:
	cargo test --workspace --lib

# The gate proper — needs a reachable cluster ($PD_ADDRS). `--show-output`
# surfaces each test's captured "observed shape" evidence even when it passes,
# so the one-shot run records the raw error shapes the findings cite.
gate-test: provenance
	cargo test -p wyrd-gate --features integration-tests --test gate -- --show-output

# Both suites, checked against the verdict EXPECTED at the pinned revision:
# everything green except d6 and d7, which MUST be red. Each asserts the correct
# behavior of a gap that is still open upstream (#519 -> PR 544, #545 -> PR 547),
# so each turns green only when its fix lands.
#
# A raw `gate-test` / `failpoint-test` exits non-zero at the pinned revision —
# those two are supposed to fail — which makes plain pass/fail useless as a
# signal for this repo. This target encodes the expectation instead, and fails
# loudly if either ever PASSES: that means the gap closed upstream and the pin,
# the README verdict, and the ledger are all stale.
#
# This is the target CI runs. It subsumes gate-test and failpoint-test, which
# remain available for raw, unfiltered runs while iterating.
verdict: provenance
	./scripts/gate-verdict.sh

# The failpoint proof of finding 2 (pessimistic rollback leaves prewrite locks).
# Separate binary, single-threaded: the `after-prewrite` failpoint is
# process-global, so it must not run alongside `gate`'s parallel commits.
failpoint-test: provenance
	cargo test -p wyrd-gate --features integration-tests,failpoints --test failpoint_gate -- \
	    --show-output --test-threads=1

# One-shot: cluster up, wait ready, run everything against it. Leaves the
# cluster running for iteration; `make cluster-down` tears it down (and its data).
#
# `verdict` covers the failpoint suite too. That matters: `gate` used to omit it
# entirely, so the documented one-shot never ran d7 — the ONLY test that
# empirically reproduces finding 2. Make runs prerequisites sequentially, and
# gate-verdict.sh runs the failpoint binary with --test-threads=1, so the
# process-global `fail` registry is never shared with the gate's parallel commits.
gate: cluster-up unit-test verdict

cluster-up: cluster/images.env
	$(COMPOSE) up -d
	./cluster/wait-ready.sh

# Generated, gitignored: the digest-pinned images, from pins.toml.
cluster/images.env: pins.toml scripts/cluster-env.sh
	./scripts/cluster-env.sh > $@

# These depend on images.env too. Every compose invocation passes
# `--env-file cluster/images.env`, and the file is generated + gitignored — so on a
# fresh checkout, or right after `make clean` removed it, compose would fail before
# it could tear anything down. Leaving a running cluster with no way to stop it is
# a poor trap to set. The dep regenerates it; it is cheap and idempotent.
cluster-down: cluster/images.env
	$(COMPOSE) down -v

cluster-logs: cluster/images.env
	$(COMPOSE) logs --tail 100

clean:
	cargo clean
	rm -rf results cluster/images.env
