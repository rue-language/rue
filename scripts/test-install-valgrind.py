#!/usr/bin/env python3
"""Exercise the bounded Valgrind installer with fake package commands."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("install-valgrind")


class InstallerTests(unittest.TestCase):
    def run_installer(self, apt_status=0, timeout_status=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            (fake_bin / "sudo").write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = -n ]; then shift; fi\n"
                "exec \"$@\"\n"
            )
            (fake_bin / "apt-get").write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$@\" >>\"$FAKE_APT_ARGS\"\n"
                f"exit {apt_status}\n"
            )
            if timeout_status is None:
                (fake_bin / "timeout").write_text(
                    "#!/bin/sh\n"
                    "printf '%s\\n' \"$@\" >\"$FAKE_TIMEOUT_ARGS\"\n"
                    "while [ \"$1\" != \"sudo\" ]; do shift; done\n"
                    "exec \"$@\"\n"
                )
            else:
                (fake_bin / "timeout").write_text(
                    "#!/bin/sh\n"
                    f"exit {timeout_status}\n"
                )
            for command in fake_bin.iterdir():
                command.chmod(0o755)

            env = os.environ.copy()
            env["PATH"] = os.pathsep.join([str(fake_bin), "/usr/bin", "/bin"])
            env["FAKE_APT_ARGS"] = str(root / "apt-args")
            env["FAKE_TIMEOUT_ARGS"] = str(root / "timeout-args")
            result = subprocess.run(
                [str(SCRIPT)], capture_output=True, text=True, env=env, check=False
            )
            apt_args = (
                (root / "apt-args").read_text().splitlines()
                if (root / "apt-args").exists()
                else []
            )
            timeout_args = (
                (root / "timeout-args").read_text().splitlines()
                if (root / "timeout-args").exists()
                else []
            )
            return result, apt_args, timeout_args

    def test_success_runs_bounded_update_then_install(self):
        result, apt_args, timeout_args = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(apt_args.count("update"), 1)
        self.assertEqual(apt_args.count("install"), 1)
        self.assertIn("--no-install-recommends", apt_args)
        self.assertIn("valgrind", apt_args)
        self.assertIn("DPkg::Lock::Timeout=60", apt_args)
        self.assertIn("Acquire::Retries=2", apt_args)
        self.assertIn("Acquire::http::Timeout=30", apt_args)
        self.assertIn("Acquire::https::Timeout=30", apt_args)
        self.assertIn('--kill-after=30s', timeout_args)
        self.assertIn('--signal=TERM', timeout_args)
        self.assertIn('600s', timeout_args)

    def test_non_timeout_apt_failure_is_preserved(self):
        result, _, _ = self.run_installer(apt_status=37)
        self.assertEqual(result.returncode, 37)
        self.assertIn("apt-get update failed (exit 37)", result.stderr)

    def test_timeout_is_124_and_explained(self):
        result, _, _ = self.run_installer(timeout_status=124)
        self.assertEqual(result.returncode, 124)
        self.assertIn("apt-get update timed out", result.stderr)

    def test_forced_kill_status_is_visible(self):
        result, _, _ = self.run_installer(timeout_status=137)
        self.assertEqual(result.returncode, 137)
        self.assertIn("apt-get update failed (exit 137)", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
