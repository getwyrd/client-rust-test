#!/usr/bin/env python3
"""Check ledger.toml, scenarios/ and pins.toml against their specs (docs/design/specs/).

    python3 -I scripts/spec-check.py [<repo-root>]     # `make spec-check`, part of `make check`

No cluster and no build: this reads files. The checks a scenario can fail on its own are
the runner's (`parity-runner --check`, scenario/v1 §4.4), and scripts/spec-check.sh runs
that first; this script makes the checks that span files, and the ledger and pins shapes.
Every error names the spec section it enforces. Exit 0 when everything conforms, 1 when
something does not.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

PLAIN = re.compile(r"[a-z0-9][a-z0-9-]*")
GAP_ID = re.compile(r"G-[0-9]{4}")
AREA = re.compile(r"[a-z0-9-]+(\.[a-z0-9-]+)*")
HEX40 = re.compile(r"[0-9a-f]{40}")
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
TIKV_VERSION = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")
IMAGE = re.compile(r"[a-z0-9./-]+:[A-Za-z0-9._-]+@sha256:[0-9a-f]{64}")
GO_VERSION = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?")
URL = re.compile(r"https://\S+")

LEDGER_FIELDS = {"id", "slug", "kind", "area", "status", "title", "expectation", "scenario",
                 "expect", "divergence", "upstream_issue", "upstream_pr", "finding", "roadmap",
                 "rules"}
LEDGER_REQUIRED = {"id", "slug", "kind", "area", "status", "title", "expectation", "scenario",
                   "expect"}
KINDS = {"behavioral", "capability", "performance"}
STATUS_EXPECT = {"OPEN": "diverges", "FIXED": "agrees"}
DIVERGENCE_FIELDS = {"step", "path", "oracle", "subject"}

PINS_SHAPE = {
    "client_rust": {"upstream": URL, "fork": URL, "branch": PLAIN, "rev": HEX40,
                    "upstream_base": HEX40},
    "client_go": {"module": re.compile(r"\S+"), "version": GO_VERSION, "rev": HEX40},
    "cluster": {"tikv_version": TIKV_VERSION, "pd_image": IMAGE, "tikv_image": IMAGE},
    "toolchain": {"rust": SEMVER, "go": SEMVER},
}


def projection_paths(root: Path) -> tuple[set[str], set[str]]:
    """DEFAULT_PATHS and OPT_IN_PATHS, read from the code that projects (observation/v1
    §4.5), so this script never keeps a copy that could drift."""
    src = (root / "crates/parity-proto/src/project.rs").read_text()

    def const(name: str) -> set[str]:
        m = re.search(rf"pub const {name}: &\[&str\] = &\[(.*?)\];", src, re.S)
        if not m:
            raise SystemExit(f"spec-check: cannot find {name} in project.rs")
        return set(re.findall(r'"([^"]+)"', m.group(1)))

    return const("DEFAULT_PATHS"), const("OPT_IN_PATHS")


def repo_path(root: Path, value: str) -> Path:
    """The file part of a `path [section]` reference (ledger/v1 §4.2)."""
    return root / value.split(" ", 1)[0]


def check_ledger(root: Path, errors: list[str]) -> None:
    def err(gid: str, sec: str, msg: str) -> None:
        errors.append(f"ledger.toml {gid}: {msg} (ledger/v1 {sec})")

    try:
        ledger = tomllib.loads((root / "ledger.toml").read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        errors.append(f"ledger.toml: cannot read: {e} (ledger/v1 §4.1)")
        return
    if ledger.get("schema") != "parity-ledger/v1":
        err("-", "§4.1.1", f"schema is {ledger.get('schema')!r}, not 'parity-ledger/v1'")
    for key in sorted(set(ledger) - {"schema", "gap"}):
        err("-", "§4.1.1", f"unknown top-level key {key!r}")
    gaps = ledger.get("gap", [])
    if not isinstance(gaps, list) or not all(isinstance(g, dict) for g in gaps):
        err("-", "§4.1.1", "`gap` must be an array of [[gap]] tables")
        return
    if not gaps:
        err("-", "§4.1.2", "declares no gaps")

    defaults, opt_in = projection_paths(root)
    seen_ids: set[str] = set()
    claimed: dict[str, str] = {}
    for gap in gaps:
        gid = str(gap.get("id", "?"))
        if "oracle_wrong" in gap:
            err(gid, "§4.6", "carries `oracle_wrong`, which is reserved until ledger-check "
                "implements XORACLE")
        for key in sorted(set(gap) - LEDGER_FIELDS - {"oracle_wrong"}):
            err(gid, "§4.2.1", f"unknown field {key!r}")
        for key in sorted(LEDGER_REQUIRED - set(gap)):
            err(gid, "§4.2", f"missing required field {key!r}")
        if not isinstance(gap.get("id"), str) or not GAP_ID.fullmatch(gap["id"]):
            err(gid, "§4.2", "id must be G- followed by four digits")
        elif gid in seen_ids:
            err(gid, "§4.2", "id is not unique")
        seen_ids.add(gid)
        for key, pattern in (("slug", PLAIN), ("area", AREA)):
            if key in gap and not (isinstance(gap[key], str) and pattern.fullmatch(gap[key])):
                err(gid, "§4.2", f"{key} {gap[key]!r} does not match {pattern.pattern}")
        if "kind" in gap and str(gap["kind"]) not in KINDS:
            err(gid, "§4.2", f"kind {gap['kind']!r} is not one of {sorted(KINDS)}")
        for key in ("title", "expectation"):
            if key in gap and not (isinstance(gap[key], str) and gap[key].strip()):
                err(gid, "§4.2", f"{key} must be non-empty text")
        if "title" in gap and "\n" in str(gap["title"]):
            err(gid, "§4.2", "title must be one line")
        # str(): a non-string value is reported as a wrong value, never crashes the check.
        status, expect = str(gap.get("status")), str(gap.get("expect"))
        if status not in STATUS_EXPECT:
            err(gid, "§4.2", f"status {status!r} is not one of {sorted(STATUS_EXPECT)}")
        elif expect != STATUS_EXPECT[status]:
            err(gid, "§4.2.2", f"status {status} requires expect = {STATUS_EXPECT[status]!r}, "
                f"not {expect!r}")
        if expect not in ("diverges", "agrees"):
            err(gid, "§4.2", f"expect {expect!r} is not 'diverges' or 'agrees'")
        for key in ("upstream_issue", "upstream_pr"):
            if key in gap and not (isinstance(gap[key], str) and URL.fullmatch(gap[key])):
                err(gid, "§4.2", f"{key} must be an https URL")
        for key in ("finding", "roadmap", "rules"):
            if key in gap and not (isinstance(gap[key], str) and repo_path(root, gap[key]).is_file()):
                err(gid, "§4.2.4", f"{key} names {gap[key]!r}, which does not exist")

        divs = gap.get("divergence", [])
        if not isinstance(divs, list):
            err(gid, "§4.2", "divergence must be an array of [[gap.divergence]] tables")
            divs = []
        if expect == "diverges" and not divs:
            err(gid, "§4.2", "a `diverges` entry needs at least one [[gap.divergence]]")
        if expect == "agrees" and "divergence" in gap:
            err(gid, "§4.2.3", "an `agrees` entry must not carry divergences")

        # The scenario that is this entry's evidence (§4.4).
        scen_ref = gap.get("scenario")
        if not isinstance(scen_ref, str):
            if "scenario" in gap:
                err(gid, "§4.4.1", f"scenario must be a repo path, not {scen_ref!r}")
            continue
        scen_path = root / scen_ref
        # One spelling only, so each scenario has one owner key: `./scenarios/x.json` and
        # `scenarios/x.json` would otherwise count as two different scenarios.
        if not re.fullmatch(r"scenarios/[^/]+\.json", scen_ref) or not scen_path.is_file():
            err(gid, "§4.4.1", f"scenario {scen_ref!r} is not written as scenarios/<file>.json "
                "naming an existing file")
            continue
        if scen_ref in claimed:
            err(gid, "§4.4.2", f"scenario {scen_ref} is already named by {claimed[scen_ref]}")
        claimed[scen_ref] = gid
        try:
            scen = json.loads(scen_path.read_text())
        except ValueError as e:
            err(gid, "§4.4.1", f"{scen_ref} is not JSON: {e}")
            continue
        if not isinstance(scen, dict):
            err(gid, "§4.4.1", f"{scen_ref} is not a JSON object")
            continue
        if scen.get("gap") != gid:
            err(gid, "§4.4.1", f"{scen_ref} was written for {scen.get('gap')!r}, not {gid}")
        steps = scen.get("steps", [])
        step_ids = {s.get("id") for s in (steps if isinstance(steps, list) else []) if isinstance(s, dict)}
        also = scen.get("also_compare", [])
        paths = defaults | (opt_in & set(also if isinstance(also, list) else []))
        for i, d in enumerate(divs):
            where = f"divergence #{i + 1}"
            if not isinstance(d, dict):
                err(gid, "§4.2", f"{where} is not a table")
                continue
            for key in sorted(set(d) - DIVERGENCE_FIELDS):
                err(gid, "§4.2.1", f"{where}: unknown field {key!r}")
            for key in sorted(DIVERGENCE_FIELDS - set(d)):
                err(gid, "§4.2", f"{where}: missing {key!r}")
            for key in sorted(DIVERGENCE_FIELDS & set(d)):
                if not isinstance(d[key], str):
                    err(gid, "§4.2", f"{where}: {key} must be a string")
            if d.get("step") not in step_ids:
                err(gid, "§4.4.3", f"{where}: step {d.get('step')!r} is not a step of {scen_ref}")
            if d.get("path") not in paths:
                err(gid, "§4.4.4", f"{where}: path {d.get('path')!r} is not projected for "
                    f"{scen_ref} (default paths, plus the opt-in paths it lists)")

    for f in sorted((root / "scenarios").glob("*.json")):
        ref = f"scenarios/{f.name}"
        if ref not in claimed:
            errors.append(f"{ref}: no ledger entry names it (ledger/v1 §4.4.2)")


def check_scenarios(root: Path, errors: list[str]) -> None:
    names: dict[str, str] = {}
    for f in sorted((root / "scenarios").glob("*.json")):
        ref = f"scenarios/{f.name}"
        try:
            scen = json.loads(f.read_text())
        except ValueError as e:
            errors.append(f"{ref}: not JSON: {e} (scenario/v1 §4.1)")
            continue
        name = scen.get("name") if isinstance(scen, dict) else None
        if name != f.stem:
            errors.append(f"{ref}: name {name!r} must equal the file name {f.stem!r} "
                          "(scenario/v1 §4.1.3)")
        if name in names:
            errors.append(f"{ref}: shares the name {name!r} with {names[name]} "
                          "(scenario/v1 §4.1.3)")
        names[name] = ref


def check_pins(root: Path, errors: list[str]) -> None:
    def err(msg: str) -> None:
        errors.append(f"pins.toml: {msg} (provenance/v1 §4.5)")

    try:
        pins = tomllib.loads((root / "pins.toml").read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        err(f"cannot read: {e}")
        return
    if pins.get("schema") != "parity-pins/v1":
        err(f"schema is {pins.get('schema')!r}, not 'parity-pins/v1'")
    for key in sorted(set(pins) - {"schema"} - set(PINS_SHAPE)):
        err(f"unknown table or key {key!r}")

    def shape(table: str, got: object, want: dict[str, re.Pattern], extra: set[str] = set()) -> None:
        if not isinstance(got, dict):
            err(f"[{table}] is missing")
            return
        for key in sorted(set(got) - set(want) - extra):
            err(f"[{table}] has unknown key {key!r}")
        for key, pattern in want.items():
            v = got.get(key)
            if not (isinstance(v, str) and pattern.fullmatch(v)):
                err(f"[{table}].{key} = {v!r} does not match {pattern.pattern}")

    for table, want in PINS_SHAPE.items():
        shape(table, pins.get(table), want, {"versions"} if table == "cluster" else set())
    versions = (pins.get("cluster") or {}).get("versions", {})
    if not isinstance(versions, dict):
        err("[cluster.versions] must be a table")
        return
    for ver, entry in versions.items():
        if not TIKV_VERSION.fullmatch(ver):
            err(f"[cluster.versions] key {ver!r} is not a version like v8.5.5")
        shape(f'cluster.versions."{ver}"', entry, {"pd_image": IMAGE, "tikv_image": IMAGE})


def check(root: Path) -> list[str]:
    errors: list[str] = []
    check_ledger(root, errors)
    check_scenarios(root, errors)
    check_pins(root, errors)
    return errors


def main(argv: list[str]) -> int:
    root = Path(argv[1]) if len(argv) > 1 else Path.cwd()
    errors = check(root)
    for e in errors:
        print(f"SPEC: {e}", file=sys.stderr)
    if errors:
        print(f"spec-check: {len(errors)} problem(s)", file=sys.stderr)
        return 1
    print("spec-check: ledger.toml, scenarios/ and pins.toml conform to their specs")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
