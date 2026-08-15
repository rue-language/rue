#!/usr/bin/env python3
"""Focused tests for the complete live-type architecture inventory."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

inventory = load_script("validate-type-architecture.py", __file__)


class InventoryTests(unittest.TestCase):
    def validate(self, files: dict[str, str], crate: str = "rue-air") -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            source_root = Path(directory) / "materialized"
            for relative, source in files.items():
                path = source_root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            return inventory.validate(
                Path(directory) / "unrelated-root",
                [(crate, source_root)],
            )

    def test_new_nested_consumer_file_is_scanned_without_a_manifest(self) -> None:
        errors = self.validate({"future/nested.rs": "pub fn pool_index() -> u32 { 0 }"})
        self.assertEqual(len(errors), 1)
        self.assertIn("future/nested.rs:1", errors[0])

    def test_authoritative_encoding_is_the_only_tag_assignment_site(self) -> None:
        self.assertEqual(
            self.validate({"type_encoding.rs": "enum Composite { Struct = 100 }"}),
            [],
        )
        errors = self.validate(
            {"semantic_import.rs": "enum Peer { Struct\t=\t0x64_u8 }"}
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("duplicate compact type tag", errors[0])

    def test_peer_handle_and_whole_definition_updater_are_rejected(self) -> None:
        errors = self.validate(
            {
                "sema/new_path.rs": "struct InternedType(u32);",
                "intern_pool.rs": "pub fn update_struct_def() {}",
            }
        )
        self.assertEqual(len(errors), 3)

    def test_new_public_type_representation_requires_classification(self) -> None:
        errors = self.validate({"sema/new_path.rs": "pub struct PeerType(u32);"})
        self.assertEqual(len(errors), 2)
        self.assertIn("unclassified public type representation", errors[0])

        self.assertEqual(self.validate({"types.rs": "pub struct Type(u32);"}), [])

    def test_renamed_private_compact_handle_requires_classification(self) -> None:
        errors = self.validate({"sema/new_path.rs": "struct SemanticTy(u32);"})
        self.assertEqual(len(errors), 1)
        self.assertIn("unclassified compact u32 newtype", errors[0])

    def test_opaque_ids_reject_public_numeric_traits_and_fields(self) -> None:
        errors = self.validate(
            {
                "types.rs": (
                    "#[derive(PartialOrd, Ord)] pub struct StructId(pub u32);\n"
                    "impl std::fmt::Display for StructId {}\n"
                )
            }
        )
        self.assertEqual(len(errors), 3)


if __name__ == "__main__":
    unittest.main()
