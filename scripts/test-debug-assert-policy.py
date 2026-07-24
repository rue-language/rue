#!/usr/bin/env python3
"""Focused tests for the production debug-assertion policy."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-debug-assert-policy.py")
SPEC = importlib.util.spec_from_file_location("debug_assert_policy", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)


class DebugAssertPolicyTests(unittest.TestCase):
    def validate(
        self, files: dict[str, str], allowances: dict[str, policy.Allowance]
    ) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, contents in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            return policy.validate(root, allowances)

    def test_accepts_exact_reviewed_allowance(self) -> None:
        path = "crates/rue-air/src/lib.rs"
        self.assertEqual(
            self.validate(
                {path: "fn f() { debug_assert!(true); }\n"},
                {path: policy.Allowance(1, "redundant test invariant")},
            ),
            [],
        )

    def test_rejects_unreviewed_assertion(self) -> None:
        errors = self.validate(
            {"crates/rue-codegen/src/lib.rs": "debug_assert_eq!(1, 1);\n"}, {}
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("unreviewed", errors[0])

    def test_rejects_ledger_drift_in_either_direction(self) -> None:
        path = "crates/rue-air/src/lib.rs"
        allowance = {path: policy.Allowance(2, "two redundant checks")}
        too_few = self.validate({path: "debug_assert!(true);\n"}, allowance)
        too_many = self.validate(
            {path: "debug_assert!(true);\ndebug_assert!(true);\ndebug_assert!(true);\n"},
            allowance,
        )
        self.assertIn("expects 2, found 1", too_few[0])
        self.assertIn("expects 2, found 3", too_many[0])

    def test_ignores_comments_and_string_literals(self) -> None:
        source = """\
// debug_assert!(false);
/* debug_assert_eq!(1, 2); */
const TEXT: &str = "debug_assert_ne!(1, 1);";
const RAW: &str = r#"debug_assert!(false);"#;
"""
        self.assertEqual(
            self.validate({"crates/rue-codegen/src/lib.rs": source}, {}), []
        )


if __name__ == "__main__":
    unittest.main()
