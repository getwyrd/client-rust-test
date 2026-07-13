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
#
# THE PROVENANCE IS READ FROM THE RESULTS, NOT RE-STAMPED HERE. That distinction is the
# whole point and it is easy to get wrong. Re-deriving provenance at adjudication time
# would describe the world as it is NOW, not the world the evidence was gathered in — so
# traces produced against a dirty or off-pin client-rust could be left in results/, the
# checkout restored to the pin, and a strict run of this script would mint fresh,
# admissible-looking provenance and settle the stale evidence with it. The trace would
# never have to lie; the adjudicator would do it for them.
#
# So the runner stamps each artifact with the provenance it ran under, and this reads
# THAT. Evidence carries the conditions it was gathered under, or it is not evidence.
HARNESS_REV="$(git rev-parse HEAD)"
python3 - "$PINS" "$HARNESS_REV" "$LEDGER" <<'PY' || exit 2
import json, pathlib, sys, tomllib

pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
harness_rev = sys.argv[2]
ledger = tomllib.loads(pathlib.Path(sys.argv[3]).read_text())

def refuse(why):
    sys.stderr.write(f"""
REFUSING TO SETTLE THE LEDGER: {why}

A ledger claim can only ever be settled by a PINNED run. A result produced under other
conditions would look identical to a pinned one and MEAN something different, so it is
not evidence and must not be published.

Run `make ledger` (which re-runs the scenarios under fresh provenance) against the
pinned world with PARITY_STRICT=1, or iterate locally without settling the ledger
(`make parity` alone reports the diff without claiming to prove anything).
""")
    sys.exit(2)

# ONLY the results THIS LEDGER adjudicates.
#
# Globbing results/divergence.*.json swept in artifacts from scenarios that have since
# been renamed or deleted. results/ is gitignored, so those files linger — and being
# stale they carry an older harness revision, which meant one obsolete artifact would
# REFUSE every subsequent run, however fresh and valid the real results were. The
# admissibility rule would have been technically right and practically useless.
#
# A ledger entry names its scenario; the scenario names its artifact. Judge those.
results = []
for gap in ledger.get("gap", []):
    name = json.loads(pathlib.Path(gap["scenario"]).read_text())["name"]
    results.append(f"results/divergence.{name}.json")

if not results:
    refuse("the ledger declares no gaps — there is nothing to settle")

for f in results:
    p_ = pathlib.Path(f)
    if not p_.exists():
        refuse(f"{f} is missing — the ledger names this scenario but it has not been run. "
               "Run `make parity`.")
    r = json.loads(p_.read_text())
    p = r.get("provenance")
    if not p:
        refuse(f"{f} carries no provenance, so the world it was produced in is unknown. "
               "Re-run `make parity` — a result that cannot say what it was gathered "
               "against is not evidence.")

    if p.get("schema") != "parity-provenance/v1":
        refuse(f"{f}: unrecognized provenance schema")
    if p.get("strict") is not True:
        refuse(f"{f}: produced with strict:false — not admissible as evidence")

    # ── THE HARNESS IS THE INSTRUMENT ────────────────────────────────────────────
    # A modified runner, driver, projection, scenario or ledger can change the observed
    # divergence — or manufacture the declared one outright — while every client stays
    # perfectly on-pin and this script reports success. Pinning what is MEASURED while
    # leaving what MEASURES uncontrolled is not a verified result; it is a verified
    # subject and an unverified instrument.
    #
    # provenance.sh has recorded harness.rev and harness.dirty from the start. Nothing
    # read them. So: evidence must come from a COMMITTED harness. Its revision is
    # recorded either way, so any claim can be reproduced against the exact harness that
    # produced it.
    #
    # (Iterating on the harness therefore cannot settle the ledger, which is the point,
    # and is the same rule already applied to a dirty client-rust. `make parity` still
    # reports the diff; it just does not claim to have proved anything.)
    h = p.get("harness", {})
    if h.get("dirty") is not False:
        refuse(f"{f}: produced by a DIRTY harness (rev {h.get('rev')}). The harness is the "
               "instrument: a modified runner, projection or scenario can change — or "
               "manufacture — the very divergence being adjudicated. Commit the harness, "
               "then re-run `make ledger`.")
    # CLEAN IS NOT ENOUGH: it must be THIS harness. `results/` is gitignored, so artifacts
    # survive a checkout — and an OLD, perfectly clean runner/projection/scenario could
    # then be adjudicated against TODAY's ledger. The instrument that produced the evidence
    # and the instrument now judging it must be the same one, or the pinning means nothing.
    if h.get("rev") != harness_rev:
        refuse(f"{f}: produced by harness {h.get('rev')}, but this is {harness_rev}. "
               "results/ is gitignored and survives a checkout, so a stale artifact can "
               "outlive the code that made it. Re-run `make ledger` to regenerate the "
               "evidence with the harness that is adjudicating it.")

    cr = p["client_rust"]
    if cr.get("matches_pin") is not True:
        refuse(f"{f}: produced against an off-pin client-rust "
               f"(actual {cr.get('rev')}, pinned {cr.get('pinned_rev')})")
    if cr.get("dirty") is not False:
        refuse(f"{f}: produced against a DIRTY client-rust — its revision does not "
               "describe its contents")
    if cr.get("rev") != pins["client_rust"]["rev"]:
        refuse(f"{f}: produced against client-rust {cr.get('rev')}, but pins.toml now "
               f"names {pins['client_rust']['rev']} — the pin moved after this result "
               "was gathered, so it no longer describes the pinned world")

    cg = p["client_go"]
    if cg.get("replaced") is True:
        refuse(f"{f}: the oracle was REPLACED — a local tree you can edit, not the pinned module")
    if cg.get("matches_pin") is not True:
        refuse(f"{f}: client-go resolved to {cg.get('version')}, but the pin names "
               f"{cg.get('pinned_version')}")
    if cg.get("version") != pins["client_go"]["version"]:
        refuse(f"{f}: produced against client-go {cg.get('version')}, but pins.toml now "
               f"names {pins['client_go']['version']}")

    # The COMPILER is part of the pinned world too. A `go` directive is a minimum, so
    # go.mod does not imply which toolchain built the oracle.
    tc = p.get("toolchain", {})
    if tc.get("go_matches_pin") is not True:
        refuse(f"{f}: built with Go {tc.get('go')}, but the pin names {tc.get('pinned_go')}")

    # The SERVER is half of every behavioural claim. Lock resolution, prewrite residue and
    # conflict shapes are server behaviour as much as client behaviour, so a run against
    # an unidentified or off-pin TiKV certifies nothing — however pinned the clients were.
    cl = p["cluster"]
    if cl.get("verified") is not True:
        refuse(f"{f}: the cluster at {cl.get('pd_addr')} could not be identified (no PD reachable)")
    if cl.get("matches_pin") is not True:
        refuse(f"{f}: the cluster was PD {cl.get('observed_pd_version')} / "
               f"TiKV [{cl.get('observed_tikv_versions')}], but the pin names "
               f"{cl.get('pinned_version')}")
