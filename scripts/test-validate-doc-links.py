#!/usr/bin/env python3
"""Focused tests for the documentation link gate."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

gate = load_script("validate-doc-links.py", __file__)

INDEX_HEADER = "| File | Records | Status | Superseded by |\n| --- | --- | --- | --- |\n"


def index_row(name: str) -> str:
    return f"| [{name}]({name}) | what it records | current | — |\n"


class DocLinkGateTests(unittest.TestCase):
    def validate(self, files: dict) -> list:
        """Run the gate over a fixture tree of repo-relative files."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, contents in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(contents)
            return gate.validate(root)

    def fixture(self, **overrides) -> dict:
        files = {
            "docs/notes/README.md": INDEX_HEADER + index_row("a-note.md"),
            "docs/notes/a-note.md": "# A note\n",
        }
        files.update(overrides)
        return files

    def test_clean_tree_passes(self) -> None:
        self.assertEqual(self.validate(self.fixture()), [])

    def test_note_without_index_row_fails(self) -> None:
        errors = self.validate(
            self.fixture(**{"docs/notes/orphan.md": "# Orphan\n"})
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("orphan.md", errors[0])
        self.assertIn("no row", errors[0])

    def test_index_row_for_missing_file_fails(self) -> None:
        files = self.fixture()
        files["docs/notes/README.md"] += index_row("phantom.md")
        errors = self.validate(files)
        # The row is flagged by the index check, and its own markdown link is
        # a dead reference too.
        self.assertEqual(len(errors), 2)
        self.assertIn("'phantom.md'", errors[0])
        self.assertIn("does not exist", errors[0])
        self.assertIn("dead reference", errors[1])

    def test_missing_index_fails(self) -> None:
        errors = self.validate({"docs/notes/a-note.md": "# A note\n"})
        self.assertEqual(len(errors), 1)
        self.assertIn("index is missing", errors[0])

    def test_dead_markdown_link_fails(self) -> None:
        files = self.fixture(
            **{"docs/notes/a-note.md": "# A note\n\nSee [gone](missing.md).\n"}
        )
        errors = self.validate(files)
        self.assertEqual(len(errors), 1)
        self.assertIn("docs/notes/a-note.md:3", errors[0])
        self.assertIn("'missing.md'", errors[0])

    def test_dead_bare_path_fails_and_live_one_passes(self) -> None:
        files = self.fixture(
            **{
                "docs/notes/a-note.md": (
                    "# A note\n\nSee `crates/rue-x/src/lib.rs` and "
                    "`crates/rue-x/src/gone.rs`.\n"
                ),
                "crates/rue-x/src/lib.rs": "// lib\n",
            }
        )
        errors = self.validate(files)
        self.assertEqual(len(errors), 1)
        self.assertIn("crates/rue-x/src/gone.rs", errors[0])

    def test_fenced_code_blocks_are_immune(self) -> None:
        body = (
            "# A note\n\n"
            "```text\n"
            "[gone](missing.md) and docs/never/was.md\n"
            "```\n\n"
            "~~~\n"
            "scripts/also-gone.py\n"
            "~~~\n"
        )
        files = self.fixture(**{"docs/notes/a-note.md": body})
        self.assertEqual(self.validate(files), [])

    def test_urls_anchors_and_rendered_page_links_pass(self) -> None:
        body = (
            "# A note\n\n"
            "[web](https://example.com/docs/gone.md), [top](#heading),\n"
            "[section](../appendices/a-grammar/)\n"
        )
        files = self.fixture(**{"docs/notes/a-note.md": body})
        self.assertEqual(self.validate(files), [])

    def test_zola_content_root_links_resolve_by_ancestor(self) -> None:
        files = self.fixture(
            **{
                "docs/spec/src/03-types/one.md": "See [t](@/04-x/two.md).\n",
                "docs/spec/src/04-x/two.md": "# Two\n",
                "docs/spec/src/03-types/bad.md": "See [t](@/04-x/gone.md).\n",
            }
        )
        errors = self.validate(files)
        self.assertEqual(len(errors), 1)
        self.assertIn("bad.md", errors[0])
        self.assertIn("@/04-x/gone.md", errors[0])

    def test_link_with_anchor_resolves_by_file(self) -> None:
        files = self.fixture(
            **{
                "docs/notes/a-note.md": "See [s](other.md#section).\n",
                "docs/notes/other.md": "# Other\n",
            }
        )
        files["docs/notes/README.md"] += index_row("other.md")
        self.assertEqual(self.validate(files), [])

    def test_word_bounded_bare_paths_do_not_partial_match(self) -> None:
        body = (
            "# A note\n\n"
            "A placeholder `docs/designs/<NNNN>-feature.md` and a longer\n"
            "path prefix like mydocs/notes/a-note.md are not references.\n"
        )
        files = self.fixture(**{"docs/notes/a-note.md": body})
        self.assertEqual(self.validate(files), [])


if __name__ == "__main__":
    unittest.main()
