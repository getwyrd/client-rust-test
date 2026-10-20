"""Candidate mode: scripts/provenance.sh, scripts/ledger-check.sh and scripts/tree-digest.py.

    python3 -m unittest discover -s scripts/tests     # `make broker-test`, part of `make check`

No cluster: ledger-check runs over real results from a candidate run
(fixtures/candidate-run), copied into a scratch harness whose digests they are rewritten
to match. Each test then changes one thing and checks the verdict or the refusal.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FIXTURE = Path(__file__).resolve().parent / "fixtures" / "candidate-run"
DIGEST = REPO / "scripts" / "tree-digest.py"


def git(cwd, *args):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True)


def digest(tree):
    return subprocess.run([sys.executable, "-I", str(DIGEST), str(tree)], check=True,
                          capture_output=True, text=True).stdout.strip()


class Scratch(unittest.TestCase):
    """A scratch harness next to a scratch client-rust, both committed git repos."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="candidate-test-"))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.h = self.tmp / "client-rust-test"
        self.cr = self.tmp / "client-rust"
        for d in (self.h, self.cr):
            d.mkdir()
            git(d, "init", "-q")
            git(d, "config", "user.email", "t@example.invalid")
            git(d, "config", "user.name", "t")
        for f in ("ledger.toml", "pins.toml", "Cargo.toml", ".gitignore"):
            shutil.copy(REPO / f, self.h / f)
        shutil.copytree(REPO / "scenarios", self.h / "scenarios")
        (self.h / "scripts").mkdir()
        for f in ("ledger-check.sh", "provenance.sh", "tree-digest.py"):
            shutil.copy(REPO / "scripts" / f, self.h / "scripts" / f)
        (self.cr / "src").mkdir()
        (self.cr / "src" / "lib.rs").write_text("// subject\n")
        for d in (self.h, self.cr):
            git(d, "add", "-A")
            git(d, "commit", "-q", "-m", "init")
        # The harness's base: candidate mode judges the other gaps by the ledger there.
        git(self.h, "update-ref", "refs/remotes/origin/main", "HEAD")
        self.results = self.h / "results"
        shutil.copytree(FIXTURE, self.results, ignore=shutil.ignore_patterns("README.md"))
        self.stamp()

    def edit_results(self, fn):
        """Apply fn to the provenance of every result file ledger-check reads."""
        for f in [self.results / "provenance.json", *self.results.glob("divergence.*.json")]:
            d = json.loads(f.read_text())
            fn(d if f.name == "provenance.json" else d["provenance"])
            f.write_text(json.dumps(d))

    def stamp(self):
        """Point the fixture's digests at the scratch trees, as a fresh candidate run would."""
        hd, cd = digest(self.h), digest(self.cr)
        base = subprocess.run(["git", "-C", str(self.h), "rev-parse", "origin/main"],
                              check=True, capture_output=True, text=True).stdout.strip()

        def fn(p):
            p["candidate"]["harness"]["base"] = base
            p["candidate"]["harness"]["digest"] = hd
            p["candidate"]["client_rust"]["digest"] = cd
        self.edit_results(fn)

    def run_script(self, script, **env):
        e = {k: v for k, v in os.environ.items()
             if k not in ("PARITY_MODE", "PARITY_GAP", "PARITY_STRICT", "CLIENT_RUST")}
        e.update(env)
        return subprocess.run([str(self.h / "scripts" / script)], cwd=self.h, env=e,
                              capture_output=True, text=True)

    def check(self, **env):
        return self.run_script("ledger-check.sh", **env)

    def candidate(self, gap="G-0002"):
        return self.check(PARITY_MODE="candidate", PARITY_GAP=gap)

    def verdict(self):
        return json.loads((self.results / "candidate-verdict.json").read_text())

    def edit_ledger(self, fn):
        """Rewrite one [[gap]] block of the scratch ledger.toml, then re-stamp."""
        led = self.h / "ledger.toml"
        parts = re.split(r"(?m)^(?=\[\[gap\]\])", led.read_text())
        led.write_text("".join(fn(part) for part in parts))
        self.stamp()

    def break_g0001(self):
        f = self.results / "divergence.orphaned-lock-resolution.json"
        d = json.loads(f.read_text())
        d["divergences"] = [{"step": "s", "path": "p", "oracle": "a", "subject": "b"}]
        f.write_text(json.dumps(d))


