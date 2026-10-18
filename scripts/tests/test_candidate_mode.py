"""Candidate mode: scripts/provenance.sh, scripts/ledger-check.sh and scripts/tree-digest.py.

    python3 -m unittest discover -s scripts/tests     # `make broker-test`, part of `make check`

No cluster: ledger-check runs over real results from a candidate run
(fixtures/candidate-run), copied into a scratch harness whose digests they are rewritten
to match. Each test then changes one thing and checks the verdict or the refusal.
"""

from __future__ import annotations

import json
import os
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

        def fn(p):
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


class LedgerCheckCandidate(Scratch):
    def test_reports_the_declared_gap_and_holds_the_rest(self):
        r = self.candidate()
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("CANDIDATE — not evidence. G-0002: XDIVERGE.", r.stdout)
        self.assertNotIn("every claim holds at the pinned", r.stdout)
        v = self.verdict()
        self.assertEqual((v["gap"], v["outcome"], v["others_hold"]), ("G-0002", "XDIVERGE", True))
        self.assertEqual(v["others"], {"G-0001": "HOLDS"})

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
        f = self.results / "divergence.orphaned-lock-resolution.json"
        d = json.loads(f.read_text())
        d["divergences"] = [{"step": "s", "path": "p", "oracle": "a", "subject": "b"}]
        f.write_text(json.dumps(d))
        r = self.candidate()
        self.assertEqual(r.returncode, 1, r.stdout)
        self.assertIn("G-0001 NEW GAP", r.stderr)
        v = self.verdict()
        self.assertEqual((v["outcome"], v["others_hold"]), ("XDIVERGE", False))

    def test_needs_a_declared_gap(self):
        self.assertEqual(self.check(PARITY_MODE="candidate").returncode, 2)
        self.assertEqual(self.candidate("G-9999").returncode, 2)

    def test_refuses_a_changed_client_rust(self):
        (self.cr / "src" / "lib.rs").write_text("// changed after the run\n")
        r = self.candidate()
        self.assertEqual(r.returncode, 2)
        self.assertIn("client-rust tree with digest", r.stderr)

    def test_refuses_a_changed_harness(self):
        (self.h / "scenarios" / "new.json").write_text("{}")
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

    def test_the_same_tree_by_another_spelling_is_accepted_past_that_check(self):
        r = self.run_script("provenance.sh", CLIENT_RUST=str(self.cr), PARITY_MODE="bogus")
        # The CLIENT_RUST check passed; the run then stops on the bad mode, before any I/O.
        self.assertNotIn("cargo builds the crate under test from", r.stderr)

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

    def test_writes_nothing_into_the_repository(self):
        (self.cr / "src" / "new.rs").write_text("fn x() {}\n")
        before = sorted(p.relative_to(self.cr) for p in (self.cr / ".git").rglob("*"))
        digest(self.cr)
        after = sorted(p.relative_to(self.cr) for p in (self.cr / ".git").rglob("*"))
        self.assertEqual(before, after)


if __name__ == "__main__":
    unittest.main()
