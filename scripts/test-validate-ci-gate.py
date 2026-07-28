#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("validate-ci-gate.py")
SPEC = importlib.util.spec_from_file_location("validate_ci_gate", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
SOURCE = Path(os.environ["RUE_CI_WORKFLOW"])


class GateValidatorTests(unittest.TestCase):
    def validate_text(self, text, native_runner=None):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ci.yml"
            path.write_text(text)
            runner_path = Path(directory) / "run-native-platform-corpus.sh"
            runner_path.write_text(
                native_runner
                if native_runner is not None
                else MODULE.NATIVE_RUNNER_SCRIPT.read_text()
            )
            return MODULE.validate(path, runner_path)

    def test_current_workflow_is_valid(self):
        self.assertEqual(MODULE.validate(SOURCE), [])

    def test_removing_or_renaming_job_fails_inventory(self):
        source = SOURCE.read_text()
        removed = source.replace("\n  asan:\n", "\n  removed-asan:\n", 1)
        errors = "\n".join(self.validate_text(removed))
        self.assertIn("CI job inventory missing: asan", errors)
        self.assertIn("unaggregated jobs: removed-asan", errors)

    def test_actions_compatible_underscore_job_is_not_invisible(self):
        source = SOURCE.read_text()
        changed = source + "\n  unaggregated_job:\n    runs-on: ubuntu-latest\n"
        errors = "\n".join(self.validate_text(changed))
        self.assertIn("unaggregated jobs: unaggregated_job", errors)

    def test_omitting_dependency_from_gate_fails(self):
        source = SOURCE.read_text()
        changed = source.replace("      - valgrind\n", "", 1)
        self.assertIn("ci-success needs drift", "\n".join(self.validate_text(changed)))

    def test_native_selector_cannot_silently_return_to_named_filters(self):
        source = SOURCE.read_text()
        runner = MODULE.NATIVE_RUNNER_SCRIPT.read_text().replace(
            "export RUE_PLATFORM_CASE_SELECTION=native",
            "export RUE_PLATFORM_CASE_SELECTION=all",
            1,
        )
        errors = "\n".join(self.validate_text(source, runner))
        self.assertIn("export RUE_PLATFORM_CASE_SELECTION=native", errors)

    def test_unexpected_skip_policy_and_platform_drift_fail(self):
        source = SOURCE.read_text()
        changed = source.replace(
            "if: github.event_name == 'merge_group'",
            "if: github.event_name != 'pull_request'",
            1,
        )
        self.assertIn("merge-group-only", "\n".join(self.validate_text(changed)))

        changed = source.replace("check_name: linux-x64-spec", "check_name: macos-spec", 1)
        self.assertIn("platform-corpus responsibility drift", "\n".join(
            self.validate_text(changed)
        ))


if __name__ == "__main__":
    unittest.main()
