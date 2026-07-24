#!/usr/bin/env python3
"""Focused tests for release-configuration validation."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-release-configuration.py")
SPEC = importlib.util.spec_from_file_location("release_configuration", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
release_configuration = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_configuration)


def payload(platform: str, command: str) -> str:
    return json.dumps(
        {
            f"(target: `prelude//rust/tools:rustc_cfg "
            f"(root//platforms:{platform}#abc)`, id: `0`)": {"cmd": command}
        }
    )


class ReleaseConfigurationTests(unittest.TestCase):
    def test_extracts_configured_rustc_action(self) -> None:
        command = "[rustc, -Copt-level=3, -Clto=thin]"
        self.assertEqual(
            release_configuration.rustc_cfg_command(
                payload("release", command), "release"
            ),
            command,
        )

    def test_requires_release_flags_and_forbids_them_in_debug(self) -> None:
        self.assertEqual(
            release_configuration.validate_commands(
                "[rustc]", "[rustc, -Copt-level=3, -Clto=thin]"
            ),
            [],
        )
        errors = release_configuration.validate_commands(
            "[rustc, -Clto=thin]", "[rustc, -Copt-level=3]"
        )
        self.assertIn("debug rustc_cfg unexpectedly contains -Clto=thin", errors)
        self.assertIn("release rustc_cfg is missing -Clto=thin", errors)

    def test_rejects_old_modifier_and_missing_platform(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, expected in release_configuration.RELEASE_CALLERS.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(f"--target-platforms {expected}\n")

            self.assertEqual(release_configuration.validate_callers(root), [])

            (root / "bench.sh").write_text("--modifier //constraints:release\n")
            errors = release_configuration.validate_callers(root)
            self.assertTrue(any("RUE-277 no-op" in error for error in errors))
            self.assertTrue(any("no longer names" in error for error in errors))

    def test_rejects_missing_or_ambiguous_rustc_action(self) -> None:
        with self.assertRaisesRegex(ValueError, "0 matching"):
            release_configuration.rustc_cfg_command("{}", "release")
        duplicate = json.loads(payload("release", "[rustc]"))
        duplicate[
            "(target: `prelude//rust/tools:rustc_cfg "
            "(root//platforms:release#def)`, id: `0`)"
        ] = {"cmd": "[rustc]"}
        with self.assertRaisesRegex(ValueError, "2 matching"):
            release_configuration.rustc_cfg_command(
                json.dumps(duplicate), "release"
            )


if __name__ == "__main__":
    unittest.main()
