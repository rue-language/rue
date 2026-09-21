#!/usr/bin/env python3
"""Unit tests for scripts/validate-lean-xref-index.py."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


def load_gate():
    path = Path(__file__).resolve().parent / "validate-lean-xref-index.py"
    spec = importlib.util.spec_from_file_location("validate_lean_xref_index", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


CALCULUS = """# The calculus

## 2. Abstract syntax

```
Types
  T ::= int(w, s)              -- integer of width w
      | bool

Places
  p ::= x                      -- a local binding
      | p . f                  -- field projection

Expressions
  e ::= lit                    -- literal
      | p                      -- a place used in VALUE context
                               --   (this continuation line is part of the gloss)

Argument forms
  a ::= e                      -- not inventoried: the fragment has no calls
```

## 5. Static semantics

### 5.1 Use and copy

```
  premise
  ───────────────────────── (Use-Copy)
  conclusion

  premise
  ───────────────────────── (Use-Move)
  conclusion
```

### 5.2 Assignment

### 5.5 Control flow and the branch join

```
  ───────────────────────── (If)
```

## 6. Dynamic semantics

### 6.3 Literals and the use of a place

```
  ───────────────────────── (D-Use-Copy)
  ─────────────── (D-Let)     -- a trailing comment
```

### 6.7 `let`

## 7. Soundness

```
  ───────────────────────── (Not-A-Rule-Here)
```
"""

STATICS = """import RueCore.Syntax

/-!
# RueCore.Statics — typing (§5)
-/

namespace RueCore

/-- One context entry (§5). -/
structure Entry where
  ty : Nat
deriving Repr

/-- Re-mark an entry (helper). -/
def Entry.setSt (en : Entry) : Entry := en

