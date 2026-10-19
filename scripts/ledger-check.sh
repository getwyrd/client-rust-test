#!/usr/bin/env bash
# THE PARITY LEDGER CHECK. Proves ledger.toml against a pinned differential run.
#
# This is scripts/gate-verdict.sh, generalized. gate-verdict.sh hard-codes its XFAIL rows
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
#
# ── CANDIDATE MODE ───────────────────────────────────────────────────────────────────
#   PARITY_MODE=candidate PARITY_GAP=<id> scripts/ledger-check.sh
#
# Judges a CHANGE (a loop bundle) instead of settling the ledger. It accepts only results
# stamped by `PARITY_MODE=candidate scripts/provenance.sh`, and only if both trees still
# have the contents that were stamped. Every gap except <id> is judged by its claim as it
# stands on the harness's BASE (its merge-base with origin/main), and the change may not
# edit those claims or their scenarios: otherwise a change could break a gap and delete
# its entry in the same stroke. Every gap except <id> must hold exactly as declared;
# <id>'s outcome is reported, not judged, in results/candidate-verdict.json, because what
# counts as success depends on the change: a fix wants XCONVERGE, a new gap wants
# XDIVERGE. A candidate run never prints the "every claim holds" line: it is not evidence.
set -uo pipefail

cd "$(git rev-parse --show-toplevel)"

LEDGER="${LEDGER:-ledger.toml}"
PINS="${PINS:-pins.toml}"
rc=0

MODE="${PARITY_MODE:-strict}"
GAP="${PARITY_GAP:-}"
case "$MODE" in
  strict|candidate) ;;
  *) echo "ledger-check: PARITY_MODE must be strict or candidate, not '$MODE' (an advisory run settles nothing)" >&2
     exit 2 ;;
esac
# First, before any check can refuse: never leave a previous run's verdict behind for a
# caller to read after a refusal.
[ "$MODE" = candidate ] && rm -f results/candidate-verdict.json
# ── 0. ONE GAP PER ID, AND EACH GAP IS ITS SCENARIO'S ────────────────────────────────
# Every later step keys gaps by id, so two entries with one id would be merged: a verdict
# could report one entry while judging the other. And a gap must be the one its scenario
# was written for (the scenario's `gap` field): otherwise a new entry could point at
# another gap's scenario and be judged by that gap's result.
python3 - "$LEDGER" <<'PY' || exit 2
import json, pathlib, sys, tomllib

ids = []
for gap in tomllib.loads(pathlib.Path(sys.argv[1]).read_text()).get("gap", []):
    gid = gap.get("id")
    if gid in ids:
        sys.exit(f"REFUSING TO JUDGE THE LEDGER: two entries share the id {gid!r}")
    ids.append(gid)
    owner = json.loads(pathlib.Path(gap["scenario"]).read_text()).get("gap")
    if owner != gid:
        sys.exit(f"REFUSING TO JUDGE THE LEDGER: {gid} names {gap['scenario']}, which was "
                 f"written for {owner!r}, so its result is not evidence about {gid}")
PY

cur_harness=""; cur_client_rust=""
if [ "$MODE" = candidate ]; then
  # The base checks below compare pins.toml and ledger.toml with the base. Another file
  # named by PINS or LEDGER would be judged by claims the base never had.
  if [ "$PINS" != pins.toml ] || [ "$LEDGER" != ledger.toml ]; then
    echo "ledger-check: candidate mode reads only pins.toml and ledger.toml (got PINS=$PINS LEDGER=$LEDGER)" >&2
    exit 2
  fi
  if ! python3 -c "
