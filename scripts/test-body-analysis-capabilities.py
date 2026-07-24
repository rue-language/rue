#!/usr/bin/env python3
"""Focused tests for the RUE-1091 body-analysis capability inventory."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-body-analysis-capabilities.py")
SPEC = importlib.util.spec_from_file_location("body_analysis_capabilities", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
inventory = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = inventory
SPEC.loader.exec_module(inventory)


class CapabilityInventoryTests(unittest.TestCase):
    def classify(self, files: dict[str, str]) -> list[object]:
        with tempfile.TemporaryDirectory() as directory:
            source_root = Path(directory) / "materialized"
            for relative, source in files.items():
                path = source_root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            return inventory.classify([("rue-air", source_root)])

    def test_epoch_impl_whitelist_and_unaccounted_are_distinct(self) -> None:
        hits = self.classify(
            {
                "sema/body_endpoint.rs": (
                    "struct EpochFacts<'a> { sema: &'a Sema }\n"
                    "impl Facts for EpochFacts<'_> {\n"
                    "  fn read(&self) { self.sema.functions.len(); }\n"
                    "}\n"
                    "fn escaped(sema: &Sema) { sema.methods.len(); }\n"
                ),
                "sema/one_body.rs": (
                    "fn body(sema: &Sema) { sema.interner.get(\"name\"); }\n"
                ),
            }
        )
        self.assertEqual(
            [hit.category for hit in hits],
            ["allowed", "unaccounted", "whitelist"],
        )
        self.assertEqual(len(inventory.errors_for(hits)), 1)

    def test_today_mode_allows_epoch_impls_but_flip_mode_rejects_them(self) -> None:
        hits = self.classify(
            {
                "sema/call_resolution.rs": (
                    "struct EpochFacts<'a> { sema: &'a Sema }\n"
                    "impl Facts for EpochFacts<'_> {\n"
                    "  fn read(&self) { self.sema.functions.len(); }\n"
                    "}\n"
                )
            }
        )
        self.assertEqual(inventory.errors_for(hits, provider_only=False), [])
        self.assertEqual(len(inventory.errors_for(hits, provider_only=True)), 1)

    def test_bare_self_epoch_read_in_sema_impl_is_visible_to_flip_mode(self) -> None:
        hits = self.classify(
            {
                "sema/typeck.rs": (
                    "impl<'a, D> Sema<'a, D> {\n"
                    "  fn leaked(&self) { self.functions.len(); }\n"
                    "}\n"
                )
            }
        )
        self.assertEqual([hit.category for hit in hits], ["allowed"])
        self.assertIn("legacy Sema epoch authority", hits[0].attribution)
        self.assertEqual(len(inventory.errors_for(hits, provider_only=True)), 1)

    def test_new_sema_field_requires_capability_classification(self) -> None:
        source = (
            "pub struct DeclarationNamespace {\n"
            "  functions: HashMap<Name, Info>,\n"
            "}\n"
            "impl DeclarationNamespace {}\n"
            "pub struct Sema<'a, D> {\n"
            "  declarations: D,\n"
            "  pub(crate) interner: &'a Interner,\n"
            "  future_epoch_table: HashMap<Name, Info>,\n"
            "}\n"
            "impl<D> Deref for Sema<'_, D> {}\n"
        )
        self.assertEqual(
            inventory.unclassified_sema_fields(source),
            {"future_epoch_table"},
        )

    def test_generated_and_anonymous_identity_tables_are_inventoried(self) -> None:
        hits = self.classify(
            {
                "sema/body_endpoint.rs": (
                    "struct EpochFacts<'a> { sema: &'a Sema }\n"
                    "impl Facts for EpochFacts<'_> {\n"
                    "  fn read(&self) {\n"
                    "    self.sema.generated_structs.get(&name);\n"
                    "    self.sema.generated_enums.get(&name);\n"
                    "    self.sema.anon_struct_identities.get(&key);\n"
                    "    self.sema.anon_enum_identities.get(&key);\n"
                    "  }\n"
                    "}\n"
                )
            }
        )
        self.assertEqual([hit.category for hit in hits], ["allowed"] * 4)

    def test_bare_self_epoch_read_inside_epoch_impl_is_not_skipped(self) -> None:
        hits = self.classify(
            {
                "sema/body_endpoint.rs": (
                    "struct EpochFacts { functions: Functions }\n"
                    "impl Facts for EpochFacts {\n"
                    "  fn read(&self) { self.functions.len(); }\n"
                    "}\n"
                )
            }
        )
        self.assertEqual([hit.category for hit in hits], ["allowed"])
        self.assertEqual(hits[0].attribution, "epoch-backed facts impl")

    def test_comments_and_strings_do_not_create_capabilities(self) -> None:
        hits = self.classify(
            {
                "sema/one_body.rs": (
                    "// sema.functions.len()\n"
                    "const NOTE: &str = \"sema.methods\";\n"
                )
            }
        )
        self.assertEqual(hits, [])


if __name__ == "__main__":
    unittest.main()
