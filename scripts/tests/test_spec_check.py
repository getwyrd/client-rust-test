"""scripts/spec-check.py: ledger.toml, scenarios/ and pins.toml against their specs.

    python3 -m unittest discover -s scripts/tests     # `make broker-test`, part of `make check`

Each test copies the REAL ledger, scenarios and pins into a scratch tree, breaks one rule,
and checks that spec-check names that rule's spec section. The per-scenario checks are the
runner's (`parity-runner --check`) and are tested in crates/parity-runner.
"""

from __future__ import annotations

import importlib.util
import json
import re
import shutil
import tempfile
import tomllib
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("spec_check", REPO / "scripts" / "spec-check.py")
spec_check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(spec_check)


class SpecCheck(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="spec-check-test-"))
        self.addCleanup(shutil.rmtree, self.root)
        for f in ("ledger.toml", "pins.toml", "crates/parity-proto/src/project.rs"):
            (self.root / f).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(REPO / f, self.root / f)
        shutil.copytree(REPO / "scenarios", self.root / "scenarios")
        # Every file the ledger refers to, so the existence check has something to find.
        for gap in tomllib.loads((REPO / "ledger.toml").read_text())["gap"]:
            for key in ("finding", "roadmap", "rules"):
                if key in gap:
                    rel = gap[key].split(" ", 1)[0]
                    (self.root / rel).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(REPO / rel, self.root / rel)

    def edit(self, rel, old, new, count=1):
        p = self.root / rel
        s = p.read_text()
        self.assertIn(old, s, f"fixture drifted: {old!r} not in {rel}")
        p.write_text(s.replace(old, new, count))

    def scenario(self, name):
        return json.loads((self.root / "scenarios" / f"{name}.json").read_text())

    def write_scenario(self, name, s):
        (self.root / "scenarios" / f"{name}.json").write_text(json.dumps(s, indent=2))

    def assertFails(self, section, fragment=""):
        errors = spec_check.check(self.root)
        hits = [e for e in errors if f"({section})" in e and fragment in e]
        self.assertTrue(hits, f"expected an error for {section} {fragment!r}; got {errors}")

    # ── the real repository ─────────────────────────────────────────────────────────

    def test_the_repository_conforms(self):
        self.assertEqual(spec_check.check(REPO), [])

    def test_the_scratch_copy_conforms(self):
        # Every other test starts here: one change from conforming.
        self.assertEqual(spec_check.check(self.root), [])

    # ── ledger/v1 ───────────────────────────────────────────────────────────────────

    def test_wrong_schema(self):
        self.edit("ledger.toml", 'schema = "parity-ledger/v1"', 'schema = "parity-ledger/v2"')
        self.assertFails("ledger/v1 §4.1.1", "schema")

    def test_unknown_top_level_key(self):
        self.edit("ledger.toml", 'schema = "parity-ledger/v1"', 'schema = "parity-ledger/v1"\nnote = "x"')
        self.assertFails("ledger/v1 §4.1.1", "'note'")

    def test_unknown_entry_field(self):
        self.edit("ledger.toml", 'id     = "G-0002"', 'id     = "G-0002"\nexpected = "diverges"')
        self.assertFails("ledger/v1 §4.2.1", "'expected'")

    def test_missing_required_field(self):
        self.edit("ledger.toml", 'area   = "raw.checksum"\n', "")
        self.assertFails("ledger/v1 §4.2", "'area'")

    def test_bad_id(self):
        self.edit("ledger.toml", 'id     = "G-0002"', 'id     = "G-2"')
        self.assertFails("ledger/v1 §4.2", "four digits")

    def test_duplicate_id(self):
        self.edit("ledger.toml", 'id     = "G-0002"', 'id     = "G-0001"')
        self.assertFails("ledger/v1 §4.2", "not unique")

    def test_kind_spelled_as_the_design_doc_spells_it(self):
        self.edit("ledger.toml", 'kind   = "behavioral"', 'kind   = "behaviour"')
        self.assertFails("ledger/v1 §4.2", "kind")

    def test_status_and_expect_disagree(self):
        self.edit("ledger.toml", 'status = "OPEN"', 'status = "FIXED"')
        self.assertFails("ledger/v1 §4.2.2", "FIXED requires")

    def test_diverges_without_divergences(self):
        ledger = (self.root / "ledger.toml").read_text()
        (self.root / "ledger.toml").write_text(ledger.split("[[gap.divergence]]")[0])
        self.assertFails("ledger/v1 §4.2", "at least one")

    def test_agrees_with_divergences(self):
        self.edit("ledger.toml", 'expect   = "agrees"',
                  'expect   = "agrees"\n\n[[gap.divergence]]\nstep = "a"\npath = "class"\noracle = "ok"\nsubject = "ok"')
        self.assertFails("ledger/v1 §4.2.3")

    def test_a_named_file_must_exist(self):
        self.edit("ledger.toml", 'rules   = "docs/design/specs/client-rules/v1.md RAW-4"',
                  'rules   = "docs/client-rules-v2.md RAW-4"')
        self.assertFails("ledger/v1 §4.2.4", "rules")

    def test_a_named_file_must_be_inside_the_repository(self):
        # Both files exist; neither is a repo path.
        outside = self.root.parent / f"{self.root.name}-outside.md"
        outside.write_text("x")
        self.addCleanup(outside.unlink)
        for ref in (str(outside), f"../{outside.name}"):
            with self.subTest(ref=ref):
                self.edit("ledger.toml", 'rules   = "docs/design/specs/client-rules/v1.md RAW-4"',
                          f'rules   = "{ref} RAW-4"')
                self.assertFails("ledger/v1 §4.2.4", "not a path inside the repository")
                self.edit("ledger.toml", f'rules   = "{ref} RAW-4"',
                          'rules   = "docs/design/specs/client-rules/v1.md RAW-4"')

    def test_oracle_wrong_is_reserved(self):
        self.edit("ledger.toml", 'id     = "G-0002"',
                  'id     = "G-0002"\noracle_wrong = { declared_by = "x", date = "2026-10-19", evidence = "y" }')
        self.assertFails("ledger/v1 §4.6", "reserved")

    def test_scenario_written_for_another_gap(self):
        s = self.scenario("raw-checksum")
        s["gap"] = "G-0001"
        self.write_scenario("raw-checksum", s)
        self.assertFails("ledger/v1 §4.4.1", "written for 'G-0001'")

    def test_scenario_spelled_another_way(self):
        self.edit("ledger.toml", 'scenario = "scenarios/raw-checksum.json"',
                  'scenario = "./scenarios/raw-checksum.json"')
        self.assertFails("ledger/v1 §4.4.1", "scenarios/<file>.json")

    def test_scenario_that_is_not_a_path(self):
        self.edit("ledger.toml", 'scenario = "scenarios/raw-checksum.json"', 'scenario = 123')
        self.assertFails("ledger/v1 §4.4.1", "not 123")

    def test_a_wrongly_typed_value_is_reported_not_crashed_on(self):
        # Each field of the G-0002 entry, set to an array and to a table in turn: spec-check
        # must name the entry, never raise.
        base = (self.root / "ledger.toml").read_text()
        entry = base.index('id     = "G-0002"')
        for key in ("id", "slug", "kind", "area", "status", "title", "expectation", "scenario",
                    "expect", "roadmap", "rules"):
            for value in ("[1]", "{ a = 1 }"):
                m = re.compile(rf"^{key} *= .*$", re.M).search(base, entry)
                self.assertIsNotNone(m, key)
                (self.root / "ledger.toml").write_text(base[:m.start()] + f"{key} = {value}" + base[m.end():])
                errors = spec_check.check(self.root)
                self.assertTrue(errors, f"{key} = {value} passed")
        # `divergence` as one table instead of an array of tables.
        one = base.replace("[[gap.divergence]]", "[gap.divergence]", 1).split("[[gap.divergence]]")[0]
        (self.root / "ledger.toml").write_text(one)
        self.assertTrue(spec_check.check(self.root))

    def test_wrongly_typed_divergence_and_scenario_values_are_reported(self):
        base = (self.root / "ledger.toml").read_text()
        for key in ("step", "path", "oracle", "subject"):
            for value in ("[1]", "{ a = 1 }"):
                m = re.compile(rf"^{key} *= .*$", re.M).search(base, base.index("[[gap.divergence]]"))
                (self.root / "ledger.toml").write_text(base[:m.start()] + f"{key} = {value}" + base[m.end():])
                self.assertTrue(spec_check.check(self.root), f"divergence {key} = {value} passed")
        (self.root / "ledger.toml").write_text(base)
        orig = self.scenario("raw-checksum")
        for key in ("name", "gap", "steps", "also_compare"):
            for value in ([[1]], {"a": 1}, [{"id": [1]}]):
                self.write_scenario("raw-checksum", {**orig, key: value})
                spec_check.check(self.root)   # must not raise
        self.write_scenario("raw-checksum", {**orig, "name": ["raw-checksum"]})
        self.assertTrue(spec_check.check(self.root))

    def test_two_entries_naming_one_scenario(self):
        self.edit("ledger.toml", 'scenario = "scenarios/raw-checksum.json"',
                  'scenario = "scenarios/orphaned-lock-resolution.json"')
        self.assertFails("ledger/v1 §4.4.2", "already named")

    def test_a_scenario_no_entry_names(self):
        s = self.scenario("raw-checksum")
        s["name"] = "raw-checksum-copy"
        self.write_scenario("raw-checksum-copy", s)
        self.assertFails("ledger/v1 §4.4.2", "raw-checksum-copy.json")

    def test_scenario_outside_scenarios(self):
        self.edit("ledger.toml", 'scenario = "scenarios/raw-checksum.json"',
                  'scenario = "pins.toml"')
        self.assertFails("ledger/v1 §4.4.1", "scenarios/<file>.json")

    def test_divergence_names_an_unknown_step(self):
        self.edit("ledger.toml", 'step    = "checker-checksum"', 'step    = "checker-checksumm"')
        self.assertFails("ledger/v1 §4.4.3", "checker-checksumm")

    def test_divergence_names_an_unprojected_path(self):
        self.edit("ledger.toml", 'path    = "class"', 'path    = "klass"')
        self.assertFails("ledger/v1 §4.4.4", "'klass'")

    def test_an_opt_in_path_counts_only_when_the_scenario_lists_it(self):
        self.edit("ledger.toml", 'path    = "class"', 'path    = "native.type"')
        self.assertFails("ledger/v1 §4.4.4", "'native.type'")
        s = self.scenario("raw-checksum")
        s["also_compare"] = ["native.type"]
        self.write_scenario("raw-checksum", s)
        self.assertEqual(spec_check.check(self.root), [])

    def test_projection_paths_come_from_the_code(self):
        defaults, opt_in = spec_check.projection_paths(REPO)
        self.assertIn("class", defaults)
        self.assertIn("native.type", opt_in)
        self.edit("crates/parity-proto/src/project.rs", 'pub const DEFAULT_PATHS', 'pub const DEFAULT_PATHZ')
        with self.assertRaises(SystemExit):
            spec_check.check(self.root)

    # ── scenario/v1 (cross-file) ───────────────────────────────────────────────────

    def test_name_must_equal_the_file_name(self):
        s = self.scenario("raw-checksum")
        s["name"] = "raw-sum"
        self.write_scenario("raw-checksum", s)
        self.assertFails("scenario/v1 §4.1.3", "'raw-sum'")

    # ── provenance/v1 §4.5: pins.toml ──────────────────────────────────────────────

    def test_pins_wrong_schema(self):
        self.edit("pins.toml", 'schema = "parity-pins/v1"', 'schema = "parity-pins/v0"')
        self.assertFails("provenance/v1 §4.5", "schema")

    def test_pins_unknown_table(self):
        (self.root / "pins.toml").write_text((self.root / "pins.toml").read_text() + '\n[extra]\nx = "y"\n')
        self.assertFails("provenance/v1 §4.5", "'extra'")

    def test_pins_unknown_key(self):
        self.edit("pins.toml", 'branch   = "integration"', 'branch   = "integration"\nbranch_old = "main"')
        self.assertFails("provenance/v1 §4.5", "'branch_old'")

    def test_pins_short_rev(self):
        pins = (self.root / "pins.toml").read_text()
        rev = re.search(r'^rev = "([0-9a-f]{40})"', pins, re.M).group(1)
        self.edit("pins.toml", f'rev = "{rev}"', f'rev = "{rev[:12]}"')
        self.assertFails("provenance/v1 §4.5", "[client_rust].rev")

    def test_pins_image_without_digest(self):
        pins = (self.root / "pins.toml").read_text()
        image = re.search(r'^pd_image\s*= "([^"]+)"', pins, re.M).group(1)
        self.edit("pins.toml", image, image.split("@")[0])
        self.assertFails("provenance/v1 §4.5", "pd_image")

    def test_pins_matrix_entry_without_digest(self):
        pins = (self.root / "pins.toml").read_text()
        image = re.search(r'^\[cluster\.versions\.[^\n]*\npd_image\s*= "([^"]+)"', pins, re.M).group(1)
        self.edit("pins.toml", image, image.split("@")[0])
        self.assertFails("provenance/v1 §4.5", "cluster.versions")

    def test_pins_missing_table(self):
        pins = (self.root / "pins.toml").read_text()
        (self.root / "pins.toml").write_text(pins.split("[toolchain]")[0])
        self.assertFails("provenance/v1 §4.5", "[toolchain] is missing")

    def test_pins_cluster_that_is_not_a_table(self):
        pins = (self.root / "pins.toml").read_text()
        head, rest = pins.split("[cluster]", 1)
        tail = rest.split("[toolchain]", 1)[1]
        (self.root / "pins.toml").write_text(
            head.replace('schema', 'cluster = "v8.5.5"\nschema', 1) + "[toolchain]" + tail)
        self.assertFails("provenance/v1 §4.5", "[cluster] is missing")


if __name__ == "__main__":
    unittest.main()
