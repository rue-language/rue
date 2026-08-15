#!/usr/bin/env python3
"""Tests for the shared gate plumbing (RUE-1522).

The masker tests are the contract three gates used to disagree about: a
Rust lifetime must survive masking while every literal form is blanked.
Offset preservation is asserted throughout because every consumer does
line/column arithmetic on the masked text.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from gatelib import (
    SKIP_DIRECTORIES,
    job_blocks,
    load_script,
    mask_rust_non_code,
    parse_crate_sources,
    prune_names,
    walk_files,
)


def masked(source: str) -> str:
    text, _ = mask_rust_non_code(source)
    return text


class MaskerShapeTests(unittest.TestCase):
    def assert_shape_preserved(self, source: str) -> None:
        text = masked(source)
        self.assertEqual(len(text), len(source), "masking changed the length")
        self.assertEqual(
            [i for i, c in enumerate(source) if c == "\n"],
            [i for i, c in enumerate(text) if c == "\n"],
            "masking moved a newline",
        )

    def test_line_and_nested_block_comments(self) -> None:
        source = "let x = 1; // trailing\n/* outer /* inner */ still */ let y = 2;\n"
        text, comments = mask_rust_non_code(source)
        self.assertNotIn("trailing", text)
        self.assertNotIn("inner", text)
        self.assertIn("let y = 2;", text)
        self.assertEqual(len(comments), 2)
        for start, end in comments:
            self.assertNotIn("let", source[start:end].replace("let y", ""))
        self.assert_shape_preserved(source)

    def test_string_forms_are_blanked(self) -> None:
        source = (
            'a("plain"); b("esc \\" quote"); c(b"bytes");'
            ' d(r"raw"); e(r#"raw # "hash"#); f(br#"rawbytes"#);\n'
        )
        text = masked(source)
        for content in ["plain", "esc", "bytes", "raw", "hash", "rawbytes"]:
            self.assertNotIn(content, text)
        for call in ["a(", "b(", "c(", "d(", "e(", "f("]:
            self.assertIn(call, text)
        self.assert_shape_preserved(source)

    def test_char_literals_are_blanked(self) -> None:
        source = "x('a'); y('\\n'); z('\\''); u('\\u{1F600}'); v(b'x');\n"
        text = masked(source)
        self.assertNotIn("a", text.replace("x(", "").replace("(", ""))
        self.assertNotIn("1F600", text)
        self.assertNotIn("b'x'", text)
        self.assert_shape_preserved(source)

    def test_lifetimes_survive_masking(self) -> None:
        source = (
            "fn f<'a, 'static, '_>(x: &'a str, m: &'a mut T) -> &'a str {\n"
            "    struct S<'de> { v: &'de [u8] }\n"
            "}\n"
        )
        text = masked(source)
        self.assertEqual(text, source, "a lifetime was masked as a literal")

    def test_lifetime_then_char_literal_on_one_line(self) -> None:
        source = "fn g<'a>(x: &'a str) -> char { 'q' }\n"
        text = masked(source)
        self.assertIn("<'a>", text)
        self.assertIn("&'a str", text)
        self.assertNotIn("'q'", text)
        self.assert_shape_preserved(source)

    def test_debug_assert_in_comment_and_string_is_masked(self) -> None:
        source = (
            "// debug_assert!(commented)\n"
            'let s = "debug_assert!(quoted)";\n'
            "debug_assert!(real);\n"
        )
        text = masked(source)
        self.assertEqual(text.count("debug_assert!"), 1)


class DiscoveryTests(unittest.TestCase):
    def test_prune_skips_named_and_vcs_directories(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for name in ["keep", "buck-out", "nested"]:
                (root / name).mkdir()
            (root / "nested" / ".git").mkdir()
            names = ["keep", "buck-out", "nested"]
            self.assertEqual(prune_names(root, names), ["keep"])
            self.assertTrue(SKIP_DIRECTORIES.issuperset({"buck-out", ".git"}))

    def test_walk_yields_sorted_kept_files(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "keep").mkdir()
            (root / "target").mkdir()
            (root / "keep" / "b.py").write_text("")
            (root / "keep" / "a.py").write_text("")
            (root / "target" / "skipped.py").write_text("")
            found = [p.name for p in walk_files(root, lambda p: p.suffix == ".py")]
            self.assertEqual(found, ["a.py", "b.py"])


class WorkflowTests(unittest.TestCase):
    def test_jobs_split_on_top_level_keys(self) -> None:
        workflow = (
            "name: CI\n"
            "jobs:\n"
            "  first:\n"
            "    steps:\n"
            "      - run: echo one\n"
            "  second-job:\n"
            "    steps:\n"
            "      - run: echo two\n"
        )
        blocks = job_blocks(workflow)
        self.assertEqual(list(blocks), ["first", "second-job"])
        self.assertIn("echo one", blocks["first"])
        self.assertNotIn("echo two", blocks["first"])

    def test_missing_jobs_mapping_raises(self) -> None:
        with self.assertRaises(ValueError):
            job_blocks("name: CI\n")


class GateHelperTests(unittest.TestCase):
    def test_parse_crate_sources(self) -> None:
        parsed = parse_crate_sources(["rue-air=/tmp/a"], allowed=["rue-air"])
        self.assertEqual(parsed, [("rue-air", Path("/tmp/a"))])
        with self.assertRaises(ValueError):
            parse_crate_sources(["nonsense"])
        with self.assertRaises(ValueError):
            parse_crate_sources(["other=/x"], allowed=["rue-air"])

    def test_load_script_loads_a_gate(self) -> None:
        module = load_script("validate-adrs.py", __file__)
        self.assertTrue(hasattr(module, "main"))
        with self.assertRaises(FileNotFoundError):
            load_script("no-such-script.py", __file__)

    def test_load_script_registers_module_for_dataclasses(self) -> None:
        # `@dataclass` resolves a class's module through `sys.modules` while
        # the script executes, so a gate defining one fails to load unless the
        # loader registers the module first.
        module = load_script("validate-tier-ci-selectors.py", __file__)
        self.assertTrue(hasattr(module, "Selector"))
        self.assertIs(sys.modules["validate_tier_ci_selectors"], module)


if __name__ == "__main__":
    unittest.main()
