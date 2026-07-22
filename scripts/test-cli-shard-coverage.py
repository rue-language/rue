#!/usr/bin/env python3
"""Focused tests for the CLI-shard coverage gate (RUE-1116)."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("validate-cli-shard-coverage.py")
SPEC = importlib.util.spec_from_file_location("cli_shard_coverage", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
coverage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coverage)


def buck(count: int, shard_targets: list[int]) -> str:
    lines = [f"CLI_TEST_SHARD_COUNT = {count}", ""]
    for shard in shard_targets:
        lines.append(f'sh_test(name = "cli-tests-shard-{shard}")')
    return "\n".join(lines) + "\n"


def workflow(shards_by_platform: dict[str, list[int]], caldera_platforms: list[str]) -> str:
    lines = ["jobs:", "  platform-corpus:", "    strategy:", "      matrix:", "        include:"]
    for platform in caldera_platforms:
        lines.append(f"          - check_name: {platform}-cli-caldera")
    for platform, shards in shards_by_platform.items():
        for shard in shards:
            lines.append(f"          - target: //:cli-tests-shard-{shard}")
            lines.append(f"            check_name: {platform}-cli-shard-{shard}")
    return "\n".join(lines) + "\n"


class CliShardCoverageTests(unittest.TestCase):
    def validate(self, buck_text: str, workflow_text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            buck_path = Path(directory) / "BUCK"
            workflow_path = Path(directory) / "ci.yml"
            buck_path.write_text(buck_text)
            workflow_path.write_text(workflow_text)
            return coverage.validate(buck_path, workflow_path)

    def test_valid_mirrored_shards_pass(self) -> None:
        platforms = ["linux-x64", "linux-arm64", "macos"]
        errors = self.validate(
            buck(2, [0, 1]),
            workflow({p: [0, 1] for p in platforms}, platforms),
        )
        self.assertEqual(errors, [])

    def test_loop_generated_shards_pass(self) -> None:
        # The real BUCK generates shard targets with a range() comprehension
        # rather than literal names; that form must validate.
        buck_text = (
            "CLI_TEST_SHARD_COUNT = 3\n"
            '[sh_test(name = "cli-tests-shard-{}".format(_s)) '
            "for _s in range(CLI_TEST_SHARD_COUNT)]\n"
        )
        errors = self.validate(
            buck_text,
            workflow({"linux-x64": [0, 1, 2]}, ["linux-x64"]),
        )
        self.assertEqual(errors, [])

    def test_no_shard_targets_at_all_fails(self) -> None:
        errors = self.validate(
            "CLI_TEST_SHARD_COUNT = 2\n# no shard targets here\n",
            workflow({"linux-x64": [0, 1]}, ["linux-x64"]),
        )
        self.assertTrue(any("found no cli-tests-shard targets" in e for e in errors), errors)

    def test_platform_prefix_is_captured_whole(self) -> None:
        # A multi-segment platform name (linux-arm64) must not be truncated.
        errors = self.validate(
            buck(2, [0, 1]),
            workflow({"linux-arm64": [0, 1]}, ["linux-arm64"]),
        )
        self.assertEqual(errors, [])

    def test_shard_missing_from_matrix_fails(self) -> None:
        errors = self.validate(
            buck(4, [0, 1, 2, 3]),
            workflow({"linux-x64": [0, 1, 2]}, ["linux-x64"]),
        )
        self.assertTrue(any("missing shards [3]" in e for e in errors), errors)

    def test_extra_shard_in_matrix_fails(self) -> None:
        errors = self.validate(
            buck(2, [0, 1]),
            workflow({"linux-x64": [0, 1, 2]}, ["linux-x64"]),
        )
        self.assertTrue(any("unexpected shards [2]" in e for e in errors), errors)

    def test_buck_shard_targets_must_match_count(self) -> None:
        errors = self.validate(
            buck(4, [0, 1, 2]),  # count says 4 but only 3 targets defined
            workflow({"linux-x64": [0, 1, 2, 3]}, ["linux-x64"]),
        )
        self.assertTrue(any("expected exactly 0..3" in e for e in errors), errors)

    def test_platform_with_shards_but_no_caldera_fails(self) -> None:
        errors = self.validate(
            buck(1, [0]),
            # 'ghost' runs shards but has no cli-caldera job.
            workflow({"linux-x64": [0], "ghost": [0]}, ["linux-x64"]),
        )
        self.assertTrue(any("ghost" in e and "inconsistent" in e for e in errors), errors)

    def test_missing_shard_count_is_reported(self) -> None:
        errors = self.validate(
            'sh_test(name = "cli-tests-shard-0")\n',
            workflow({"linux-x64": [0]}, ["linux-x64"]),
        )
        self.assertTrue(any("CLI_TEST_SHARD_COUNT is not defined" in e for e in errors), errors)


if __name__ == "__main__":
    unittest.main()
