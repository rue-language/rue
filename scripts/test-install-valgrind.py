#!/usr/bin/env python3
"""Exercise the bounded Valgrind installer with fake package commands."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("install-valgrind")


class InstallerTests(unittest.TestCase):
    def run_installer(self, apt_status=0, timeout_status=None, source_layout="deb822"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fake_bin = root / "bin"
            fake_bin.mkdir()
            apt_root = root / "etc" / "apt"
            source_parts = apt_root / "sources.list.d"
            source_parts.mkdir(parents=True)
            (source_parts / "google-chrome.list").write_text(
                "deb https://dl.google.com/linux/chrome/deb stable main\n"
            )
            if source_layout == "deb822":
                (source_parts / "ubuntu.sources").write_text(
                    "Types: deb\n"
                    "URIs: http://azure.archive.ubuntu.com/ubuntu/\n"
                    "Suites: noble noble-updates noble-backports\n"
                    "Components: main universe restricted multiverse\n"
                    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n"
                    "Types: deb\n"
                    "URIs: http://security.ubuntu.com/ubuntu/\n"
                    "Suites: noble-security\n"
                    "Components: main universe restricted multiverse\n"
                    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
                )
            elif source_layout == "deb822-reordered":
                (source_parts / "ubuntu.sources").write_text(
                    "URIs: http://azure.archive.ubuntu.com/ubuntu/   \n"
                    "# Comments do not end a continued URI field.\n"
                    "  https://security.ubuntu.com/ubuntu/\n"
                    "Suites: noble noble-security\n"
                    "Components: main universe restricted multiverse\n"
                    "Types: deb\n"
                    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n"
                    "Types: deb\n"
                    "URIs: http://archive.ubuntu.com/ubuntu/\n"
                    "# This continuation makes the whole stanza third-party.\n"
                    "  https://dl.google.com/linux/chrome/deb\n"
                    "Suites: noble\n"
                    "Components: main\n"
                )
            elif source_layout in ("deb822-mirror", "deb822-mirror-third-party"):
                (apt_root / "apt-mirrors.txt").write_text(
                    "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n"
                    + (
                        "https://dl.google.com/linux/chrome/deb\tpriority:2\n"
                        if source_layout == "deb822-mirror-third-party"
                        else "https://archive.ubuntu.com/ubuntu/\tpriority:2\n"
                    )
                    + "https://security.ubuntu.com/ubuntu/\tpriority:3\n"
                )
                (source_parts / "ubuntu.sources").write_text(
                    "Types: deb\n"
                    "URIs: mirror+file:/etc/apt/apt-mirrors.txt\n"
                    "Suites: noble noble-updates noble-security\n"
                    "Components: main universe restricted multiverse\n"
                    "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
                )
            elif source_layout == "classic":
                (apt_root / "sources.list").write_text(
                    "# deb http://archive.ubuntu.com/ubuntu comment-only main\n"
                    "deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] http://azure.archive.ubuntu.com/ubuntu noble main universe\n"
                    "deb http://security.ubuntu.com/ubuntu noble-security main universe\n"
                    "deb [arch=amd64] https://dl.google.com/linux/chrome/deb stable main http://archive.ubuntu.com/ubuntu\n"
                    "deb https://dl.google.com/linux/chrome/deb stable main\n"
                )
            elif source_layout != "missing":
                raise AssertionError(f"unknown source layout: {source_layout}")
            (fake_bin / "sudo").write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = -n ]; then shift; fi\n"
                "exec \"$@\"\n"
            )
            (fake_bin / "apt-get").write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$@\" >>\"$FAKE_APT_ARGS\"\n"
                "source=\"\"\n"
                "previous=\"\"\n"
                "for argument in \"$@\"; do\n"
                "  if [ \"$previous\" = -o ] && [ \"${argument#Dir::Etc::sourcelist=}\" != \"$argument\" ]; then\n"
                "    source=\"${argument#Dir::Etc::sourcelist=}\"\n"
                "  fi\n"
                "  previous=\"$argument\"\n"
                "done\n"
                "if [ -n \"${FAKE_APT_SOURCE_TRACE:-}\" ]; then\n"
                "  printf 'SOURCE:%s\\n' \"$source\" >>\"$FAKE_APT_SOURCE_TRACE\"\n"
                "  cat \"$source\" >>\"$FAKE_APT_SOURCE_TRACE\"\n"
                "  printf '%s\\n' END-SOURCE >>\"$FAKE_APT_SOURCE_TRACE\"\n"
                "fi\n"
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
            env["FAKE_APT_SOURCE_TRACE"] = str(root / "apt-sources")
            env["RUE_APT_ETC_DIR"] = str(apt_root)
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
            source_trace = (
                (root / "apt-sources").read_text()
                if (root / "apt-sources").exists()
                else ""
            )
            return result, apt_args, timeout_args, source_trace

    def test_success_runs_bounded_update_then_install(self):
        result, apt_args, timeout_args, _ = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(apt_args.count("update"), 1)
        self.assertEqual(apt_args.count("install"), 1)
        self.assertIn("--no-install-recommends", apt_args)
        self.assertIn("valgrind", apt_args)
        self.assertIn("DPkg::Lock::Timeout=60", apt_args)
        self.assertIn("Acquire::Retries=2", apt_args)
        self.assertIn("Acquire::http::Timeout=30", apt_args)
        self.assertIn("Acquire::https::Timeout=30", apt_args)
        source_args = [
            argument.split("=", 1)[1]
            for argument in apt_args
            if argument.startswith("Dir::Etc::sourcelist=")
        ]
        self.assertEqual(len(source_args), 2)
        self.assertEqual(source_args[0], source_args[1])
        self.assertIn("Dir::Etc::sourceparts=-", apt_args)
        self.assertIn('--kill-after=30s', timeout_args)
        self.assertIn('--signal=TERM', timeout_args)
        self.assertIn('600s', timeout_args)

    def test_non_timeout_apt_failure_is_preserved(self):
        result, _, _, _ = self.run_installer(apt_status=37)
        self.assertEqual(result.returncode, 37)
        self.assertIn("apt-get update failed (exit 37)", result.stderr)

    def test_timeout_is_124_and_explained(self):
        result, _, _, _ = self.run_installer(timeout_status=124)
        self.assertEqual(result.returncode, 124)
        self.assertIn("apt-get update timed out", result.stderr)

    def test_forced_kill_status_is_visible(self):
        result, _, _, _ = self.run_installer(timeout_status=137)
        self.assertEqual(result.returncode, 137)
        self.assertIn("apt-get update failed (exit 137)", result.stderr)

    def test_deb822_fixture_excludes_third_party_sources(self):
        result, apt_args, _, source_trace = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(source_trace.count("SOURCE:"), 2)
        self.assertIn("azure.archive.ubuntu.com", source_trace)
        self.assertIn("security.ubuntu.com", source_trace)
        self.assertNotIn("dl.google.com", source_trace)
        self.assertEqual(apt_args.count("Dir::Etc::sourceparts=-"), 2)

    def test_deb822_parser_handles_reordered_and_continued_uris(self):
        result, _, _, source_trace = self.run_installer(source_layout="deb822-reordered")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("azure.archive.ubuntu.com", source_trace)
        self.assertIn("security.ubuntu.com", source_trace)
        self.assertNotIn("dl.google.com", source_trace)

    def test_deb822_runner_mirror_file_is_accepted(self):
        result, _, _, source_trace = self.run_installer(source_layout="deb822-mirror")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("mirror+file:/etc/apt/apt-mirrors.txt", source_trace)

    def test_deb822_runner_mirror_file_must_be_official(self):
        result, apt_args, timeout_args, _ = self.run_installer(
            source_layout="deb822-mirror-third-party"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no official Ubuntu package source found", result.stderr)
        self.assertEqual(apt_args, [])
        self.assertEqual(timeout_args, [])

    def test_classic_sources_list_is_filtered_and_reused(self):
        result, apt_args, _, source_trace = self.run_installer(source_layout="classic")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(source_trace.count("SOURCE:"), 2)
        self.assertIn("azure.archive.ubuntu.com", source_trace)
        self.assertIn("security.ubuntu.com", source_trace)
        self.assertNotIn("dl.google.com", source_trace)
        self.assertNotIn("comment-only", source_trace)
        source_args = [
            argument.split("=", 1)[1]
            for argument in apt_args
            if argument.startswith("Dir::Etc::sourcelist=")
        ]
        self.assertTrue(source_args[0].endswith("/ubuntu.list"))

    def test_missing_ubuntu_source_fails_before_apt(self):
        result, apt_args, timeout_args, _ = self.run_installer(source_layout="missing")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no official Ubuntu package source found", result.stderr)
        self.assertEqual(apt_args, [])
        self.assertEqual(timeout_args, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
