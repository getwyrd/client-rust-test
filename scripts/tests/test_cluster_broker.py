"""scripts/cluster-broker, with Docker and the readiness poll stubbed (no cluster needed).

    python3 -m unittest discover -s scripts/tests     # `make broker-test`, part of `make check`
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BROKER = REPO / "scripts" / "cluster-broker"


class Broker(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="broker-test-"))
        self.calls = self.tmp / "calls.log"
        stub = self.tmp / "docker"
        stub.write_text(
            "#!/usr/bin/env bash\n"
            f'echo "$*" >> {self.calls}\n'
            # `compose ls` answers with one orphan project for the gc test
            'if [[ "$*" == *"compose ls"* ]]; then echo \'[{"Name":"crt-orphan1"},{"Name":"other"}]\'; fi\n'
            '[ -f "$STUB_FAIL_UP" ] && [[ "$*" == *" up "* ]] && { echo boom >&2; exit 1; }\n'
            '[ -f "$STUB_FAIL_DOWN" ] && [[ "$*" == *" down "* ]] && { echo down-boom >&2; exit 1; }\n'
            "exit 0\n")
        stub.chmod(0o755)
        ready = self.tmp / "ready"
        ready.write_text("#!/usr/bin/env bash\n"
                         f'echo "ready $1 $PD_ADDRS $COMPOSE_PROJECT_NAME" >> {self.calls}\n')
        ready.chmod(0o755)
        self.env = {**os.environ, "PARITY_BROKER_DIR": str(self.tmp / "state"),
                    "PARITY_BROKER_DOCKER": str(stub), "PARITY_BROKER_WAIT": str(ready),
                    "PARITY_BROKER_MAX_LEASES": "2", "PARITY_BROKER_PORT_CHECK": "1",
                    "STUB_FAIL_UP": str(self.tmp / "fail-up"),
                    "STUB_FAIL_DOWN": str(self.tmp / "fail-down")}

    def run_broker(self, *args, ok=True):
        p = subprocess.run([sys.executable, str(BROKER), *args], cwd=REPO, env=self.env,
                           text=True, capture_output=True)
        if ok:
            self.assertEqual(p.returncode, 0, p.stderr)
        return p

    def lease(self, *args):
        return json.loads(self.run_broker("lease", *args).stdout)

    def test_two_leases_get_disjoint_ports_projects_and_never_slot_0(self):
        a = self.lease("single")
        b = self.lease("3store")
        self.assertEqual({a["slot"], b["slot"]}, {1, 2})
        self.assertNotEqual(a["project"], b["project"])
        self.assertTrue(set(a["ports"].values()).isdisjoint(b["ports"].values()))
        self.assertEqual(a["pd_addrs"], f"127.0.0.1:{a['ports']['PD_CLIENT_PORT']}")
        self.assertEqual(len([k for k in b["ports"] if k.endswith("_STATUS_PORT")]), 3)
        log = self.calls.read_text()
        self.assertIn(f"ready 1 {a['pd_addrs']} {a['project']}", log)
        self.assertIn(f"ready 3 {b['pd_addrs']} {b['project']}", log)
        env = (Path(self.env["PARITY_BROKER_DIR"]) / "leases" / f"{a['lease_id']}.env").read_text()
        self.assertIn("TIKV_IMAGE=pingcap/tikv:v8.5.5@sha256:", env)
        self.assertIn(f"PD_CLIENT_PORT={a['ports']['PD_CLIENT_PORT']}", env)

    def test_cap_on_concurrent_leases(self):
        self.lease("single"); self.lease("single")
        p = self.run_broker("lease", "single", ok=False)
        self.assertEqual(p.returncode, 1)
        self.assertIn("all 2 lease slots are in use", p.stderr)

    def test_slot_0_is_the_classic_ports_and_exclusive(self):
        if not all(self._free(p) for p in (2379, 2380, 20160, 20180)):
            self.skipTest("a real cluster holds the classic ports")
        a = self.lease("single", "--slot", "0", "--ttl", "0", "--id", "manual-single")
        self.assertEqual(a["pd_addrs"], "127.0.0.1:2379")
        self.assertIsNone(a["expires_at"])
        again = self.lease("single", "--slot", "0", "--ttl", "0", "--id", "manual-single")
        self.assertEqual(again["lease_id"], "manual-single")      # idempotent, like `compose up -d`
        p = self.run_broker("lease", "3store", "--slot", "0", "--id", "manual-3store", ok=False)
        self.assertIn("slot 0 is held by lease manual-single", p.stderr)

    def test_a_busy_port_block_is_skipped(self):
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)   # a just-released lease leaves TIME_WAIT on it
        s.bind(("127.0.0.1", 32000)); s.listen()
        try:
            a = self.lease("single")
        finally:
            s.close()
        self.assertEqual(a["slot"], 2)

    def test_release_runs_down_and_removes_state(self):
        a = self.lease("single")
        self.run_broker("release", a["lease_id"])
        self.assertIn(f"-p {a['project']}", self.calls.read_text().splitlines()[-1])
        self.assertIn("down -v", self.calls.read_text().splitlines()[-1])
        self.assertEqual(list((Path(self.env["PARITY_BROKER_DIR"]) / "leases").iterdir()), [])
        self.assertEqual(self.run_broker("release", a["lease_id"], ok=False).returncode, 1)
        self.run_broker("release", a["lease_id"], "--missing-ok")

    def test_failed_bring_up_releases_the_lease(self):
        (self.tmp / "fail-up").write_text("")
        p = self.run_broker("lease", "single", ok=False)
        self.assertIn("did not come up; released", p.stderr)
        self.assertIn("no leases", self.run_broker("list").stdout)

    def test_gc_releases_expired_and_reports_orphans(self):
        a = self.lease("single", "--ttl", "1s")
        b = self.lease("single", "--ttl", "2h")
        f = Path(self.env["PARITY_BROKER_DIR"]) / "leases" / f"{a['lease_id']}.json"
        rec = json.loads(f.read_text()); rec["expires_at"] = "2000-01-01T00:00:00+00:00"
        f.write_text(json.dumps(rec))
        out = self.run_broker("gc").stdout
        self.assertIn(f"released expired lease {a['lease_id']}", out)
        self.assertIn("orphan compose project crt-orphan1", out)
        self.assertNotIn("other", out.replace("another", ""))
        listing = self.run_broker("list").stdout
        self.assertIn(b["lease_id"], listing)
        self.assertNotIn(a["lease_id"], listing)

    def test_renew_and_env(self):
        a = self.lease("single", "--ttl", "1m")
        self.run_broker("renew", a["lease_id"], "--ttl", "0")
        self.assertIn("never", self.run_broker("list").stdout)
        self.assertEqual(self.run_broker("env", a["lease_id"]).stdout.strip(),
                         f"export PD_ADDRS={a['pd_addrs']}")

    def test_refusals(self):
        self.assertIn("chaos tier", self.run_broker("lease", "chaos", ok=False).stderr)
        self.assertIn("not pinned", self.run_broker("lease", "single", "--tikv", "v1.0.0", ok=False).stderr)
        self.assertIn("bad --ttl", self.run_broker("lease", "single", "--ttl", "soon", ok=False).stderr)
        self.assertIn("bad --id", self.run_broker("lease", "single", "--id", "Bad_ID", ok=False).stderr)

    def test_matrix_version(self):
        a = self.lease("single", "--tikv", "v8.5.8")
        self.assertEqual(a["tikv_version"], "v8.5.8")

    def _expire(self, lid):
        f = Path(self.env["PARITY_BROKER_DIR"]) / "leases" / f"{lid}.json"
        rec = json.loads(f.read_text()); rec["expires_at"] = "2000-01-01T00:00:00+00:00"
        f.write_text(json.dumps(rec))

    # Review on #55: a failed teardown must not make the broker forget the lease.
    def test_failed_down_keeps_the_lease_for_a_retry(self):
        a = self.lease("single")
        (self.tmp / "fail-down").write_text("")
        p = self.run_broker("release", a["lease_id"], ok=False)
        self.assertIn("the lease is kept", p.stderr)
        self.assertIn(a["lease_id"], self.run_broker("list").stdout)
        (self.tmp / "fail-down").unlink()
        self.run_broker("release", a["lease_id"])
        self.assertIn("no leases", self.run_broker("list").stdout)

    # Review on #55: gc claims a lease under the lock, so renew cannot race it.
    def test_gc_claims_before_release_and_retries_a_failed_one(self):
        a = self.lease("single")
        self._expire(a["lease_id"])
        (self.tmp / "fail-down").write_text("")
        self.run_broker("gc", ok=False)
        p = self.run_broker("renew", a["lease_id"], ok=False)
        self.assertIn("is being released", p.stderr)
        (self.tmp / "fail-down").unlink()
        self.assertIn(f"released expired lease {a['lease_id']}", self.run_broker("gc").stdout)

    # Review on #55: a reused id is idempotent only for the same request.
    def test_reused_id_must_match_slot_and_not_be_expired(self):
        a = self.lease("single", "--id", "named", "--ttl", "1m")
        p = self.run_broker("lease", "single", "--id", "named", "--slot", str(a["slot"] + 1), ok=False)
        self.assertIn(f"exists on slot {a['slot']}", p.stderr)
        again = self.lease("single", "--id", "named", "--ttl", "0")
        self.assertIsNone(again["expires_at"])                 # the requested ttl is applied
        self._expire("named")
        p = self.run_broker("lease", "single", "--id", "named", ok=False)
        self.assertIn("has expired", p.stderr)

    # Review on #55: each matrix field must name its own repository.
    def test_pins_check_rejects_swapped_matrix_images(self):
        pins = (REPO / "pins.toml").read_text()
        pd = 'pd_image   = "pingcap/pd:v8.5.8@'
        tk = 'tikv_image = "pingcap/tikv:v8.5.8@'
        swapped = pins.replace(pd, 'pd_image   = "pingcap/tikv:v8.5.8@').replace(tk, 'tikv_image = "pingcap/pd:v8.5.8@')
        self.assertNotEqual(swapped, pins)
        f = self.tmp / "pins.toml"; f.write_text(swapped)
        p = subprocess.run([str(REPO / "scripts" / "check-pins.sh")], cwd=REPO, text=True,
                           capture_output=True, env={**os.environ, "PINS": str(f)})
        self.assertIn("v8.5.8.pd_image", p.stderr + p.stdout)
        self.assertIn("v8.5.8.tikv_image", p.stderr + p.stdout)

    @staticmethod
    def _free(port):
        with socket.socket() as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)   # as the broker does
            try:
                s.bind(("127.0.0.1", port)); return True
            except OSError:
                return False


if __name__ == "__main__":
    unittest.main()