/-- `Γ ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5). (Use-Copy) is §5.1. -/
inductive Typed : Nat → Prop where
  | intLit : Typed 0
  /-- (Use-Copy): copies; Σ unchanged (`3.8:5`). -/
  | useCopy : Typed 1
  /-- (If): the §5.5 join (`3.8:50`, `3.8:73`). -/
  | ite : Typed 2

/-- The skeleton (helper). -/
theorem Typed.skel : True := trivial

/-- Attributed, on one line (helper). -/
@[simp] theorem Typed.attr_inline : True := trivial

/-- Attributed, on two lines (§5.1). -/
@[simp, inline]
theorem Typed.attr_split : True := trivial

end RueCore
"""

DYNAMICS = """/-!
# RueCore.Dynamics — the machine (§6)
-/

namespace RueCore

/-- The interpreter: `use` is (D-Use-Copy) (§6.3); `letIn` is (D-Let)
(§6.7). -/
def eval : Nat := 0

end RueCore
"""

EXAMPLES = """/-!
# Examples (`xref: examples`)
-/

namespace RueCore.Examples

/-- A program with no citation. -/
def scalars : Nat := 0

def uncommented : Nat := 1

/-- Exercises (Use-Copy). -/
def cited : Nat := 2

end RueCore.Examples
"""


class GateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gate = load_gate()
        # The §2 mapping is a table about the real calculus; the fixture gets
        # its own, so these tests exercise the checks rather than the rows.
        self.gate.SYNTAX_FORMS = {
            ("T", "int(w, s)"): ("partial", ["Typed.useCopy"], "one width only"),
            ("T", "bool"): ("yes", ["Typed.intLit"], ""),
            ("p", "x"): ("yes", ["Entry.setSt"], "whole bindings only"),
            ("p", "p . f"): ("no", [], "no projections"),
            ("e", "lit"): ("yes", ["Typed.intLit"], ""),
            ("e", "p"): ("no", [], "no uses yet"),
        }
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.lean = self.root / "lean"
        (self.lean / "RueCore").mkdir(parents=True)
        self.calculus = self.root / "calculus.md"
        self.calculus.write_text(CALCULUS)
        self.write("Statics.lean", STATICS)
        self.write("Dynamics.lean", DYNAMICS)
        self.write("Examples.lean", EXAMPLES)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def write(self, name: str, text: str) -> None:
        (self.lean / "RueCore" / name).write_text(text)

    def collect(self):
        return self.gate.collect(self.lean, self.calculus)

    def test_calculus_inventory_is_only_labeled_rules_of_5_and_6(self) -> None:
        calculus = self.gate.parse_calculus(self.calculus)
        self.assertEqual(calculus.rule_order, ["Use-Copy", "Use-Move", "If", "D-Use-Copy", "D-Let"])
        self.assertEqual(calculus.rules["D-Let"], "6.3")
        self.assertEqual(
            [n for n, _, _ in calculus.sections],
            ["2", "5", "5.1", "5.2", "5.5", "6", "6.3", "6.7", "7"],
        )

    def test_declarations_constructors_and_citations(self) -> None:
        modules, calculus, errors = self.collect()
        self.assertEqual(errors, [])
        statics = next(m for m in modules if m.name == "RueCore.Statics")
        names = {d.name: d for d in statics.declarations}
        self.assertIn("RueCore.Typed", names)
        self.assertIn("RueCore.Typed.useCopy", names)
        self.assertIn("RueCore.Typed.intLit", names)
        self.assertIsNone(names["RueCore.Typed.intLit"].doc)
        use_copy = names["RueCore.Typed.useCopy"]
        self.assertEqual(use_copy.rules, ["Use-Copy"])
        self.assertEqual(use_copy.paragraphs, ["3.8:5"])
        ite = names["RueCore.Typed.ite"]
        self.assertEqual(ite.sections, ["5.5"])
        self.assertEqual(ite.paragraphs, ["3.8:50", "3.8:73"])
        self.assertTrue(names["RueCore.Entry.setSt"].helper)
        self.assertTrue(names["RueCore.Typed.attr_inline"].helper)
        self.assertEqual(names["RueCore.Typed.attr_split"].sections, ["5.1"])
        self.assertEqual(statics.module_citation.sections, ["5"])

    def test_examples_module_is_exempt_but_indexed(self) -> None:
        modules, _, errors = self.collect()
        self.assertEqual(errors, [])
        examples = next(m for m in modules if m.name == "RueCore.Examples")
        self.assertTrue(examples.examples)
        cited = next(d for d in examples.declarations if d.name.endswith(".cited"))
        self.assertEqual(cited.rules, ["Use-Copy"])

    def test_missing_doc_comment_is_an_error(self) -> None:
        self.write("Dynamics.lean", DYNAMICS.replace("/-- The interpreter", "/- not a doc\n-/\n/-- x (helper) -/\ndef y : Nat := 0\n\n/-- The interpreter"))
        _, _, errors = self.collect()
        self.assertEqual(errors, [])
        self.write("Dynamics.lean", DYNAMICS.replace("/-- The interpreter: `use` is (D-Use-Copy) (§6.3); `letIn` is (D-Let)\n(§6.7). -/\n", ""))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1)
        self.assertIn("`RueCore.eval` has no doc-comment", errors[0])

    def test_citation_free_doc_comment_is_an_error_unless_helper(self) -> None:
        self.write("Dynamics.lean", DYNAMICS.replace("(D-Use-Copy) (§6.3); `letIn` is (D-Let)\n(§6.7)", "nothing"))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1)
        self.assertIn("cites nothing", errors[0])
        self.write("Dynamics.lean", DYNAMICS.replace("(D-Use-Copy) (§6.3); `letIn` is (D-Let)\n(§6.7)", "nothing (helper)"))
        _, _, errors = self.collect()
        self.assertEqual(errors, [])

    def test_unknown_rule_label_is_an_error_but_prose_parentheses_are_not(self) -> None:
        self.write("Dynamics.lean", DYNAMICS.replace("(D-Let)", "(D-If) (RUE-387) (E0456) (RCopy) (Not-A-Rule-Here)"))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 2, errors)
        self.assertTrue(any("`(D-If)`" in e for e in errors))
        self.assertTrue(any("`(Not-A-Rule-Here)`" in e for e in errors))

    def test_render_marks_unmechanized_rules_and_sections(self) -> None:
        modules, calculus, errors = self.collect()
        self.assertEqual(errors, [])
        text = self.gate.render_index(modules, calculus)
        self.assertIn("| §5.1 | `(Use-Move)` | *not yet mechanized* |", text)
        self.assertIn("| §5.1 | `(Use-Copy)` | `RueCore.Examples.cited`, `RueCore.Typed`, `RueCore.Typed.useCopy` |", text)
        self.assertIn("| §5.1 | Use and copy | `RueCore.Typed`, `RueCore.Typed.attr_split` |", text)
        self.assertIn("| §5.2 | Assignment | *not cited* |", text)
        self.assertIn("| §6.7 | `let` | `RueCore.eval` |", text)
        self.assertNotIn("| §7 | Soundness | *not yet mechanized* |", text)
        self.assertIn("`RueCore.Entry.setSt`", text)
        self.assertIn("| `3.8:73` | `RueCore.Typed.ite` |", text)
        self.assertIn(self.gate.GENERATED_MARKER, text)

    def test_sections_mutual_and_continuations(self) -> None:
        self.write("Blocks.lean", """namespace RueCore
section Foo
/-- A thing (§5.1). -/
def a : Nat := 0
end Foo
/-- Another (§5.1). -/
def b : Nat := 1
mutual
/-- Even (§5.1). -/
def isEven : Nat → Bool
  | 0 => true
  | n + 1 => isOdd n
/-- Odd (§5.1). -/
def isOdd : Nat → Bool
  | 0 => false
  | n + 1 => isEven n
end
/-- Heavy (§5.1). -/
set_option maxHeartbeats 400000 in
theorem heavy : True := trivial
/-- Opened (§5.1). -/
open Nat in
theorem opened : True := trivial
/-- Private (§5.1). -/
private theorem hidden : True := trivial
/-- Unsafe (helper). -/
unsafe def risky : Nat := 0
/-- Nested /- inner -/ comment (§5.1). -/
def nested : Nat := 1
/-- One-liner (§5.1). -/ def oneLiner : Nat := 2
def undocumented : Nat := 3 -- a trailing /- is not a comment opener
end RueCore
""")
        modules, _, errors = self.collect()
        blocks = next(m for m in modules if m.name == "RueCore.Blocks")
        names = [d.name for d in blocks.declarations]
        self.assertEqual(
            names,
            [
                "RueCore.a", "RueCore.b", "RueCore.isEven", "RueCore.isOdd", "RueCore.heavy",
                "RueCore.opened", "RueCore.hidden", "RueCore.risky", "RueCore.nested",
                "RueCore.oneLiner", "RueCore.undocumented",
            ],
        )
        by_name = {d.name: d for d in blocks.declarations}
        for name in ("RueCore.heavy", "RueCore.opened", "RueCore.hidden", "RueCore.nested", "RueCore.oneLiner"):
            self.assertEqual(by_name[name].sections, ["5.1"], name)
        self.assertTrue(by_name["RueCore.risky"].helper)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`RueCore.undocumented` has no doc-comment", errors[0])

    def test_syntax_inventory_is_only_the_T_p_and_e_productions(self) -> None:
        calculus = self.gate.parse_calculus(self.calculus)
        self.assertEqual(
            [(form.nonterminal, form.text) for form in calculus.forms],
            [
                ("T", "int(w, s)"), ("T", "bool"),
                ("p", "x"), ("p", "p . f"),
                ("e", "lit"), ("e", "p"),
            ],
        )
        self.assertEqual(calculus.forms[0].comment, "integer of width w")
        # A wrapped `--` gloss is a continuation, not a seventh alternative.
        self.assertEqual(calculus.forms[-1].comment, "a place used in VALUE context")

    def test_unmapped_syntax_form_is_an_error(self) -> None:
        del self.gate.SYNTAX_FORMS[("e", "p")]
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`e ::= p` has no SYNTAX_FORMS row", errors[0])

    def test_syntax_row_for_a_vanished_alternative_is_an_error(self) -> None:
        self.gate.SYNTAX_FORMS[("e", "loop { e }")] = ("no", [], "gone from the grammar")
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("which §2 no longer writes", errors[0])

    def test_syntax_row_naming_an_undeclared_constructor_is_an_error(self) -> None:
        self.gate.SYNTAX_FORMS[("e", "lit")] = ("yes", ["Expr.intLit"], "")
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`RueCore.Expr.intLit`, which the Lean sources do not declare", errors[0])

    def test_stand_in_row_reads_not_yet_mechanized_and_still_checks_its_names(self) -> None:
        # A stand-in is not coverage of the form it stands in for: the row
        # names it in the note and still reads *not yet mechanized*, and the
        # coverage line does not count it.
        self.gate.SYNTAX_FORMS[("e", "lit")] = (
            "stand-in",
            ["Typed.intLit"],
            "`RueCore.Typed.intLit` stands in for the literal's typing only",
        )
        modules, calculus, errors = self.collect()
        self.assertEqual(errors, [])
        text = self.gate.render_index(modules, calculus)
        self.assertIn(
            "| `e` | `lit` | *not yet mechanized* | "
            "`RueCore.Typed.intLit` stands in for the literal's typing only |",
            text,
        )
        self.assertIn(
            "Coverage: 3 of 6 §2 forms have a core image (1 of them partial); "
            "3 are *not yet mechanized*.",
            text,
        )

    def test_stand_in_row_naming_an_undeclared_constructor_is_an_error(self) -> None:
        self.gate.SYNTAX_FORMS[("e", "lit")] = ("stand-in", ["Expr.gone"], "stands in")
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`RueCore.Expr.gone`, which the Lean sources do not declare", errors[0])

    def test_missing_syntax_block_is_an_error(self) -> None:
        self.calculus.write_text(CALCULUS.replace("## 2. Abstract syntax", "## 2b. Abstract syntax"))
        _, _, errors = self.collect()
        self.assertTrue(any("found no §2 alternatives" in e for e in errors), errors)

    def test_render_states_the_fragment_boundary_with_counts(self) -> None:
        modules, calculus, errors = self.collect()
        self.assertEqual(errors, [])
        text = self.gate.render_index(modules, calculus)
        self.assertIn("## Abstract syntax forms → declarations", text)
        self.assertIn(
            "Coverage: 4 of 6 §2 forms have a core image (1 of them partial); "
            "2 are *not yet mechanized*.",
            text,
        )
        self.assertIn(
            "Coverage: 4 of 5 labeled §5/§6 rules are mechanized; 1 is *not yet mechanized*.",
            text,
        )
        self.assertIn(
            "| `T` | `int(w, s)` | `RueCore.Typed.useCopy` *(partial)* | one width only |", text
        )
        self.assertIn("| `p` | `p . f` | *not yet mechanized* | no projections |", text)

    def test_main_checks_and_writes(self) -> None:
        args = ["--lean-dir", str(self.lean), "--calculus", str(self.calculus)]
        self.assertEqual(self.gate.main(args), 1)  # missing INDEX.md
        self.assertEqual(self.gate.main(args + ["--write"]), 0)
        self.assertTrue((self.lean / "INDEX.md").exists())
        self.assertEqual(self.gate.main(args), 0)
        (self.lean / "INDEX.md").write_text("stale\n")
        self.assertEqual(self.gate.main(args), 1)


if __name__ == "__main__":
    unittest.main()
