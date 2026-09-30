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

    def test_file_and_path_spans_are_masked(self) -> None:
        for text in ["see `validate-lean-xref-index.py` here", "the `GUIDE.md` row", "under `docs/formal/lean`"]:
            with self.subTest(text=text):
                self.assertNotIn("index", self.check.mask_file_spans(text).replace("xref", ""))
                self.assertEqual(len(self.check.mask_file_spans(text)), len(text))

    def test_other_spans_are_kept(self) -> None:
        for text in ["`RueCore.eval` runs", "an `index` word", "`Config.stack`", "`drop ℓ c`"]:
            with self.subTest(text=text):
                self.assertEqual(self.check.mask_file_spans(text), text)

    def test_first_use_skips_a_filename(self) -> None:
        source = self.check.Source(short="DOC")
        source.lines = [("§1", "run `validate-lean-xref-index.py` first"), ("§2", "the index is current")]
        pattern = self.check.word_pattern("index")
        self.assertEqual(self.check.first_use([pattern], [source]), "DOC §2")


if __name__ == "__main__":
    unittest.main()
