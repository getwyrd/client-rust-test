# The TiKV client parity harness. `make gate` is the one-shot: stand up the
# throwaway cluster and run everything against it.
#
# Every run stamps results/provenance.json with the revision of the crate under
# test that was ACTUALLY exercised. Set PARITY_STRICT=1 (CI does) to refuse to
# run at all when that is not the revision pins.toml names.

export RUSTFLAGS = -Dwarnings
export PD_ADDRS ?= 127.0.0.1:2379

# THE ORACLE IS THE PINNED MODULE, NEVER A SIBLING CHECKOUT.
#
# pins.toml: "an oracle you can accidentally edit is not an oracle." GOWORK=off means a
# stray go.work in the tree cannot silently swap the pinned, content-addressed client-go
# for a local one you can change. -mod=readonly means a build can never quietly rewrite
# go.mod to resolve something new. The Go driver ALSO reports `replaced` in its hello, so
# ledger-check refuses such a run even if these were somehow bypassed — three layers,
# because a compromised oracle invalidates every claim in the ledger without looking wrong.
export GOWORK ?= off
export GOFLAGS ?= -mod=readonly
GO ?= go

# PARITY_STRICT=1 turns an off-pin run from a warning into a hard stop.
# Default 0 so local iteration against a work-in-progress client-rust branch
# still works — that is the whole reason the dependency is a path dep.
export PARITY_STRICT ?= 0

COMPOSE := docker compose --env-file cluster/images.env -f cluster/docker-compose.yml

.PHONY: default check go-check pins-check provenance unit-test gate-test verdict failpoint-test gate \
        drivers parity ledger cluster-up cluster-down cluster-logs clean

default: check

# --all-features would switch on `failpoints` for the whole workspace, compiling
# fault injection into every member. Name the features instead.
CARGO_FEATURES := --features wyrd-gate/integration-tests,wyrd-gate/failpoints

check: pins-check go-check
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets $(CARGO_FEATURES) -- -D warnings
	cargo check --workspace --all-targets $(CARGO_FEATURES)

# The Go side of the harness. `go mod verify` is what turns pins.toml's claim —
# "content-addressed via go.sum, deterministic by construction" — from a comment into
# an assertion: it rechecks every module's hash against go.sum.
go-check:
	cd go && $(GO) vet ./...
	cd go && $(GO) mod verify
	cd go && test -z "$$(gofmt -l .)" || { echo "gofmt: files need formatting:"; gofmt -l go; exit 1; }

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

# ─── THE PARITY MECHANISM ────────────────────────────────────────────────────
# The drivers. Each links ONE client and speaks the parity command protocol; the
# runner links NEITHER, so the thing deciding a verdict cannot reach for the crate
# it is adjudicating.
drivers:
	cargo build -p rust-driver -p parity-runner
	cd go && $(GO) build -o ../target/parity-driver-go ./driver

# Run every scenario against both clients and REPORT the diff. Deliberately does not
# decide whether the diff was expected — that is the ledger's job. Keeping "what
# happened" separate from "was that what we predicted" is what lets one be checked
# against the other.
parity: drivers provenance
	./target/debug/parity-runner

# The verdict for the parity claims: XDIVERGE / XCONVERGE / WRONG DIVERGENCE.
# Refuses to settle anything from a run whose provenance says strict:false.
ledger: parity
	./scripts/ledger-check.sh

# One-shot: cluster up, wait ready, run everything against it. Leaves the
# cluster running for iteration; `make cluster-down` tears it down (and its data).
#
# `verdict` covers the failpoint suite too. That matters: `gate` used to omit it
# entirely, so the documented one-shot never ran d7 — the ONLY test that
# empirically reproduces finding 2. Make runs prerequisites sequentially, and
# gate-verdict.sh runs the failpoint binary with --test-threads=1, so the
# process-global `fail` registry is never shared with the gate's parallel commits.
#
# TWO VERDICT MECHANISMS, AND THAT IS CORRECT FOR NOW. `verdict` adjudicates the wyrd
# M4 contract (an application-contract suite, with no Go counterpart — client-go has no
# MetadataStore, so diffing it against the oracle would be meaningless). `ledger`
# adjudicates parity between the two clients. They answer different questions; the
# gate's XFAIL rows fold into the ledger only once it grows a test-signature evidence
# kind, which is later work.
gate: cluster-up unit-test verdict ledger

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
