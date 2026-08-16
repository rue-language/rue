#!/usr/bin/env python3
"""Focused tests for the production debug-assertion policy."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

policy = load_script("validate-debug-assert-policy.py", __file__)


class DebugAssertPolicyTests(unittest.TestCase):
    def validate(
        self,
        crate: str,
        files: dict[str, str],
        allowances: dict[str, policy.Allowance],
    ) -> list[str]:
        """Run the scoped check over a fixture src/ tree for one crate."""
        with tempfile.TemporaryDirectory() as directory:
            sources = Path(directory)
            for relative, contents in files.items():
                path = sources / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            return policy.validate_crate(crate, sources, allowances)

    def test_accepts_exact_reviewed_allowance(self) -> None:
        self.assertEqual(
            self.validate(
                "rue-air",
                {"lib.rs": "fn f() { debug_assert!(true); }\n"},
                {"crates/rue-air/src/lib.rs": policy.Allowance(1, "redundant test invariant")},
            ),
            [],
        )

    def test_rejects_unreviewed_assertion(self) -> None:
        errors = self.validate(
            "rue-codegen", {"lib.rs": "debug_assert_eq!(1, 1);\n"}, {}
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("unreviewed", errors[0])
        self.assertIn("crates/rue-codegen/src/lib.rs", errors[0])

    def test_rejects_ledger_drift_in_either_direction(self) -> None:
        allowance = {
            "crates/rue-air/src/lib.rs": policy.Allowance(2, "two redundant checks")
        }
        too_few = self.validate(
            "rue-air", {"lib.rs": "debug_assert!(true);\n"}, allowance
        )
        too_many = self.validate(
            "rue-air",
            {"lib.rs": "debug_assert!(true);\ndebug_assert!(true);\ndebug_assert!(true);\n"},
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
        self.assertEqual(self.validate("rue-codegen", {"lib.rs": source}, {}), [])

    def test_scoping_ignores_other_crates_ledger_entries(self) -> None:
        # Checking rue-codegen must not fail on rue-air's allowance: that
        # entry belongs to rue-air's own check.
        self.assertEqual(
            self.validate(
                "rue-codegen",
                {"lib.rs": "fn f() {}\n"},
                {"crates/rue-air/src/lib.rs": policy.Allowance(1, "another crate's")},
            ),
            [],
        )

    def test_allowance_for_unscanned_file_fails_as_drift(self) -> None:
        # An in-scope ledger entry whose file the scan cannot reach (deleted,
        # or outside src/) must fail rather than go silently unchecked.
        errors = self.validate(
            "rue-air",
            {"lib.rs": "fn f() {}\n"},
            {"crates/rue-air/src/missing.rs": policy.Allowance(1, "stale entry")},
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("expects 1, found 0", errors[0])

    def test_missing_or_empty_sources_directory_fails_closed(self) -> None:
        missing = policy.validate_crate(
            "rue-air", Path("/nonexistent/rue-air/src"), {}
        )
        self.assertEqual(len(missing), 1)
        self.assertIn("does not exist", missing[0])

        with tempfile.TemporaryDirectory() as directory:
            empty = policy.validate_crate("rue-air", Path(directory), {})
        self.assertEqual(len(empty), 1)
        self.assertIn("no .rs files", empty[0])

    def project(self, *root_modules: str) -> dict:
        return {"crates": [{"root_module": module} for module in root_modules]}

    def test_ledger_accepts_entries_for_existing_crates(self) -> None:
        self.assertEqual(
            policy.validate_ledger_crates(
                self.project(
                    "crates/rue-air/src/lib.rs",
                    "third-party/vendor/lasso/src/lib.rs",
                ),
                {"crates/rue-air/src/lib.rs": policy.Allowance(1, "reviewed")},
            ),
            [],
        )

    def test_ledger_rejects_orphaned_crate_entry(self) -> None:
        # A deleted crate emits no per-crate check, so its surviving ledger
        # entries would be enforced by nothing; the ledger mode fails on them.
        errors = policy.validate_ledger_crates(
            self.project("crates/rue-air/src/lib.rs"),
            {"crates/rue-retired/src/lib.rs": policy.Allowance(1, "orphaned")},
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("rue-retired", errors[0])
        self.assertIn("orphaned", errors[0])

    def test_ledger_rejects_malformed_key_and_empty_project(self) -> None:
        malformed = policy.validate_ledger_crates(
            self.project("crates/rue-air/src/lib.rs"),
            {"scripts/foo.py": policy.Allowance(1, "not a crate path")},
        )
        self.assertEqual(len(malformed), 1)
        self.assertIn("crates/<name>/", malformed[0])

        # Third-party-only (or empty) project data means the first-party
        # detection is broken, never that the ledger is clean.
        empty = policy.validate_ledger_crates(
            self.project("third-party/vendor/lasso/src/lib.rs"),
            {"crates/rue-air/src/lib.rs": policy.Allowance(1, "reviewed")},
        )
        self.assertEqual(len(empty), 1)
        self.assertIn("no first-party crates", empty[0])

    def test_real_ledger_names_only_first_party_shapes(self) -> None:
        # The checked-in ledger must keep the crates/<name>/src/... shape the
        # per-crate checks reconstruct their keys in.
        for path in policy.ALLOWANCES:
            parts = path.split("/")
            self.assertEqual(parts[0], "crates", path)
            self.assertEqual(parts[2], "src", path)


if __name__ == "__main__":
    unittest.main()
