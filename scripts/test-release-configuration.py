#!/usr/bin/env python3
"""Focused tests for release-configuration validation."""

from __future__ import annotations

import importlib.util
import json
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
                "[rustc]",
                "[rustc, -Copt-level=3, -Clto=thin, --cfg=rue_release_build]",
            ),
            [],
        )
        errors = release_configuration.validate_commands(
            "[rustc, -Clto=thin]", "[rustc, -Copt-level=3]"
        )
        self.assertIn("debug rustc_cfg unexpectedly contains -Clto=thin", errors)
        self.assertIn("release rustc_cfg is missing -Clto=thin", errors)
        self.assertIn("release rustc_cfg is missing --cfg=rue_release_build", errors)

    def test_scaling_workflow_must_build_and_resolve_release_binaries(self) -> None:
        valid = "\n".join(release_configuration.SCALING_RELEASE_SNIPPETS)
        self.assertEqual(
            release_configuration.validate_scaling_workflow(valid),
            [],
        )
        errors = release_configuration.validate_scaling_workflow(
            valid.replace("scripts/rue-bin --target-platforms //platforms:release", "scripts/rue-bin")
        )
        self.assertTrue(any("scripts/rue-bin" in error for error in errors), errors)

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
