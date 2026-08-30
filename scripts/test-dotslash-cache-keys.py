#!/usr/bin/env python3
"""Focused tests for the dotslash cache-key policy (RUE-1854)."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

keys = load_script("validate-dotslash-cache-keys.py", __file__)


def cache_step(key: str) -> str:
    return (
        "jobs:\n  build:\n    steps:\n"
        "      - uses: actions/cache@v4\n"
        "        with:\n"
        "          path: |\n            ~/.cache/dotslash\n"
        f"          key: {key}\n"
        "          restore-keys: |\n            dotslash-linux-x64-\n"
    )


class DotslashCacheKeyTests(unittest.TestCase):
    def validate(self, workflow: str) -> tuple[list[str], int]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ci.yml"
            path.write_text(workflow)
            return keys.validate(path)

    def test_rejects_hashing_the_wrapper(self) -> None:
        # The exact regression: the wrapper does not change on a pin bump, so
        # the key stays put and the stale store is never replaced.
        errors, found = self.validate(
            cache_step("dotslash-linux-x64-${{ hashFiles('buck2') }}")
        )
        self.assertEqual(found, 1)
        self.assertEqual(len(errors), 2)
        self.assertIn("wrapper", errors[0])
        self.assertIn("buck2-bin", errors[1])

    def test_accepts_hashing_the_pinned_manifest(self) -> None:
        errors, found = self.validate(
            cache_step("dotslash-linux-x64-${{ hashFiles('buck2-bin') }}")
        )
        self.assertEqual((errors, found), ([], 1))

    def test_accepts_additional_tool_manifests(self) -> None:
        # affected-targets caches btd's binary in the same store, so its key
        # legitimately hashes both manifests.
        errors, found = self.validate(
            cache_step("dotslash-linux-x64-${{ hashFiles('buck2-bin', 'btd') }}")
        )
        self.assertEqual((errors, found), ([], 1))

    def test_rejects_a_key_hashing_nothing(self) -> None:
        errors, found = self.validate(cache_step("dotslash-linux-x64-v1"))
        self.assertEqual(found, 1)
        self.assertEqual(len(errors), 1)
        self.assertIn("hashes no file", errors[0])

    def test_accepts_a_matrix_scoped_key(self) -> None:
        errors, found = self.validate(
            cache_step("dotslash-${{ matrix.name }}-${{ hashFiles('buck2-bin') }}")
        )
        self.assertEqual((errors, found), ([], 1))

    def test_ignores_unrelated_cache_keys(self) -> None:
        errors, found = self.validate(
            cache_step("cargo-registry-${{ hashFiles('Cargo.lock') }}")
        )
        self.assertEqual((errors, found), ([], 0))

    def test_directory_argument_covers_every_workflow(self) -> None:
        # The Buck gate passes a directory so a workflow added later is
        # checked without editing the gate.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ci.yml").write_text(
                cache_step("dotslash-linux-x64-${{ hashFiles('buck2-bin') }}")
            )
            (root / "later.yaml").write_text(
                cache_step("dotslash-macos-arm64-${{ hashFiles('buck2') }}")
            )
            found = keys.workflows_in(root)
            self.assertEqual([path.name for path in found], ["ci.yml", "later.yaml"])
            errors = [error for path in found for error in keys.validate(path)[0]]
            self.assertEqual(len(errors), 2)

    def test_reports_every_offending_key(self) -> None:
        workflow = cache_step(
            "dotslash-linux-x64-${{ hashFiles('buck2') }}"
        ) + cache_step("dotslash-macos-arm64-${{ hashFiles('buck2') }}")
        errors, found = self.validate(workflow)
        self.assertEqual(found, 2)
        self.assertEqual(len(errors), 4)


if __name__ == "__main__":
    unittest.main()