class LedgerCheckCandidate(Scratch):
    def edit_trace(self, scenario, run, fn):
        f = self.results / "traces" / f"{scenario}.{run}.json"
        t = json.loads(f.read_text())
        fn(t)
        f.write_text(json.dumps(t))

    def test_an_all_rust_subject_run_is_judged(self):
        # G-0001: each client built its own orphan, so the subject run has no client-go
        # to attest, and the oracle run still proves which client-go ran.
        t = json.loads((self.results / "traces" /
                        "orphaned-lock-resolution.subject.json").read_text())
        self.assertEqual({rb["driver"] for rb in t["roles"]}, {"rust"})
        r = self.candidate()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_the_oracle_run_must_attest_client_go(self):
        def all_rust(t):
            for rb in t["roles"]:
                rb["driver"] = "rust"
        self.edit_trace("orphaned-lock-resolution", "oracle", all_rust)
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("is the oracle run and has no `go` role", r.stderr)

    def test_a_trace_must_report_every_go_role_its_scenario_binds(self):
        # G-0003's subject run binds the orphaner to go; dropping it from the trace must
        # not make the run pass as all-Rust.
        def drop_go(t):
            t["roles"] = [rb for rb in t["roles"] if rb["driver"] != "go"]
        self.edit_trace("orphaned-lock-interop", "subject", drop_go)
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("but the scenario binds ['orphaner'] to go", r.stderr)

    def test_a_go_binding_in_the_subject_run_is_still_checked(self):
        def wrong_go(t):
            for rb in t["roles"]:
                if rb["driver"] == "go":
                    rb["hello"]["client"]["version"] = "v2.0.0"
        # G-0003's subject run keeps a Go orphaner.
        self.edit_trace("orphaned-lock-interop", "subject", wrong_go)
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("but the pin names", r.stderr)

    def test_reports_the_declared_gap_and_holds_the_rest(self):
        r = self.candidate()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("CANDIDATE — not evidence. G-0002: XDIVERGE.", r.stdout)
        self.assertNotIn("every claim holds at the pinned", r.stdout)
        v = self.verdict()
        self.assertEqual((v["gap"], v["outcome"], v["others_hold"]), ("G-0002", "XDIVERGE", True))
        self.assertEqual(v["others"], {"G-0001": "HOLDS", "G-0003": "HOLDS"})

    def test_a_closed_gap_is_reported_not_failed(self):
        f = self.results / "divergence.raw-checksum.json"
        d = json.loads(f.read_text())
        d["divergences"] = []
        f.write_text(json.dumps(d))
        r = self.candidate()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.verdict()["outcome"], "XCONVERGE")
        self.assertNotIn("flip this entry", r.stderr)

    def test_another_gap_breaking_fails_but_still_writes_the_verdict(self):
        self.break_g0001()
        r = self.candidate()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("G-0001 NEW GAP", r.stderr)
        v = self.verdict()
        self.assertEqual((v["outcome"], v["others_hold"]), ("XDIVERGE", False))

    def test_records_what_the_change_did_to_the_harness(self):
        v = (self.candidate(), self.verdict())[1]
        self.assertEqual((v["declared_entry"], v["harness_changed_files"]), ("unchanged", []))
        self.edit_ledger(lambda b: b.replace('status = "OPEN"', 'status = "OPEN" # edited')
                         if 'id     = "G-0002"' in b else b)
        r = self.candidate()
        self.assertEqual(r.returncode, 0, r.stderr)
        v = self.verdict()
        self.assertEqual((v["declared_entry"], v["harness_changed_files"]), ("unchanged", ["ledger.toml"]))
        self.edit_ledger(lambda b: b.replace('status = "OPEN" # edited', 'status = "FIXED"'))
        self.candidate()
        self.assertEqual(self.verdict()["declared_entry"], "changed")

    def test_the_change_cannot_delete_a_gap_it_breaks(self):
        # The review's reproduction: break G-0001 and drop its entry in the same change.
        self.break_g0001()
        self.edit_ledger(lambda b: "" if 'id     = "G-0001"' in b else b)
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("the change removes G-0001", r.stderr)

    def test_the_change_cannot_restate_another_gap(self):
        self.edit_ledger(lambda b: b.replace('expect   = "agrees"', 'expect   = "diverges"'))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("the change edits the entry for G-0001", r.stderr)

    def test_the_change_cannot_edit_another_gaps_scenario(self):
        f = self.h / "scenarios" / "orphaned-lock-resolution.json"
        f.write_text(f.read_text() + "\n")
        self.stamp()
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("the scenario of G-0001", r.stderr)

    def test_the_change_cannot_add_a_second_gap(self):
        src = json.loads((self.h / "scenarios" / "orphaned-lock-resolution.json").read_text())
        src.update(name="extra", gap="G-0099")
        (self.h / "scenarios" / "extra.json").write_text(json.dumps(src))
        self.edit_ledger(lambda b: b + b.replace('"G-0001"', '"G-0099"')
                         .replace("scenarios/orphaned-lock-resolution.json", "scenarios/extra.json")
                         if 'id     = "G-0001"' in b else b)
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("adds G-0099, which is not the declared gap", r.stderr)

    def test_a_scenario_cannot_take_another_gaps_name(self):
        # The re-review's reproduction: a new scenario named like G-0001's overwrites its
        # result, while G-0001's own entry and scenario stay byte-identical to the base.
        src = json.loads((self.h / "scenarios" / "orphaned-lock-resolution.json").read_text())
        (self.h / "scenarios" / "zz-new.json").write_text(json.dumps(src))
        self.stamp()
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("share the scenario name 'orphaned-lock-resolution'", r.stderr)
        self.assertEqual(self.check().returncode, 2)

    def test_a_scenario_name_cannot_climb_out_of_results(self):
        # The third review's reproduction: a different string that resolves to the same file.
        src = json.loads((self.h / "scenarios" / "orphaned-lock-resolution.json").read_text())
        src["name"] = "x/../divergence.orphaned-lock-resolution"
        (self.h / "scenarios" / "zz-new.json").write_text(json.dumps(src))
        self.stamp()
        r = self.candidate()
        self.assertEqual(r.returncode, 2, r.stdout)
        self.assertIn("cannot be trusted as a file name", r.stderr)
        self.assertEqual(self.check().returncode, 2)

    def test_reads_only_the_default_pins_and_ledger(self):
        shutil.copy(self.h / "pins.toml", self.h / "other.toml")
        r = self.check(PARITY_MODE="candidate", PARITY_GAP="G-0002", PINS="other.toml")
        self.assertEqual(r.returncode, 2)
        self.assertIn("reads only pins.toml and ledger.toml", r.stderr)

    def test_refuses_two_entries_with_one_id(self):
        # Codex on #71: a second entry reusing the declared gap's id.
        self.edit_ledger(lambda b: b + b if 'id     = "G-0002"' in b else b)
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("two entries share the id 'G-0002'", r.stderr)
        self.assertEqual(self.check().returncode, 2)

    def test_a_gap_cannot_borrow_another_gaps_scenario(self):
        # Codex on #71: a new gap pointing at G-0002's scenario would get G-0002's result.
        self.edit_ledger(lambda b: b + b.replace('"G-0002"', '"G-0003"')
                         if 'id     = "G-0002"' in b else b)
        r = self.candidate("G-0003")
        self.assertEqual(r.returncode, 2)
        self.assertIn("G-0003 names scenarios/raw-checksum.json, which was written for 'G-0002'",
                      r.stderr)

    def test_refuses_an_artifact_produced_for_another_gap(self):
        f = self.results / "divergence.raw-checksum.json"
        d = json.loads(f.read_text())
        d["gap"] = "G-0001"
        f.write_text(json.dumps(d))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("was produced for gap 'G-0001', but the ledger judges it as G-0002",
                      r.stderr)

    def test_refuses_a_change_to_the_pins(self):
        with open(self.h / "pins.toml", "a") as f:
            f.write("\n# moved\n")
        self.stamp()
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("the change edits pins.toml", r.stderr)

    def test_refuses_an_unpinned_rust_compiler(self):
        self.edit_results(lambda p: p["toolchain"].update(rust_matches_pin=False))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("not the pinned Rust toolchain", r.stderr)

    def test_refuses_a_compiler_pinned_by_another_file(self):
        self.edit_results(lambda p: p["toolchain"].update(pinned_rust="1.99.0"))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("not the pinned Rust toolchain", r.stderr)

    def test_a_refused_ledger_removes_an_earlier_verdict(self):
        self.assertEqual(self.candidate().returncode, 0)
        self.edit_ledger(lambda b: b + b if 'id     = "G-0001"' in b else b)
        self.assertEqual(self.candidate().returncode, 2)
        self.assertFalse((self.results / "candidate-verdict.json").exists())

    def test_a_refused_pins_override_removes_an_earlier_verdict(self):
        self.assertEqual(self.candidate().returncode, 0)
        r = self.check(PARITY_MODE="candidate", PARITY_GAP="G-0002", PINS="other.toml")
        self.assertEqual(r.returncode, 2)
        self.assertFalse((self.results / "candidate-verdict.json").exists())

    def test_refuses_results_from_another_base(self):
        self.edit_results(lambda p: p["candidate"]["harness"].update(base="0" * 40))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("the claims it was judged against have moved", r.stderr)

    def test_needs_a_base(self):
        git(self.h, "update-ref", "-d", "refs/remotes/origin/main")
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("no merge-base with origin/main", r.stderr)

    def test_verdict_provenance_is_the_checked_one(self):
        (self.results / "provenance.json").write_text('{"candidate": "stale"}')
        self.assertEqual(self.candidate().returncode, 0)
        self.assertEqual(self.verdict()["provenance"]["client_rust"]["digest"], digest(self.cr))

    def test_needs_a_declared_gap(self):
        self.assertEqual(self.check(PARITY_MODE="candidate").returncode, 2)
        self.assertEqual(self.candidate("G-9999").returncode, 2)

    def test_refuses_a_changed_client_rust(self):
        (self.cr / "src" / "lib.rs").write_text("// changed after the run\n")
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("client-rust tree with digest", r.stderr)

    def test_refuses_a_changed_harness(self):
        (self.h / "scripts" / "new.sh").write_text("")
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("produced by a harness with digest", r.stderr)

    def test_refuses_a_tree_without_the_pin(self):
        self.edit_results(lambda p: p["candidate"]["client_rust"].update(contains_pin=False))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("does not contain the pin", r.stderr)

    def test_refuses_a_non_candidate_result(self):
        self.edit_results(lambda p: p.update(mode="advisory"))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("candidate mode judges only candidate-mode results", r.stderr)

    def test_oracle_checks_still_apply(self):
        self.edit_results(lambda p: p["cluster"].update(matches_pin=False))
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("but the pin names", r.stderr)

    def test_a_refusal_removes_an_earlier_verdict(self):
        self.assertEqual(self.candidate().returncode, 0)
        (self.cr / "src" / "lib.rs").write_text("// changed\n")
        self.assertEqual(self.candidate().returncode, 2)
        self.assertFalse((self.results / "candidate-verdict.json").exists())


