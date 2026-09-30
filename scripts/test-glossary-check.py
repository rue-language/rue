#!/usr/bin/env python3
"""Unit tests for scripts/glossary-check.py."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


def load_check():
    path = Path(__file__).resolve().parent / "glossary-check.py"
    spec = importlib.util.spec_from_file_location("glossary_check", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module  # dataclasses look the module up by name
    spec.loader.exec_module(module)
    return module


class FileSpanTests(unittest.TestCase):
    """RUE-2523: a code span naming a file is not a use of the words inside it."""

    def setUp(self) -> None:
        self.check = load_check()

    def blanked(self, text: str, span: str) -> str:
        return text.replace(span, " " * len(span))

    def test_file_and_path_spans_are_masked(self) -> None:
        for text, span in [
            ("see `validate-lean-xref-index.py` here", "`validate-lean-xref-index.py`"),
            ("the `GUIDE.md` row", "`GUIDE.md`"),
            ("under `docs/formal/lean` now", "`docs/formal/lean`"),
            ("from `crates/rue-oracle-diff` on", "`crates/rue-oracle-diff`"),
            ("the renderings (`lean/explain/*.txt`)", "`lean/explain/*.txt`"),
        ]:
            with self.subTest(text=text):
                self.assertEqual(self.check.mask_file_spans(text), self.blanked(text, span))

    def test_other_spans_are_kept(self) -> None:
        for text in [
            "`RueCore.eval` runs", "an `index` word", "`Config.stack`", "`drop ℓ c`",
            "`0/0` is NaN", "`a/b` rounds", "results `Rval/Rfail/Rtimeout`", "C's `/`", "`Nat.log`", "`List.diff`",
        ]:
            with self.subTest(text=text):
                self.assertEqual(self.check.mask_file_spans(text), text)

    def test_first_use_skips_a_filename(self) -> None:
        source = self.check.Source(short="DOC")
        source.lines = [("§1", "run `validate-lean-xref-index.py` first"), ("§2", "the index is current")]
        pattern = self.check.word_pattern("index")
        self.assertEqual(self.check.first_use([pattern], [source]), "DOC §2")


if __name__ == "__main__":
    unittest.main()
