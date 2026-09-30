#!/usr/bin/env python3
"""Unit tests for scripts/validate-lean-trust-list.py."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


def load_gate():
    path = Path(__file__).resolve().parent / "validate-lean-trust-list.py"
    spec = importlib.util.spec_from_file_location("validate_lean_trust_list", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


BUCK = """\
lean_package(
    name = "other-package",
    trust = ["RueCore.elsewhere"],
)

lean_package(
    name = "lean-ruecore",
    module = "RueCore",
    trust = [
{items}
    ],
)
"""

TRUST_MD = """\
# RueCore trust report

| Theorem | Module | Axioms |
| --- | --- | --- |
| `soundness` | `RueCore.Soundness` | `propext`, `Quot.sound` |
| `drop_order` | `RueCore.TraceOrder` | `propext`, `Quot.sound` |
| `Config.stuck_iff` | `RueCore.Step` | *none* |
"""

SOURCE = """\
namespace RueCore

/-- A doc-comment mentioning theorem not_a_decl. -/
theorem soundness : True := trivial

-- theorem commented_out : True := trivial

mutual
theorem drop_order : True := trivial
end

namespace Config
@[simp] protected theorem stuck_iff : True := trivial
end Config

end RueCore
"""

SPEC = """\
def spine : List (Lean.Name × Lean.Name) := [
  -- type safety
  (`RueCore.soundness, ``soundness_stmt),
  (`RueCore.Config.stuck_iff, ``Config.stuck_iff_stmt),
]
"""


class TrustListTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = load_gate()
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.lean = self.dir / "lean"
        (self.lean / "RueCore").mkdir(parents=True)
        (self.lean / "TRUST.md").write_text(TRUST_MD)
        (self.lean / "RueCore" / "Main.lean").write_text(SOURCE)
        (self.lean / "RueCore" / "Spec.lean").write_text(SPEC)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_items(self, items: str) -> list[str]:
        buck = self.dir / "BUCK"
        buck.write_text(BUCK.format(items=items))
        return self.gate.errors(buck, self.lean)

    def run_gate(self, names: list[str]) -> list[str]:
        return self.run_items("\n".join(f'        "{n}",' for n in names))

    def test_current_list_passes(self) -> None:
        self.assertEqual(
            self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff", "RueCore.drop_order"]), []
        )

    def test_renamed_theorem_fails(self) -> None:
        problems = self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff", "RueCore.eval_blocks"])
        self.assertEqual(len(problems), 1)
        self.assertIn("`RueCore.eval_blocks`, which no Lean source", problems[0])

    def test_rename_with_stale_report_still_fails(self) -> None:
        # The report still lists the old name; the sources are what count.
        (self.lean / "TRUST.md").write_text(TRUST_MD + "| `old_name` | `RueCore.Soundness` | *none* |\n")
        problems = self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff", "RueCore.old_name"])
        self.assertEqual(len(problems), 1)
        self.assertIn("`RueCore.old_name`, which no Lean source", problems[0])

    def test_stale_report_missing_a_theorem_fails(self) -> None:
        (self.lean / "TRUST.md").write_text(TRUST_MD.replace("| `drop_order`", "| `drop_order_old`"))
        problems = self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff", "RueCore.drop_order"])
        self.assertEqual(len(problems), 1)
        self.assertIn("missing from the trust report", problems[0])

    def test_commented_out_declaration_does_not_count(self) -> None:
        problems = self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff", "RueCore.commented_out"])
        self.assertEqual(len(problems), 1)
        self.assertIn("`RueCore.commented_out`", problems[0])

    def test_missing_spine_theorem_fails(self) -> None:
        problems = self.run_gate(["RueCore.soundness", "RueCore.drop_order"])
        self.assertEqual(len(problems), 1)
        self.assertIn("spine theorem `RueCore.Config.stuck_iff`", problems[0])

    def test_commented_out_list_entry_does_not_count(self) -> None:
        problems = self.run_items(
            '        "RueCore.soundness",\n        # "RueCore.Config.stuck_iff",\n'
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("spine theorem `RueCore.Config.stuck_iff`", problems[0])

    def test_comment_with_bracket_inside_list(self) -> None:
        self.assertEqual(
            self.run_items(
                '        # see [RUE-1]\n        "RueCore.soundness",\n        "RueCore.Config.stuck_iff",\n'
            ),
            [],
        )

    def test_single_line_list(self) -> None:
        self.assertEqual(self.run_items('        "RueCore.soundness", "RueCore.Config.stuck_iff",'), [])

    def test_unreadable_list_entry_fails(self) -> None:
        problems = self.run_items('        "RueCore.soundness",\n        SPINE_NAMES,\n')
        self.assertEqual(len(problems), 1)
        self.assertIn("cannot read", problems[0])

    def test_unreadable_spine_line_fails(self) -> None:
        (self.lean / "RueCore" / "Spec.lean").write_text(
            SPEC.replace("(`RueCore.soundness, ``soundness_stmt),", "(\n  `RueCore.soundness,\n  ``soundness_stmt),")
        )
        problems = self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff"])
        self.assertEqual(len(problems), 1)
        self.assertIn("cannot read", problems[0])

    def test_duplicate_fails(self) -> None:
        problems = self.run_gate(["RueCore.soundness", "RueCore.soundness", "RueCore.Config.stuck_iff"])
        self.assertEqual(len(problems), 1)
        self.assertIn("twice", problems[0])

    def test_other_package_is_ignored(self) -> None:
        self.assertEqual(self.run_gate(["RueCore.soundness", "RueCore.Config.stuck_iff"]), [])

    def test_missing_package_is_reported(self) -> None:
        buck = self.dir / "BUCK"
        buck.write_text('lean_package(\n    name = "other",\n    trust = [],\n)\n')
        problems = self.gate.errors(buck, self.lean)
        self.assertEqual(len(problems), 1)
        self.assertIn('no `lean_package(name = "lean-ruecore"', problems[0])

    def test_empty_trust_report_is_reported(self) -> None:
        (self.lean / "TRUST.md").write_text("# nothing\n")
        problems = self.run_gate(["RueCore.soundness"])
        self.assertEqual(len(problems), 1)
        self.assertIn("found no theorem rows", problems[0])


if __name__ == "__main__":
    unittest.main()