class LedgerCheckStrict(Scratch):
    def as_pinned_run(self):
        """The fixture as a strict run at the pin, from this committed scratch harness."""
        head = subprocess.run(["git", "-C", str(self.h), "rev-parse", "HEAD"], check=True,
                              capture_output=True, text=True).stdout.strip()

        def fn(p):
            p.pop("candidate")
            p.update(mode="strict", strict=True)
            p["harness"].update(rev=head, dirty=False)
            p["client_rust"].update(rev=p["client_rust"]["pinned_rev"], dirty=False,
                                    matches_pin=True)
        self.edit_results(fn)

    def test_a_pinned_run_still_settles_the_ledger(self):
        self.as_pinned_run()
        r = self.check()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("LEDGER: every claim holds at the pinned revisions.", r.stdout)
        self.assertFalse((self.results / "candidate-verdict.json").exists())

    def test_a_pinned_run_still_fails_on_xconverge(self):
        self.as_pinned_run()
        f = self.results / "divergence.raw-checksum.json"
        d = json.loads(f.read_text())
        d["divergences"] = []
        f.write_text(json.dumps(d))
        r = self.check()
        self.assertEqual(r.returncode, 1)
        self.assertIn("flip this entry", r.stderr)

    def test_strict_refuses_candidate_results(self):
        r = self.check()
        self.assertEqual(r.returncode, 2)
        self.assertIn("strict:false", r.stderr)

    def test_advisory_mode_settles_nothing(self):
        self.assertEqual(self.check(PARITY_MODE="advisory").returncode, 2)


