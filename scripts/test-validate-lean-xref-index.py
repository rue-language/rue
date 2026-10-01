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

<a id="core:5.1:1"></a>**[core:5.1:1]** The two use rules:

```
  premise
  ───────────────────────── (Use-Copy)
  conclusion

  premise
  ───────────────────────── (Use-Move)
  conclusion
```

<a id="core:5.1:1a"></a>**[core:5.1:1a]** A paragraph inserted later, with a letter suffix.

- <a id="core:5.1:2"></a>**[core:5.1:2]** A list item; `core:5.1:1` here is a citation, not a declaration.

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

<a id="core:7:1"></a>**[core:7:1]** Stated now.

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
        # Every paragraph the fixture Lean sources cite (`3.8:5`, `3.8:50`,
        # `3.8:73`), spelled the way `docs/spec/src` itself spells them —
        # so the fixtures exercise the same shortcode the real spec uses,
        # not a stand-in format.
        self.spec = self.root / "spec"
        self.spec.mkdir()
        (self.spec / "03-types.md").write_text(
            '{{ rule(id="3.8:5", cat="normative") }}\n'
            '{{ rule(id="3.8:5a", cat="normative") }}\n'
            '{{ rule(id="3.8:50", cat="normative") }}\n'
            '{{ rule(id="3.8:73", cat="normative") }}\n'
        )
        self.write("Statics.lean", STATICS)
        self.write("Dynamics.lean", DYNAMICS)
        self.write("Examples.lean", EXAMPLES)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def write(self, name: str, text: str) -> None:
        (self.lean / "RueCore" / name).write_text(text)

    def collect(self):
        return self.gate.collect(self.lean, self.calculus, self.spec)

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

    def test_paragraph_citation_of_a_real_spec_id_passes(self) -> None:
        modules, _, errors = self.collect()
        self.assertEqual(errors, [])
        statics = next(m for m in modules if m.name == "RueCore.Statics")
        use_copy = next(d for d in statics.declarations if d.name == "RueCore.Typed.useCopy")
        self.assertEqual(use_copy.paragraphs, ["3.8:5"])

    def test_paragraph_citation_of_a_nonexistent_spec_id_is_an_error(self) -> None:
        self.write("Statics.lean", STATICS.replace("`3.8:5`", "`3.8:999`"))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`RueCore.Typed.useCopy` cites `3.8:999`", errors[0])
        self.assertIn("not a paragraph `docs/spec/src` declares", errors[0])

    def test_paragraph_citation_keeps_its_letter_suffix(self) -> None:
        # `3.8:5a` is a paragraph of its own, not `3.8:5`: reading it as
        # `3.8:5` would file the citation under the wrong paragraph, and the
        # existence check could not notice, because both exist (RUE-2494).
        self.write("Statics.lean", STATICS.replace("`3.8:5`", "`3.8:5a`"))
        modules, _, errors = self.collect()
        self.assertEqual(errors, [])
        statics = next(m for m in modules if m.name == "RueCore.Statics")
        use_copy = next(d for d in statics.declarations if d.name == "RueCore.Typed.useCopy")
        self.assertEqual(use_copy.paragraphs, ["3.8:5a"])

    def test_section_citation_lookalike_is_not_checked_as_a_paragraph(self) -> None:
        # `§6.4` names a calculus *section*, not a spec paragraph, even
        # though its digits could pass for the chapter half of one; only
        # `PARAGRAPH_CITATION`'s own `N.M:K` shape decides what is checked
        # against `docs/spec/src`, so a section citation the spec has no
        # `rule(id=…)` anywhere near is never flagged.
        self.write(
            "Dynamics.lean",
            DYNAMICS.replace(
                "`letIn` is (D-Let)\n(§6.7)",
                "`letIn` is (D-Let) (§6.7); see also §6.4, which the spec fixture "
                "has no paragraph under at all",
            ),
        )
        _, _, errors = self.collect()
        self.assertEqual(errors, [])

    # --- the calculus's own paragraph ids (RUE-2511) --------------------------

    def cite_in_statics(self, text: str) -> None:
        self.write("Statics.lean", STATICS.replace("`3.8:5`", text))

    def use_copy(self, modules):
        statics = next(m for m in modules if m.name == "RueCore.Statics")
        return next(d for d in statics.declarations if d.name == "RueCore.Typed.useCopy")

    def test_calculus_paragraph_ids_are_parsed_in_document_order(self) -> None:
        calculus = self.gate.parse_calculus(self.calculus)
        self.assertEqual(calculus.paragraph_ids, ["5.1:1", "5.1:1a", "5.1:2", "7:1"])
        self.assertEqual(calculus.errors, [])

    def test_calculus_paragraph_citation_resolves_and_counts_its_section(self) -> None:
        self.cite_in_statics("`3.8:5`, core:5.1:1a and core:7:1")
        modules, calculus, errors = self.collect()
        self.assertEqual(errors, [])
        use_copy = self.use_copy(modules)
        self.assertEqual(use_copy.calculus_paragraphs, ["5.1:1a", "7:1"])
        # not read as the spec's `5.1:1a`, and it counts for §5.1 and §7
        self.assertEqual(use_copy.paragraphs, ["3.8:5"])
        self.assertEqual(use_copy.sections, ["5.1", "7"])
        text = self.gate.render_index(modules, calculus)
        self.assertIn("| `RueCore.Statics` | `RueCore.Typed.useCopy` | constructor | (Use-Copy), §5.1, §7, core:5.1:1a, core:7:1 | `3.8:5` |", text)

    def test_calculus_paragraph_citation_of_an_undeclared_id_is_an_error(self) -> None:
        self.cite_in_statics("`3.8:5`, core:5.1:9")
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        # reported at the citation's own line, like any other file's
        line = STATICS.replace("`3.8:5`", "`3.8:5`, core:5.1:9").splitlines().index(
            "  /-- (Use-Copy): copies; Σ unchanged (`3.8:5`, core:5.1:9). -/"
        ) + 1
        self.assertEqual(
            errors[0], f"lean/RueCore/Statics.lean:{line}: cites `core:5.1:9`, which is not a paragraph `calculus.md` declares"
        )

    def test_calculus_paragraph_letter_suffix_is_its_own_id(self) -> None:
        self.cite_in_statics("`3.8:5`, core:5.1:2a")
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("cites `core:5.1:2a`", errors[0])

    def test_duplicate_paragraph_marker_is_an_error(self) -> None:
        self.calculus.write_text(CALCULUS.replace('<a id="core:7:1"></a>**[core:7:1]**', '<a id="core:5.1:1"></a>**[core:5.1:1]**'))
        _, _, errors = self.collect()
        self.assertTrue(any("`core:5.1:1` is declared twice" in e for e in errors), errors)

    def test_paragraph_marker_under_another_section_is_an_error(self) -> None:
        self.calculus.write_text(CALCULUS.replace('<a id="core:7:1"></a>**[core:7:1]**', '<a id="core:6.3:1"></a>**[core:6.3:1]**'))
        _, _, errors = self.collect()
        self.assertTrue(any("`core:6.3:1` sits under §7, not §6.3" in e for e in errors), errors)

    def test_paragraph_marker_whose_anchor_and_tag_disagree_is_an_error(self) -> None:
        self.calculus.write_text(CALCULUS.replace('<a id="core:7:1"></a>', '<a id="core:7:2"></a>'))
        _, _, errors = self.collect()
        self.assertTrue(any("anchor `core:7:2` and tag `core:7:1` disagree" in e for e in errors), errors)

    def test_malformed_paragraph_marker_is_an_error(self) -> None:
        self.calculus.write_text(CALCULUS.replace('"core:7:1"></a>**[core:7:1]**', '"core:7-1"></a>**[core:7-1]**'))
        _, _, errors = self.collect()
        self.assertTrue(any("`core:7-1` is not of the form" in e for e in errors), errors)

    def test_hand_documents_citations_must_resolve(self) -> None:
        # Any Markdown beside the calculus (SPINE, GLOSSARY, the metatheory):
        # a declared id passes, an undeclared one fails with its line, and a
        # hidden directory (Lake's `.lake`) is not read.
        (self.root / "03-metatheory.md").write_text("Holds by core:5.1:1.\n\nAnd by core:5.1:7 (moved).\n")
        (self.lean / "SPINE.md").write_text("| x | core:7:1 |\n")
        (self.lean / ".lake").mkdir()
        (self.lean / ".lake" / "dep.md").write_text("core:9.9:9\n")
        _, _, errors = self.collect()
        self.assertEqual(
            errors, ["03-metatheory.md:3: cites `core:5.1:7`, which is not a paragraph `calculus.md` declares"]
        )

    def test_lean_string_literal_citation_must_resolve(self) -> None:
        # Not only doc-comments: a string the explainer prints cites too.
        self.write(
            "Explain.lean",
            '/-! (`xref: examples`) -/\ndef label : String := "(Use-Copy) core:5.1:1"\ndef bad : String := "(Use-Copy) core:5.1:99"\n',
        )
        _, _, errors = self.collect()
        self.assertEqual(
            errors, ["lean/RueCore/Explain.lean:3: cites `core:5.1:99`, which is not a paragraph `calculus.md` declares"]
        )

    def test_lean_line_comment_citation_must_resolve(self) -> None:
        self.write("Statics.lean", STATICS.replace("end RueCore", "-- see core:7:1, and core:7:30\nend RueCore"))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertRegex(errors[0], r"^lean/RueCore/Statics\.lean:\d+: cites `core:7:30`, which is not")

    def test_explain_text_citation_must_resolve(self) -> None:
        # Every text file under the formal directory, not only Markdown.
        (self.lean / "explain").mkdir()
        (self.lean / "explain" / "call.txt").write_text("step (Use-Copy) core:5.1:1\nstep (Use-Copy) core:5.1:99\n")
        _, _, errors = self.collect()
        self.assertEqual(
            errors, ["lean/explain/call.txt:2: cites `core:5.1:99`, which is not a paragraph `calculus.md` declares"]
        )

    def test_trailing_dot_digit_tail_is_an_error(self) -> None:
        # `core:7:3.1` is not `core:7:3`: ids have no sub-paragraph part. A `.` that
        # ends a sentence is not a tail.
        (self.root / "03-metatheory.md").write_text("By core:7:1.\n\nBy core:7:1.1 as well.\n")
        _, _, errors = self.collect()
        self.assertEqual(
            errors,
            ["03-metatheory.md:3: cites `core:7:1.1`: a paragraph id has no `.`-numbered part after its number"],
        )

    def test_malformed_paragraph_number_is_an_error(self) -> None:
        (self.root / "03-metatheory.md").write_text("By core:7:1_x.\n")
        _, _, errors = self.collect()
        self.assertEqual(errors, ["03-metatheory.md:1: cites `core:7:1_x`: `1_x` is not a paragraph number (`3`, `4a`)"])

    def test_both_ends_of_a_range_must_resolve(self) -> None:
        (self.root / "03-metatheory.md").write_text("By core:5.1:1–2 and core:5.1:1-1a.\n\nBy core:5.1:1–9.\n\nBy core:5.1:8-2.\n")
        _, _, errors = self.collect()
        self.assertEqual(
            errors,
            [
                "03-metatheory.md:3: cites `core:5.1:9`, which is not a paragraph `calculus.md` declares",
                "03-metatheory.md:5: cites `core:5.1:8`, which is not a paragraph `calculus.md` declares",
            ],
        )

    def test_range_in_a_doc_comment_indexes_both_ends(self) -> None:
        self.cite_in_statics("`3.8:5`, core:5.1:1–2")
        modules, _, errors = self.collect()
        self.assertEqual(errors, [])
        self.assertEqual(self.use_copy(modules).calculus_paragraphs, ["5.1:1", "5.1:2"])

    def test_marker_inside_a_code_fence_is_not_a_declaration(self) -> None:
        # An example of the marker syntax in a fence declares nothing, and its
        # `core:X.Y:Z` is read as a citation, so an undeclared one fails.
        fenced = '```\n<a id="core:7:99"></a>**[core:7:99]** example\n```\n'
        self.calculus.write_text(CALCULUS + "\n" + fenced)
        calculus = self.gate.parse_calculus(self.calculus)
        self.assertNotIn("7:99", calculus.paragraph_ids)
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("cites `core:7:99`, which is not a paragraph", errors[0])

    def test_the_calculus_cross_references_its_own_paragraphs(self) -> None:
        # Its markers are declarations; a `core:X.Y:Z` elsewhere in it is a citation.
        self.calculus.write_text(CALCULUS.replace("`core:5.1:1` here", "`core:5.1:3` here"))
        _, _, errors = self.collect()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("calculus.md:", errors[0])
        self.assertIn("cites `core:5.1:3`", errors[0])

    def test_calculus_ids_and_spec_ids_do_not_cross_match(self) -> None:
        # `core:3.8:5` is the calculus's paragraph 3.8:5 (which it does not
        # declare), never the spec's `3.8:5`, which it does; a bare `5.1:1` is
        # the spec's paragraph 5.1:1, never the calculus's `core:5.1:1`; and
        # the spec's own `§3.8:5` spelling is the spec's paragraph.
        self.cite_in_statics("core:3.8:5")
        modules, _, errors = self.collect()
        self.assertEqual(self.use_copy(modules).paragraphs, [])
        self.assertEqual(self.use_copy(modules).calculus_paragraphs, ["3.8:5"])
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("cites `core:3.8:5`, which is not a paragraph `calculus.md` declares", errors[0])

        self.cite_in_statics("`3.8:5`, 5.1:1")
        modules, _, errors = self.collect()
        self.assertEqual(self.use_copy(modules).paragraphs, ["3.8:5", "5.1:1"])
        self.assertEqual(self.use_copy(modules).calculus_paragraphs, [])
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("5.1:1", errors[0])
        self.assertNotIn("core:", errors[0])

        self.cite_in_statics("§3.8:5")
        modules, _, errors = self.collect()
        self.assertEqual(errors, [])
        self.assertEqual(self.use_copy(modules).paragraphs, ["3.8:5"])
        self.assertEqual(self.use_copy(modules).calculus_paragraphs, [])

    def test_section_without_a_paragraph_number_is_an_error(self) -> None:
        # `core:7` is not an id; a section alone is cited `§7`. `core:` followed
        # by prose is not a citation at all.
        (self.root / "03-metatheory.md").write_text("By core:7.\n\nThe core: surface syntax.\n")
        _, _, errors = self.collect()
        self.assertEqual(
            errors,
            [
                "03-metatheory.md:1: cites `core:7`: a paragraph id names its paragraph "
                "(`core:X.Y:Z`); a section alone is `§X.Y`"
            ],
        )

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
        args = [
            "--lean-dir", str(self.lean),
            "--calculus", str(self.calculus),
            "--spec-dir", str(self.spec),
        ]
        self.assertEqual(self.gate.main(args), 1)  # missing INDEX.md
        self.assertEqual(self.gate.main(args + ["--write"]), 0)
        self.assertTrue((self.lean / "INDEX.md").exists())
        self.assertEqual(self.gate.main(args), 0)
        (self.lean / "INDEX.md").write_text("stale\n")
        self.assertEqual(self.gate.main(args), 1)


if __name__ == "__main__":
    unittest.main()
