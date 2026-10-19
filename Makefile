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

# PARITY_MODE=candidate tests a CHANGE against the pinned world (scripts/provenance.sh):
# `PARITY_MODE=candidate PARITY_GAP=<id> make ledger` reports <id>'s outcome in
# results/candidate-verdict.json and fails if any other claim breaks. It is never evidence.

.PHONY: default check go-check pins-check provenance unit-test gate-test verdict failpoint-test gate \
        drivers parity ledger cluster-up cluster-down cluster-logs cluster-up-3store \
        cluster-down-3store cluster-up-upstream cluster-down-upstream broker-test clean \
        clear-candidate-verdict

default: check

# --all-features would switch on `failpoints` for the whole workspace, compiling
# fault injection into every member. Name the features instead.
CARGO_FEATURES := --features wyrd-gate/integration-tests,wyrd-gate/failpoints

check: pins-check go-check broker-test
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
# go.mod and compose — and the client-rust pin is on the fork's integration branch,
# which sits on upstream history, so a gap stated against it is fileable upstream.
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

# Both suites, checked against the verdict EXPECTED at the pinned revision: every test
# green except those declared expected-red in scripts/gate-verdict.sh's XFAIL table.
# At the current pin that table is EMPTY: d6 (#519, fixed by tikv/client-rust#561) and d7
# (#545, fixed by #547) were red at e53837d and now guard against a regression.
#
# While the table holds a row, a raw `gate-test` / `failpoint-test` exits non-zero at
# the pinned revision — that test is supposed to fail — which makes plain pass/fail
# useless as a signal for this repo. This target encodes the expectation instead, and fails
# loudly if either ever PASSES: that means the gap closed at the pin and the
# README verdict and the ledger are stale.
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
#
# DRIVER_FEATURES=--features=rust-driver/pre-557 builds the Rust driver against a
# client-rust without #557 — CI's informational run at pins.toml upstream_base (ADR-0001).
DRIVER_FEATURES ?=
drivers:
	cargo build -p rust-driver -p parity-runner $(DRIVER_FEATURES)
	cd go && $(GO) build -o ../target/parity-driver-go ./driver

# Run every scenario against both clients and REPORT the diff. Deliberately does not
# decide whether the diff was expected — that is the ledger's job. Keeping "what
# happened" separate from "was that what we predicted" is what lets one be checked
# against the other.
parity: clear-candidate-verdict drivers provenance
	./target/debug/parity-runner

# First, before anything can fail: a caller reads results/candidate-verdict.json even
# after a non-zero exit (ledger-check writes it when another gap breaks), so a verdict
# from an earlier run must never survive a run that failed before reaching ledger-check.
clear-candidate-verdict:
	rm -f results/candidate-verdict.json

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

# The cluster targets go through scripts/cluster-broker, which owns every cluster's
# lifecycle (client-rust-loop-design WP-03). A human or CI cluster is the lease
# `manual-single` (or `manual-3store`) on slot 0 — the classic ports, so $PD_ADDRS keeps
# its default — with no expiry. Concurrent runs lease their own slot:
#   ./scripts/cluster-broker lease single --ttl 2h   # prints {"pd_addrs": ...}
BROKER := ./scripts/cluster-broker

cluster-up:
	$(BROKER) lease single --slot 0 --ttl 0 --id manual-single --owner make

# Generated, gitignored: the default pin's images, from pins.toml — for running compose
# by hand. The broker writes its own per-lease env file (images + ports) instead, and
# tears a lease down from that file, so `cluster-down` works on a fresh checkout too.
cluster/images.env: pins.toml scripts/cluster-env.sh
	./scripts/cluster-env.sh > $@

cluster-down:
	$(BROKER) release manual-single --missing-ok

cluster-logs:
	$(BROKER) logs manual-single

# ── The 3-store profile ────────────────────────────────────────────────────────────
# Three TiKV stores, three replicas per region (cluster/pd-3store.toml) — the harness
# prerequisite for replica/stale-read scenarios (docs/parity-roadmap.md §5.4/§8).
# These `make` targets share slot 0 with the single-node ones, so they still exclude each
# other (`make cluster-down` first); broker leases on other slots run side by side. No scenario uses this profile yet; it exists so 5.4's scenarios have somewhere
# to run, and so its bring-up is verified before it is needed.
cluster-up-3store:
	$(BROKER) lease 3store --slot 0 --ttl 0 --id manual-3store --owner make

cluster-down-3store:
	$(BROKER) release manual-3store --missing-ok

# ── The upstream profile ───────────────────────────────────────────────────────────
# client-rust's own CI cluster (three stores, its config/tikv.toml + config/pd.toml), for
# running client-rust's integration suites against a container. Slot 0 like the others;
# set CLIENT_RUST_CONFIG to the checkout under test (default ../client-rust/config).
cluster-up-upstream:
	$(BROKER) lease upstream --slot 0 --ttl 0 --id manual-upstream --owner make

cluster-down-upstream:
	$(BROKER) release manual-upstream --missing-ok

# The script tests: the broker (Docker and the readiness poll stubbed) and candidate
# mode (over recorded results). No cluster, so they run anywhere.
broker-test:
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests

clean:
	cargo clean
	rm -rf results cluster/images.env
