#!/usr/bin/env python3
"""Focused tests for the production runtime ABI literal inventory gate."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

inventory = load_script("validate-runtime-abi-inventory.py", __file__)


class InventoryTests(unittest.TestCase):
    def validate(self, source: str, relative: str = "lib.rs") -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            src = Path(directory) / "materialized"
            path = src / relative
            path.parent.mkdir(parents=True)
            path.write_text(source)
            return inventory.validate(
                Path(directory) / "unrelated-root",
                [("rue-air", src)],
            )

    def test_production_literal_after_test_module_is_rejected(self) -> None:
        errors = self.validate(
            '#[cfg(test)] mod tests { const OK: &str = "__rue_alloc"; }\n'
            'const DEBT: &str = "__rue_free";\n'
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("'__rue_free'", errors[0])

    def test_line_nested_block_and_doc_comments_are_ignored(self) -> None:
        self.assertEqual(
            self.validate(
                '// "__rue_alloc"\n'
                '/* outer "__rue_free" /* nested "__rue_exit" */ */\n'
                '/// example: "__rue_panic"\n'
                'const OK: &str = "__rue_";\n'
            ),
            [],
        )

    def test_categorical_production_allowances_are_narrow(self) -> None:
        allowed = "\n".join(
            f'const OK_{index}: &str = "{value}";'
            for index, value in enumerate(inventory.ALLOWED_PRODUCTION)
        )
        self.assertEqual(self.validate(allowed), [])
        self.assertTrue(self.validate('const DEBT: &str = "__rue_alloc";'))

    def test_test_paths_and_exact_test_module_spans_are_allowed(self) -> None:
        self.assertEqual(
            self.validate('const FIXTURE: &str = "__rue_alloc";', "tests/calls.rs"),
            [],
        )
        self.assertEqual(
            self.validate(
                '#[cfg(test)]\nmod checks {\n'
                '  fn fixture() { let _ = "__rue_alloc"; }\n'
                '}\n'
            ),
            [],
        )

    def test_buck_materialized_source_mapping_is_stable(self) -> None:
        errors = self.validate('const DEBT: &str = "__rue_alloc";', "nested/file.rs")
        self.assertEqual(len(errors), 1)
        self.assertTrue(errors[0].startswith("crates/rue-air/src/nested/file.rs:1:"))


if __name__ == "__main__":
    unittest.main()
