#!/usr/bin/env python3
"""Unit tests for scripts/validate-lean-toolchain-pin.py."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


def load_gate():
    path = Path(__file__).resolve().parent / "validate-lean-toolchain-pin.py"
    spec = importlib.util.spec_from_file_location("validate_lean_toolchain_pin", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class PinAgreementTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = load_gate()
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def write(self, toolchain: str, defs: str) -> tuple[Path, Path]:
        a = self.dir / "lean-toolchain"
        b = self.dir / "defs.bzl"
        a.write_text(toolchain)
        b.write_text(defs)
        return a, b

    def test_agreeing_pins_pass(self) -> None:
        a, b = self.write("leanprover/lean4:v4.33.1\n", 'LEAN_VERSION = "4.33.1"\n')
        self.assertEqual(self.gate.errors(a, b), [])

    def test_disagreeing_pins_fail(self) -> None:
        a, b = self.write("leanprover/lean4:v4.34.0\n", 'LEAN_VERSION = "4.33.1"\n')
        problems = self.gate.errors(a, b)
        self.assertEqual(len(problems), 1)
        self.assertIn("v4.34.0", problems[0])
        self.assertIn("v4.33.1", problems[0])

    def test_malformed_toolchain_file_fails(self) -> None:
        a, b = self.write("stable\n", 'LEAN_VERSION = "4.33.1"\n')
        self.assertEqual(len(self.gate.errors(a, b)), 1)

    def test_missing_or_duplicate_buck_version_fails(self) -> None:
        a, b = self.write("leanprover/lean4:v4.33.1\n", "# no version here\n")
        self.assertEqual(len(self.gate.errors(a, b)), 1)
        a, b = self.write(
            "leanprover/lean4:v4.33.1\n",
            'LEAN_VERSION = "4.33.1"\nLEAN_VERSION = "4.33.2"\n',
        )
        self.assertEqual(len(self.gate.errors(a, b)), 1)

    def test_main_exit_codes(self) -> None:
        a, b = self.write("leanprover/lean4:v4.33.1\n", 'LEAN_VERSION = "4.33.1"\n')
        self.assertEqual(self.gate.main(["--lean-toolchain", str(a), "--buck-defs", str(b)]), 0)
        a, b = self.write("leanprover/lean4:v4.33.1\n", 'LEAN_VERSION = "4.33.2"\n')
        self.assertEqual(self.gate.main(["--lean-toolchain", str(a), "--buck-defs", str(b)]), 1)


if __name__ == "__main__":
    unittest.main()