import sys,tomllib,pathlib
ids=[g['id'] for g in tomllib.loads(pathlib.Path('$LEDGER').read_text()).get('gap',[])]
sys.exit(0 if sys.argv[1] in ids else 1)" "$GAP"; then
    echo "ledger-check: candidate mode needs PARITY_GAP set to a gap declared in $LEDGER (got '$GAP')" >&2
    exit 2
  fi
  # The trees as they are NOW. Each result must carry these same digests, or it was
  # produced from different contents than the ones being judged.
  built=$(python3 -c "
import tomllib,pathlib
print(tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['dependencies']['tikv-client']['path'])")
  cur_harness=$(python3 -I scripts/tree-digest.py .) || exit 2
  cur_client_rust=$(python3 -I scripts/tree-digest.py "$built") || exit 2
  base=$(git merge-base HEAD origin/main 2>/dev/null) || base=""
  if [ -z "$base" ]; then
    echo "ledger-check: candidate mode judges the other gaps by the harness's base, but HEAD has no merge-base with origin/main (fetch origin)" >&2
    exit 2
  fi
  # ── 1a. THE CHANGE MAY NOT REWRITE THE CLAIMS IT IS JUDGED BY ─────────────────────
  # The ledger and the scenarios are part of the harness tree the change may modify. So
  # every gap except the declared one must be on the base exactly as it is here, with an
  # unchanged scenario, and the change may add no gap but the declared one.
  python3 - "$LEDGER" "$GAP" "$base" <<'PY' || exit 2
import pathlib, subprocess, sys, tomllib

ledger_path, gap, base = sys.argv[1], sys.argv[2], sys.argv[3]

def at_base(path):
    r = subprocess.run(["git", "show", f"{base}:{path}"], capture_output=True)
    return r.stdout if r.returncode == 0 else None

def refuse(why):
    sys.exit(f"""
REFUSING TO JUDGE THE CANDIDATE: {why}

A candidate is judged against the claims on its base ({base[:12]}). A change that edits
another gap's claim or scenario could break that gap and re-state it in the same stroke,
so it must change only the declared gap ({gap}). Re-state other gaps in their own change.
""")

# The pinned world is the base's: a candidate cannot move it. (A pin move is its own
# change, settled in strict mode once merged.)
if at_base("pins.toml") != pathlib.Path("pins.toml").read_bytes():
    refuse("the change edits pins.toml; a candidate is judged against the base's pins")

raw = at_base(ledger_path)
if raw is None:
    refuse(f"{ledger_path} does not exist on the base")
then = {g["id"]: g for g in tomllib.loads(raw.decode()).get("gap", [])}
now = {g["id"]: g for g in tomllib.loads(pathlib.Path(ledger_path).read_text()).get("gap", [])}
for gid in sorted(set(then) | set(now)):
    if gid == gap:
        continue
    if gid not in now:
        refuse(f"the change removes {gid} from {ledger_path}")
    if gid not in then:
        refuse(f"the change adds {gid}, which is not the declared gap")
    if now[gid] != then[gid]:
        refuse(f"the change edits the entry for {gid}")
    scen = now[gid]["scenario"]
    if at_base(scen) != pathlib.Path(scen).read_bytes():
        refuse(f"the change edits {scen}, the scenario of {gid}")
PY
fi

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
python3 - "$PINS" "$HARNESS_REV" "$LEDGER" "$MODE" "$cur_harness" "$cur_client_rust" "${base:-}" <<'PY' || exit 2
import json, pathlib, sys, tomllib

pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
harness_rev = sys.argv[2]
ledger = tomllib.loads(pathlib.Path(sys.argv[3]).read_text())
mode, cur_harness, cur_client_rust, base = sys.argv[4], sys.argv[5], sys.argv[6], sys.argv[7]

def refuse(why):
    if mode == "candidate":
        sys.exit(f"""
REFUSING TO JUDGE THE CANDIDATE: {why}

A candidate verdict is only meaningful for the exact trees that produced the results,
run against the pinned oracle, cluster and toolchain. Re-run
`PARITY_MODE=candidate make parity` with the trees as they are now.
""")
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
#
# ONE NAME, ONE SCENARIO. The runner runs every scenarios/*.json and names each artifact
# after the scenario's JSON `name`, not its file. Two files with one name would write the
# same artifact, the later one winning, so a gap could be judged by another scenario's
# result while its own scenario file is untouched.
#
# And the names must be PLAIN (the runner's rule, scenario.rs): they become file names,
# so `x/../divergence.other` would differ from `other` as a string and still write over
# its result. Run names become part of trace file names, so they are held to it too.
import re
plain = re.compile(r"[a-z0-9][a-z0-9-]*")
names = {}
for sp in sorted(pathlib.Path("scenarios").glob("*.json")):
    sc = json.loads(sp.read_text())
    n = sc.get("name")
    if not isinstance(n, str) or not plain.fullmatch(n):
        refuse(f"{sp}: scenario name {n!r} is not lowercase letters, digits and `-`, so it "
               "cannot be trusted as a file name under results/")
    for run in sc.get("runs", []):
        if not isinstance(run.get("name"), str) or not plain.fullmatch(run["name"]):
            refuse(f"{sp}: run name {run.get('name')!r} is not lowercase letters, digits and `-`")
    if n in names:
        refuse(f"{names[n]} and {sp} share the scenario name {n!r}, so one overwrites the "
               "other's result and a gap could be judged by the wrong scenario")
    names[n] = sp
results = []
for gap in ledger.get("gap", []):
    name = json.loads(pathlib.Path(gap["scenario"]).read_text())["name"]
    results.append((gap["id"], f"results/divergence.{name}.json"))

if not results:
    refuse("the ledger declares no gaps — there is nothing to settle")

def strict_subject_and_harness(f, p):
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


def oracle_cluster_toolchain(f, p):
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

for gid, f in results:
    p_ = pathlib.Path(f)
    if not p_.exists():
        refuse(f"{f} is missing — the ledger names this scenario but it has not been run. "
               "Run `make parity`.")
    r = json.loads(p_.read_text())
    # The artifact says which gap it was produced for (its scenario's `gap`). It must be
    # the gap that names it, or another scenario's result is standing in.
    if r.get("gap") != gid:
        refuse(f"{f} was produced for gap {r.get('gap')!r}, but the ledger judges it as {gid}")
    p = r.get("provenance")
    if not p:
        refuse(f"{f} carries no provenance, so the world it was produced in is unknown. "
               "Re-run `make parity` — a result that cannot say what it was gathered "
               "against is not evidence.")

    if p.get("schema") != "parity-provenance/v1":
        refuse(f"{f}: unrecognized provenance schema")

    if mode == "candidate":
        # ── A CANDIDATE: a change judged against the pinned world ────────────────────
        # The subject and the harness may differ from the pin; what they contain is
        # fixed by digest instead of by revision. A digest that no longer matches the
        # tree on disk means the evidence came from other contents (results/ is
        # gitignored and survives edits), so it describes nothing that is here now.
        # The oracle, cluster and toolchain checks below apply exactly as in strict mode.
        if p.get("mode") != "candidate":
            refuse(f"{f}: candidate mode judges only candidate-mode results, and this one "
                   f"was produced in mode {p.get('mode', 'strict' if p.get('strict') else 'advisory')!r}. "
                   "Re-run with PARITY_MODE=candidate.")
        c = p.get("candidate") or {}
        ch, cc = c.get("harness", {}), c.get("client_rust", {})
        if ch.get("base") != base:
            refuse(f"{f}: produced on harness base {ch.get('base')!r}, but the base is now "
                   f"{base}, so the claims it was judged against have moved")
        if ch.get("digest") != cur_harness:
            refuse(f"{f}: produced by a harness with digest {ch.get('digest')}, but the harness "
                   f"here now has digest {cur_harness}. The harness is the instrument; re-run "
                   "with the harness that is judging the result.")
        if p["client_rust"].get("pinned_rev") != pins["client_rust"]["rev"]:
            refuse(f"{f}: produced against the pin {p['client_rust'].get('pinned_rev')}, but "
                   f"pins.toml now names {pins['client_rust']['rev']}")
        if cc.get("contains_pin") is not True:
            refuse(f"{f}: the client-rust tree does not contain the pin, so its verdict mixes "
                   "the change with everything between the two revisions")
        if p.get("toolchain", {}).get("pinned_rust") != pins["toolchain"]["rust"] \
                or p.get("toolchain", {}).get("rust_matches_pin") is not True:
            refuse(f"{f}: built with {p.get('toolchain', {}).get('rust')}, not the pinned "
                   "Rust toolchain. A candidate may edit rust-toolchain.toml, so the "
                   "compiler that ran is checked, not assumed.")
        if cc.get("digest") != cur_client_rust:
            refuse(f"{f}: produced against a client-rust tree with digest {cc.get('digest')}, "
                   f"but the tree cargo builds now has digest {cur_client_rust}")
    else:
        if p.get("strict") is not True:
            refuse(f"{f}: produced with strict:false — not admissible as evidence")
        strict_subject_and_harness(f, p)

    oracle_cluster_toolchain(f, p)
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
python3 - "$PINS" "$LEDGER" "$MODE" <<'PY' || exit 2
import json, pathlib, sys, tomllib

pins = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
ledger = tomllib.loads(pathlib.Path(sys.argv[2]).read_text())
mode = sys.argv[3]
want = pins["client_go"]["version"]

def refuse(why):
    what = "JUDGE THE CANDIDATE" if mode == "candidate" else "SETTLE THE LEDGER"
    sys.exit(f"REFUSING TO {what}: {why}")

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
python3 - "$LEDGER" "$MODE" "$GAP" "${base:-}" <<'PY'
import json, pathlib, subprocess, sys, tomllib

ledger = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
mode, declared_gap, base = sys.argv[2], sys.argv[3], sys.argv[4]
outcome = {}

if mode == "candidate":
    def git(*a):
        return subprocess.run(["git", *a], capture_output=True, text=True)
    then = git("show", f"{base}:{sys.argv[1]}")
    then_gaps = {g["id"]: g for g in tomllib.loads(then.stdout).get("gap", [])} if then.returncode == 0 else {}
    now_entry = next(g for g in ledger["gap"] if g["id"] == declared_gap)
    declared_entry = ("new" if declared_gap not in then_gaps
                      else "unchanged" if then_gaps[declared_gap] == now_entry else "changed")
    # Tracked changes against the base, plus untracked files .gitignore does not exclude
    # (the same set scripts/tree-digest.py digests).
    harness_changed = sorted(set(
        git("diff", "--name-only", base).stdout.split("\n")
        + git("ls-files", "--others", "--exclude-per-directory=.gitignore").stdout.split("\n")) - {""})

for gap in ledger.get("gap", []):
    gid, scen = gap["id"], gap["scenario"]
    # In candidate mode the declared gap's outcome is the RESULT being reported, so the
    # advice written for a pinned run (re-pin, flip the entry) does not apply to it.
    reported = mode == "candidate" and gid == declared_gap
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
        outcome[gid] = "MISSING"
        continue

    observed = json.loads(dpath.read_text())["divergences"]
    # Compare as SETS of (step, path, oracle, subject). Order is not a fact.
    obs = {(d["step"], d["path"], d["oracle"], d["subject"]) for d in observed}

    if gap["expect"] == "agrees":
        if not obs:
            print("    HOLDS — the two clients agree on every compared field.")
            outcome[gid] = "HOLDS"
        else:
            print(f"""
NEW GAP — {gid} is declared to AGREE, and it did not.

An UNDECLARED difference between the two clients. That is exactly what this harness
exists to find, and it is a hard failure so that it cannot be ignored: either file it as
a finding and declare it here, or fix it.
""", file=sys.stderr)
            for d in sorted(obs):
                print(f"    {d[0]} · {d[1]}\n        oracle : {d[2]}\n        subject: {d[3]}", file=sys.stderr)
            outcome[gid] = "NEW GAP"
        continue

    # expect == "diverges"
    declared = {(d["step"], d["path"], d["oracle"], d["subject"]) for d in gap.get("divergence", [])}

    if not obs:
        outcome[gid] = "XCONVERGE"
        if reported:
            print(f"    XCONVERGE — the two clients AGREED; {gid} is declared to diverge.")
            continue
        print(f"""
XCONVERGE — {gid} is declared to DIVERGE, and the two clients AGREED.

The gap appears to be CLOSED at the pinned revision. That is good news, and it is a hard
failure on purpose: the pin, the README's verdict, and this ledger entry are now stale.

  1. find the fix ({gap.get('upstream_pr', 'the upstream PR')}): on the fork's integration
     branch, and whether it has merged upstream yet — CI's informational upstream_base run
     shows the latter (ADR-0001)
  2. make sure pins.toml client_rust.rev is an integration commit that contains it
  3. flip this entry to status = "FIXED" and expect = "agrees" — it becomes a permanent
     REGRESSION GUARD. Do not delete it: the day the fix regresses, this is what catches it.
""", file=sys.stderr)
        continue

    missing = declared - obs
    extra   = obs - declared

    if not missing and not extra:
        print(f"    XDIVERGE — the gap is still open, and it diverged EXACTLY as declared "
              f"({len(declared)} field(s)).")
        for d in sorted(declared):
            print(f"      {d[0]} · {d[1]}: oracle={d[2]!r} subject={d[3]!r}")
        outcome[gid] = "XDIVERGE"
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
    outcome[gid] = "WRONG DIVERGENCE"

# A claim holds when it came out as declared: HOLDS for `agrees`, XDIVERGE for `diverges`.
broken = {g: o for g, o in outcome.items() if o not in ("HOLDS", "XDIVERGE")}

print()
if mode == "candidate":
    others = {g: o for g, o in outcome.items() if g != declared_gap}
    others_broken = sorted(g for g in others if g in broken)
    # Written before the exit, pass or fail: the caller decides what the declared gap's
    # outcome means for its change, and needs it even when another gap broke.
    # The provenance is the one section 1 checked: the declared gap's own artifact, not
    # results/provenance.json, which a later provenance.sh run may have rewritten.
    scen_name = json.loads(pathlib.Path(
        next(g for g in ledger["gap"] if g["id"] == declared_gap)["scenario"]).read_text())["name"]
    checked = json.loads(pathlib.Path(f"results/divergence.{scen_name}.json").read_text())
    pathlib.Path("results/candidate-verdict.json").write_text(json.dumps({
        "schema": "parity-candidate-verdict/v1",
        "gap": declared_gap,
        "outcome": outcome[declared_gap],
        "others_hold": not others_broken,
        "others": others,
        # What the change did to the instrument, for the caller to judge: a fix to
        # client-rust should touch no harness file at all.
        "declared_entry": declared_entry,
        "harness_changed_files": harness_changed,
        "provenance": checked["provenance"].get("candidate"),
    }, indent=2) + "\n")
    print(f"CANDIDATE — not evidence. {declared_gap}: {outcome[declared_gap]}.")
    if outcome[declared_gap] == "MISSING":
        print(f"CANDIDATE: {declared_gap} has no result, so there is no outcome to report.",
              file=sys.stderr)
        sys.exit(1)
    if others_broken:
        print("CANDIDATE: other claims do NOT hold: "
              + ", ".join(f"{g} {others[g]}" for g in others_broken), file=sys.stderr)
        sys.exit(1)
    print("CANDIDATE: every other claim holds. Outcome -> results/candidate-verdict.json")
    sys.exit(0)

if not broken:
    print("LEDGER: every claim holds at the pinned revisions.")
    sys.exit(0)
print("LEDGER: the claims do NOT match what was observed.", file=sys.stderr)
sys.exit(1)
PY
rc=$?

exit "$rc"