class Provenance(Scratch):
    def test_client_rust_must_be_the_tree_cargo_builds(self):
        other = self.tmp / "elsewhere"
        other.mkdir()
        r = self.run_script("provenance.sh", CLIENT_RUST=str(other))
        self.assertEqual(r.returncode, 1)
        self.assertIn("cargo builds the crate under test from", r.stderr)

    def test_the_same_tree_by_another_spelling_is_accepted(self):
        for spelling in (str(self.cr), "../client-rust/", "./../client-rust"):
            r = self.run_script("provenance.sh", CLIENT_RUST=spelling, PARITY_MODE="advisory",
                                PD_ADDRS="127.0.0.1:9")
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertNotIn("cargo builds the crate under test from", r.stderr)

    def test_candidate_refuses_when_cargo_cannot_confirm_the_tree(self):
        # The scratch workspace names member crates it does not have, so `cargo metadata`
        # fails: candidate mode must refuse rather than digest a tree it cannot tie to the build.
        r = self.run_script("provenance.sh", PARITY_MODE="candidate", PD_ADDRS="127.0.0.1:9")
        self.assertEqual(r.returncode, 1)
        self.assertIn("cargo builds tikv-client from", r.stderr)

    def test_candidate_stamps_only_against_pins_toml(self):
        r = self.run_script("provenance.sh", PARITY_MODE="candidate", PINS="other.toml")
        self.assertEqual(r.returncode, 1)
        self.assertIn("candidate mode reads only pins.toml", r.stderr)

    def test_mode_conflicts_and_typos_are_refused(self):
        r = self.run_script("provenance.sh", PARITY_STRICT="1", PARITY_MODE="candidate")
        self.assertEqual(r.returncode, 1)
        self.assertIn("contradicts", r.stderr)
        r = self.run_script("provenance.sh", PARITY_MODE="candidat")
        self.assertEqual(r.returncode, 1)
        self.assertIn("must be strict, candidate or advisory", r.stderr)


