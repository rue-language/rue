#!/usr/bin/env python3
"""Focused tests for deterministic CLI shard weight generation."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

weights = load_script("generate-cli-shard-weights.py", __file__)


class CliShardWeightTests(unittest.TestCase):
    def timing_file(self, directory: Path, name: str, events: list[dict]) -> Path:
        path = directory / name
        path.write_text("".join(json.dumps(event) + "\n" for event in events))
        return path

    def test_median_weights_are_sorted_and_rounded_to_milliseconds(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            first = self.timing_file(
                directory,
                "first.jsonl",
                [
                    {"event": "rue_cli_case_timing", "name": "z", "elapsed_s": 0.0014},
                    {"event": "unrelated", "name": "ignored", "elapsed_s": 900},
                    {"event": "case_start", "name": "a", "elapsed_s": "2.0"},
                    {"event": "case_complete", "name": "a", "elapsed_s": "2.010"},
                ],
            )
            second = self.timing_file(
                directory,
                "second.jsonl",
                [
                    {"event": "rue_cli_case_timing", "name": "a", "elapsed_s": 0.020},
                    {"event": "rue_cli_case_timing", "name": "z", "elapsed_s": 0.0001},
                ],
            )
            self.assertEqual(weights.timing_weights([first, second]), {"a": 15, "z": 1})

    def test_input_requires_timing_events(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            path = self.timing_file(Path(raw_directory), "empty.jsonl", [])
            with self.assertRaisesRegex(ValueError, "no rue_cli_case_timing"):
                weights.timing_weights([path])

    def test_document_validation_rejects_zero_weight(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            path = Path(raw_directory) / "weights.json"
            path.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "default_ms": 1,
                        "common": {"bad": 0},
                        "platforms": {},
                    }
                )
            )
            with self.assertRaisesRegex(ValueError, "positive integer"):
                weights.read_document(path)

    def test_canonical_output_is_stable(self) -> None:
        document = {
            "version": 1,
            "default_ms": 1000,
            "common": {"z": 2, "a": 1},
            "platforms": {},
        }
        self.assertEqual(
            weights.canonical_text(document),
            '{\n  "common": {\n    "a": 1,\n    "z": 2\n  },\n'
            '  "default_ms": 1000,\n  "platforms": {},\n  "version": 1\n}\n',
        )


if __name__ == "__main__":
    unittest.main()
