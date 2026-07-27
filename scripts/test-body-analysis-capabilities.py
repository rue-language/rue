#!/usr/bin/env python3
"""Focused tests for the provider-native body-analysis capability guard."""

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
    def classify(
        self,
        files: dict[tuple[str, str], str],
        *,
        buck_filegroup_layout: bool = False,
    ) -> list[object]:
        with tempfile.TemporaryDirectory() as directory:
            roots = {}
            for (crate, relative), source in files.items():
                root = Path(directory) / crate
                roots[crate] = root
                path = root / "src" / relative if buck_filegroup_layout else root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            return inventory.classify(sorted(roots.items()))

    def complete_sources(self, provider_host: str = "") -> dict[tuple[str, str], str]:
        return {
            ("rue-air", "sema/provider.rs"): "pub trait BodyFactProvider {}\n",
            ("rue-air", "sema/provider_body_host.rs"): provider_host,
            ("rue-compiler", "body_query.rs"): "pub struct BodyTransaction;\n",
        }

    def test_provider_contract_is_clean(self) -> None:
        hits = self.classify(
            self.complete_sources(
                "struct ProviderBodyHost<P> { provider: P }\n"
                "fn analyze<P>(host: &ProviderBodyHost<P>) { host.provider; }\n"
            )
        )
        self.assertEqual(hits, [])

    def test_provider_contract_is_clean_in_buck_filegroup_layout(self) -> None:
        hits = self.classify(
            self.complete_sources(
                "struct ProviderBodyHost<P> { provider: P }\n"
                "fn analyze<P>(host: &ProviderBodyHost<P>) { host.provider; }\n"
            ),
            buck_filegroup_layout=True,
        )
        self.assertEqual(hits, [])

    def test_whole_program_type_is_rejected(self) -> None:
        hits = self.classify(
            self.complete_sources("fn analyze(sema: &BoundSema<'_>) {}\n")
        )
        self.assertEqual(len(hits), 1)
        self.assertIn("whole-program type `BoundSema`", hits[0].display())

    def test_declaration_universe_table_is_rejected(self) -> None:
        hits = self.classify(
            self.complete_sources("fn analyze(host: &Host) { host.functions.len(); }\n")
        )
        self.assertEqual(len(hits), 1)
        self.assertIn("declaration-universe table `functions`", hits[0].display())

    def test_comments_and_strings_do_not_create_capabilities(self) -> None:
        hits = self.classify(
            self.complete_sources(
                "// BoundSema and host.functions are forbidden\n"
                'const NOTE: &str = "CanonicalMergedProgram";\n'
            )
        )
        self.assertEqual(hits, [])

    def test_missing_required_source_fails_closed(self) -> None:
        hits = self.classify(
            {
                ("rue-air", "sema/provider.rs"): "pub trait BodyFactProvider {}\n",
                ("rue-compiler", "body_query.rs"): "pub struct BodyTransaction;\n",
            }
        )
        self.assertEqual(len(hits), 1)
        self.assertIn("missing required provider source", hits[0].display())


if __name__ == "__main__":
    unittest.main()
