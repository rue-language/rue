#!/usr/bin/env python3
"""Unit tests for rue-program-derive-manifest.py (ADR-0070 / RUE-1404).

Correctness here IS hermeticity: the derive script owns the declared-boundary
check and the machine-stable re-anchoring, so these tests pin its set
arithmetic directly — the fixture targets exercise it end to end but cannot
distinguish "boundary enforced" from "boundary accidentally never violated".

The load-bearing case is relocation invariance: two envelopes recording the
same logical scan under different absolute checkout roots must derive
byte-identical manifests, because the scan's cache key is machine-independent
and its result crosses machines (ADR-0070, "Generating the manifest").
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "rue-program-derive-manifest.py")


def envelope(root_abs, std_abs, accepted, absent):
    return {
        "version": 1,
        "status": "complete",
        "revision": "test",
        "context": {
            "import_policy_version": 1,
            "epoch": "1",
            "project_root": root_abs,
            "std_root": std_abs,
            "read_policy_revision": "unrestricted",
        },
        "topology": {"root": "main.rue", "records": [], "cycles": []},
        "observations": [
            {
                "request": {"requested_path": path},
                "outcome": {"status": "absent"},
            }
            for path in absent
        ],
        "accepted_reads": [{"requested_path": path} for path in accepted],
    }


class DeriveManifestTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.cwd = self.dir.name
        # A fake std tree the script walks for the unconditional union.
        os.makedirs(os.path.join(self.cwd, "stdout-dir/std"))
        for name in ("_std.rue", "option.rue"):
            with open(os.path.join(self.cwd, "stdout-dir/std", name), "w") as handle:
                handle.write("// std\n")
        os.makedirs(os.path.join(self.cwd, "out"))

    def tearDown(self):
        self.dir.cleanup()

    def run_derive(self, env, srcs, out="out/sources.manifest", extra=()):
        envelope_path = os.path.join(self.cwd, "envelope.json")
        with open(envelope_path, "w") as handle:
            json.dump(env, handle)
        srcs_path = os.path.join(self.cwd, "srcs.list")
        with open(srcs_path, "w") as handle:
            handle.write("\n".join(srcs) + "\n")
        return subprocess.run(
            [
                sys.executable,
                SCRIPT,
                "--envelope",
                envelope_path,
                "--root",
                "prog/main.rue",
                "--srcs-list",
                srcs_path,
                "--std-dir",
                "stdout-dir/std",
                "--out",
                out,
                *extra,
            ],
            cwd=self.cwd,
            capture_output=True,
            text=True,
        )

    def read_out(self, out="out/sources.manifest"):
        with open(os.path.join(self.cwd, out)) as handle:
            return handle.read()

    def test_accepted_absent_and_std_are_all_declared(self):
        env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/checkout/prog/main.rue", "/checkout/prog/sub/types.rue"],
            absent=["/checkout/prog/sub/shared.rue"],
        )
        result = self.run_derive(env, ["prog/main.rue", "prog/sub/types.rue"])
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = self.read_out()
        # Accepted reads, the absent arm, and every std file — all relative to
        # the manifest's own directory (out/).
        self.assertIn("../prog/main.rue", manifest)
        self.assertIn("../prog/sub/shared.rue", manifest)
        self.assertIn("../stdout-dir/std/option.rue", manifest)
        self.assertNotIn("/checkout", manifest)

    def test_out_of_srcs_read_fails_the_build(self):
        env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/checkout/prog/main.rue", "/checkout/prog/extra.rue"],
            absent=[],
        )
        result = self.run_derive(env, ["prog/main.rue"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("prog/extra.rue", result.stderr)
        self.assertIn("does not declare", result.stderr)

    def test_read_outside_both_roots_fails_the_build(self):
        env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/somewhere/else.rue"],
            absent=[],
        )
        result = self.run_derive(env, ["prog/main.rue"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("/somewhere/else.rue", result.stderr)

    def test_std_reads_are_allowed_without_declaration(self):
        env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/checkout/prog/main.rue", "/checkout/stdroot/option.rue"],
            absent=[],
        )
        result = self.run_derive(env, ["prog/main.rue"])
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_relocation_invariance(self):
        """Same logical scan under two checkout roots -> identical bytes."""
        manifests = []
        for fake_root in ("/ci/runner/work/rue", "/home/dev/src/rue-checkout"):
            env = envelope(
                fake_root + "/prog",
                fake_root + "/stdlocation",
                accepted=[fake_root + "/prog/main.rue"],
                absent=[fake_root + "/prog/helper.rue"],
            )
            out = "out/manifest-" + fake_root.replace("/", "_")
            result = self.run_derive(env, ["prog/main.rue"], out=out)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifests.append(self.read_out(out))
        self.assertEqual(manifests[0], manifests[1])

    def test_incomplete_envelope_is_rejected(self):
        env = envelope("/c/prog", None, accepted=[], absent=[])
        env["status"] = "incomplete"
        result = self.run_derive(env, ["prog/main.rue"])
        self.assertEqual(result.returncode, 1)
        self.assertIn("incomplete", result.stderr)

    def test_expect_violation_succeeds_only_on_exact_match(self):
        env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/checkout/prog/main.rue", "/checkout/prog/extra.rue"],
            absent=[],
        )
        ok = self.run_derive(
            env,
            ["prog/main.rue"],
            out="out/marker",
            extra=["--expect-violation", "prog/extra.rue"],
        )
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("boundary-violation-detected", self.read_out("out/marker"))

        wrong = self.run_derive(
            env,
            ["prog/main.rue"],
            out="out/marker2",
            extra=["--expect-violation", "prog/other.rue"],
        )
        self.assertEqual(wrong.returncode, 1)

        clean_env = envelope(
            "/checkout/prog",
            "/checkout/stdroot",
            accepted=["/checkout/prog/main.rue"],
            absent=[],
        )
        clean = self.run_derive(
            clean_env,
            ["prog/main.rue"],
            out="out/marker3",
            extra=["--expect-violation", "prog/extra.rue"],
        )
        self.assertEqual(clean.returncode, 1)


if __name__ == "__main__":
    unittest.main()
