#!/usr/bin/env python3
"""Pin the separation between correctness repetition and performance gates."""

import os
import unittest
from pathlib import Path

ROOT = Path(os.environ.get("RUE_TIMEOUT_WORKFLOW_ROOT", Path(__file__).parent.parent))


class WorkflowContractTests(unittest.TestCase):
    def test_correctness_repetitions_are_scheduled_and_never_mask_failure(self):
        workflow = (ROOT / ".github/workflows/correctness-repetitions.yml").read_text()
        script = (ROOT / "scripts/ci-repeat-correctness").read_text()
        self.assertIn("schedule:", workflow)
        self.assertIn("fail-fast: false", workflow)
        self.assertIn("if: always()", workflow)
        self.assertIn("scripts/ci-repeat-correctness", workflow)
        self.assertIn("failures=$((failures + 1))", script)
        self.assertIn("(( failures == 0 ))", script)
        self.assertNotIn("until scripts/ci-heavy-suite", script.lower())
        self.assertNotIn("while scripts/ci-heavy-suite", script.lower())

    def test_performance_thresholds_live_in_benchmark_workflow(self):
        benchmark = (ROOT / ".github/workflows/benchmarks.yml").read_text()
        correctness = (ROOT / ".github/workflows/correctness-repetitions.yml").read_text()
        self.assertIn("enforce performance thresholds", benchmark)
        self.assertIn("./bench.sh", benchmark)
        self.assertNotIn("bench.sh", correctness)
        self.assertNotIn("enforce-budgets", correctness)

    def test_required_correctness_workflow_has_no_retry_action(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text().lower()
        self.assertNotIn("retry@", workflow)
        self.assertNotIn("nick-fields/retry", workflow)


if __name__ == "__main__":
    unittest.main()