PY

# ── 2. THE ORACLE MUST BE THE PINNED ORACLE, AS THE BINARY ITSELF REPORTS IT ─────────
# Not as a file describing it claims. Every trace carries each driver's `hello`, which
# the Go driver fills from runtime/debug.ReadBuildInfo() — including whether the module
# was `replace`d. pins.toml says "an oracle you can accidentally edit is not an oracle";
# this is where that stops being a comment.
#
# THE TRACES ARE REQUIRED, NOT MERELY INSPECTED IF PRESENT. A loop over a missing or
# empty results/traces/ would perform zero checks and exit happily — so deleting the
# traces (or keeping only the divergence artifacts) would have SKIPPED the one proof of
# which client-go binary actually ran, and the skip would have looked like a pass. An
# absent check must never read as a satisfied one, so every adjudicated scenario must
# produce the traces its ledger entry depends on, and each must carry a Go binding.
python3 - "$PINS" "$LEDGER" <<'PY' || exit 2
import json, pathlib, sys, tomllib

pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
ledger = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
want = pins["client_go"]["version"]

def refuse(why):
    sys.exit(f"REFUSING TO SETTLE THE LEDGER: {why}")

for gap in ledger.get("gap", []):
    scen_path = pathlib.Path(gap["scenario"])
    scen = json.loads(scen_path.read_text())
    name = scen["name"]

    # Exactly the runs this gap's verdict is computed from.
    for run in scen["compare"]:
        f = pathlib.Path(f"results/traces/{name}.{run}.json")
        if not f.exists():
            refuse(f"{gap['id']}: {f} is missing. The verdict is computed from these traces, "
                   "and without them the oracle's identity is unproven — an absence is not an answer.")

        t = json.loads(f.read_text())
        go_bindings = [rb for rb in t.get("roles", []) if rb["driver"] == "go"]
        if not go_bindings:
            refuse(f"{gap['id']}: {f} has no `go` role, so nothing attests which client-go ran.")

        for rb in go_bindings:
            c = rb["hello"]["client"]
            if c.get("replaced"):
                refuse(f"{f}: the go driver ran against a REPLACED client-go ({c['version']}) — "
                       "an oracle you can edit is not an oracle.")
            if c["version"] != want:
                refuse(f"{f}: the go driver linked client-go {c['version']}, but the pin names {want}.")
PY

# ── 3. THE VERDICT ───────────────────────────────────────────────────────────────────
python3 - "$LEDGER" <<'PY'
import json, pathlib, sys, tomllib

ledger = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
rc = 0

for gap in ledger.get("gap", []):
    gid, scen = gap["id"], gap["scenario"]
    # The runner names its artifacts after the scenario's JSON `name`, NOT its filename.
    # Deriving the path from the filename stem instead would report a perfectly fresh
    # result as MISSING the moment the two differ — and the admissibility section above
    # already reads scen["name"], so the two halves of this script would disagree about
    # which file they were adjudicating.
    name = json.loads(pathlib.Path(scen).read_text())["name"]
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
