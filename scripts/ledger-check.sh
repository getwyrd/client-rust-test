#!/usr/bin/env bash
# THE PARITY LEDGER CHECK. Proves ledger.toml against a pinned differential run.
#
# This is scripts/gate-verdict.sh, generalized. gate-verdict.sh hard-codes two XFAIL rows
# and one kind of evidence (a cargo test's exit status plus a substring of its stdout).
# It calls itself, accurately, "a deliberately small stand-in for the parity ledger,
# which generalizes it to every gap with a declared expectation and machine-checked
# evidence." This is that ledger.
#
# The three rules are the same three rules, lifted from "a substring in stdout" to "a
# field in a trace":
#
#   XDIVERGE          declared `diverges`, and it diverged EXACTLY as declared.
#   XCONVERGE         declared `diverges`, but the clients AGREED  -> HARD FAILURE.
#   WRONG DIVERGENCE  declared `diverges`, but differently         -> HARD FAILURE.
#   NEW GAP           declared `agrees`, but they diverged         -> HARD FAILURE.
#
# XCONVERGE is the important one. Without it the harness would quietly rot into "these
# are red forever" and nobody would notice the day a fix landed upstream.
#
# WRONG DIVERGENCE is the subtle one. "They differ" is NOT evidence that THIS gap is
# still open — the run could differ for an unrelated reason, or because the harness
# broke. Accepting any difference as proof would let a broken harness masquerade as a
# confirmed finding, which is the same false-green this whole mechanism exists to prevent.
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

LEDGER="${LEDGER:-ledger.toml}"
PINS="${PINS:-pins.toml}"
rc=0

# ── 1. ADMISSIBILITY ─────────────────────────────────────────────────────────────────
# Checked BEFORE any verdict. A claim settled off-pin is not settled.
#
# This finally keeps the promise scripts/provenance.sh has been making all along:
# "Downstream, `ledger-check` refuses any result whose provenance says strict:false."
./scripts/provenance.sh results/provenance.json || exit 2

python3 - "$PINS" <<'PY' || exit 2
import json, pathlib, sys, tomllib

prov_path = pathlib.Path("results/provenance.json")
if not prov_path.exists():
    sys.exit("REFUSING: no results/provenance.json — run `make provenance` first.")

p = json.loads(prov_path.read_text())
pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())

def refuse(why):
    sys.stderr.write(f"""
REFUSING TO SETTLE THE LEDGER: {why}

A ledger claim can only ever be settled by a PINNED run. A result produced under other
conditions would look identical to a pinned one and MEAN something different, so it is
not evidence and must not be published.

Run under PARITY_STRICT=1 against the pinned world, or iterate locally without settling
the ledger (`make parity` alone reports the diff without claiming to prove anything).
""")
    sys.exit(2)

if p.get("schema") != "parity-provenance/v1":
    refuse("unrecognized provenance schema")
if p.get("strict") is not True:
    refuse("provenance says strict:false — this run is not admissible as evidence")

cr = p["client_rust"]
if cr.get("matches_pin") is not True:
    refuse(f"the crate under test is off-pin (actual {cr.get('rev')}, pinned {cr.get('pinned_rev')})")
if cr.get("dirty") is not False:
    refuse("the crate under test is DIRTY — its revision does not describe its contents")

cg = p["client_go"]
if cg.get("replaced") is True:
    refuse("client-go is REPLACED — the oracle is a local tree you can edit, not the pinned module")
if cg.get("matches_pin") is not True:
    refuse(f"client-go resolved to {cg.get('version')}, but the pin names {cg.get('pinned_version')}")

# The SERVER is half of every behavioural claim. Lock resolution, prewrite residue and
# conflict shapes are server behaviour as much as client behaviour, so a run against an
# unidentified or off-pin TiKV certifies nothing — however pinned the two clients were.
cl = p["cluster"]
if cl.get("verified") is not True:
    refuse(f"the cluster at {cl.get('pd_addr')} could not be identified (no PD reachable)")
if cl.get("matches_pin") is not True:
    refuse(
        f"the cluster is PD {cl.get('observed_pd_version')} / TiKV [{cl.get('observed_tikv_versions')}], "
        f"but the pin names {cl.get('pinned_version')}"
    )
PY