class TreeDigest(Scratch):
    def test_what_changes_the_digest(self):
        base = digest(self.cr)
        (self.cr / "target").mkdir()
        (self.cr / ".gitignore").write_text("target\n")
        git(self.cr, "add", ".gitignore")
        git(self.cr, "commit", "-q", "-m", "ignore")
        base = digest(self.cr)
        (self.cr / "target" / "junk").write_text("x")
        self.assertEqual(digest(self.cr), base, "an ignored file must not count")
        (self.cr / "src" / "new.rs").write_text("")
        untracked = digest(self.cr)
        self.assertNotEqual(untracked, base, "an untracked file must count")
        (self.cr / "src" / "new.rs").chmod(0o755)
        self.assertNotEqual(digest(self.cr), untracked, "the executable bit must count")
        (self.cr / "src" / "new.rs").unlink()
        self.assertEqual(digest(self.cr), base)
        (self.cr / "src" / "lib.rs").unlink()
        self.assertNotEqual(digest(self.cr), base, "a deleted tracked file must count")

    def test_only_gitignore_decides_what_is_left_out(self):
        base = digest(self.cr)
        (self.cr / ".git" / "info").mkdir(exist_ok=True)
        (self.cr / ".git" / "info" / "exclude").write_text("hidden.rs\n")
        (self.cr / "src" / "hidden.rs").write_text("")
        self.assertNotEqual(digest(self.cr), base, ".git/info/exclude must not hide a file")

    def test_refuses_any_symlink(self):
        # Even one inside the tree: its target may be ignored (target/), so not digested.
        (self.cr / "target").mkdir()
        (self.cr / "target" / "gen.rs").write_text("")
        (self.cr / ".gitignore").write_text("target\n")
        (self.cr / "src" / "gen.rs").symlink_to("../target/gen.rs")
        r = subprocess.run([sys.executable, "-I", str(DIGEST), str(self.cr)],
                           capture_output=True, text=True)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("is a symlink", r.stderr)

    def test_writes_nothing_into_the_repository(self):
        (self.cr / "src" / "new.rs").write_text("fn x() {}\n")
        before = sorted(p.relative_to(self.cr) for p in (self.cr / ".git").rglob("*"))
        digest(self.cr)
        after = sorted(p.relative_to(self.cr) for p in (self.cr / ".git").rglob("*"))
        self.assertEqual(before, after)


if __name__ == "__main__":
    unittest.main()