# ── 2. THE ORACLE MUST BE THE PINNED ORACLE, AS THE BINARY ITSELF REPORTS IT ─────────
# Not as a file describing it claims. Every trace carries each driver's `hello`, which
# the Go driver fills from runtime/debug.ReadBuildInfo() — including whether the module
# was `replace`d. pins.toml says "an oracle you can accidentally edit is not an oracle";
# this is where that stops being a comment.
python3 - "$PINS" <<'PY' || exit 2
import json, glob, pathlib, sys, tomllib
pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
want = pins["client_go"]["version"]
for f in sorted(glob.glob("results/traces/*.json")):
    t = json.loads(pathlib.Path(f).read_text())
    for rb in t.get("roles", []):
        c = rb["hello"]["client"]
        if rb["driver"] != "go":
            continue
        if c.get("replaced"):
            sys.exit(f"REFUSING: {f}: the go driver ran against a REPLACED client-go ({c['version']}).")
        if c["version"] != want:
            sys.exit(f"REFUSING: {f}: the go driver linked client-go {c['version']}, but the pin names {want}.")
PY

# ── 3. THE VERDICT ───────────────────────────────────────────────────────────────────
python3 - "$LEDGER" <<'PY'
import json, pathlib, sys, tomllib

ledger = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
rc = 0

for gap in ledger.get("gap", []):
    gid, scen = gap["id"], gap["scenario"]
    name = pathlib.Path(scen).stem
    dpath = pathlib.Path(f"results/divergence.{name}.json")

    print(f"\n═══ {gid} — {gap['title']}")
    print(f"    scenario: {scen}   expect: {gap['expect']}   status: {gap['status']}")

    if not dpath.exists():
        print(f"\nMISSING — no result for `{name}`. An absence is never an answer.", file=sys.stderr)
        rc = 1
        continue

    observed = json.loads(dpath.read_text())["divergences"]
    # Compare as SETS of (step, path, oracle, subject). Order is not a fact.
    obs = {(d["step"], d["path"], d["oracle"], d["subject"]) for d in observed}

    if gap["expect"] == "agrees":
        if not obs:
            print("    HOLDS — the two clients agree on every compared field.")
        else:
            print(f"""
NEW GAP — {gid} is declared to AGREE, and it did not.

An UNDECLARED difference between the two clients. That is exactly what this harness
exists to find, and it is a hard failure so that it cannot be ignored: either file it as
a finding and declare it here, or fix it.
""", file=sys.stderr)
            for d in sorted(obs):
                print(f"    {d[0]} · {d[1]}\n        oracle : {d[2]}\n        subject: {d[3]}", file=sys.stderr)
            rc = 1
        continue

    # expect == "diverges"
    declared = {(d["step"], d["path"], d["oracle"], d["subject"]) for d in gap.get("divergence", [])}

    if not obs:
        print(f"""
XCONVERGE — {gid} is declared to DIVERGE, and the two clients AGREED.

The gap appears to be CLOSED at the pinned revision. That is good news, and it is a hard
failure on purpose: the pin, the README's verdict, and this ledger entry are now stale.

  1. confirm the fix merged upstream ({gap.get('upstream_pr', 'the upstream PR')})
  2. bump pins.toml client_rust.rev to a commit that contains it
  3. flip this entry to status = "FIXED" and expect = "agrees" — it becomes a permanent
     REGRESSION GUARD. Do not delete it: the day the fix regresses, this is what catches it.
""", file=sys.stderr)
        rc = 1
        continue

    missing = declared - obs
    extra   = obs - declared

    if not missing and not extra:
        print(f"    XDIVERGE — the gap is still open, and it diverged EXACTLY as declared "
              f"({len(declared)} field(s)).")
        for d in sorted(declared):
            print(f"      {d[0]} · {d[1]}: oracle={d[2]!r} subject={d[3]!r}")
        continue

    print(f"""
WRONG DIVERGENCE — {gid} diverged, but NOT in the way it is declared to.

"They differ" is not evidence that THIS gap is still open. The run may have diverged for
an unrelated reason, or the harness may be broken. Treating any difference as proof is
how a broken harness masquerades as a confirmed finding — so this is a hard failure, and
the ledger must be re-stated to say what is actually true.
""", file=sys.stderr)
    for d in sorted(missing):
        print(f"    DECLARED BUT NOT OBSERVED: {d[0]} · {d[1]}\n"
              f"        expected oracle={d[2]!r} subject={d[3]!r}", file=sys.stderr)
    for d in sorted(extra):
        print(f"    OBSERVED BUT NOT DECLARED: {d[0]} · {d[1]}\n"
              f"        oracle={d[2]!r} subject={d[3]!r}", file=sys.stderr)
    rc = 1

print()
if rc == 0:
    print("LEDGER: every claim holds at the pinned revisions.")
else:
    print("LEDGER: the claims do NOT match what was observed.", file=sys.stderr)
sys.exit(rc)
PY
rc=$?

exit "$rc"
