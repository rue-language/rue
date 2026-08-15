#!/usr/bin/env python3
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

MODULE = load_script("ci-required-results.py", __file__)


def results(value="success"):
    return {job: {"result": value} for job in MODULE.EXPECTED_REQUIRED_JOBS}


class RequiredResultsTests(unittest.TestCase):
    def test_merge_group_requires_every_job_including_remote_execution(self):
        self.assertEqual(MODULE.validate_required_results("merge_group", results()), [])
        failed = results()
        failed["asan"]["result"] = "failure"
        self.assertIn("asan: expected success", "\n".join(
            MODULE.validate_required_results("merge_group", failed)
        ))

    def test_pull_request_allows_only_remote_execution_to_be_skipped(self):
        pull_request = results()
        pull_request["remote-execution"]["result"] = "skipped"
        self.assertEqual(
            MODULE.validate_required_results("pull_request", pull_request), []
        )
        pull_request["native-platforms"]["result"] = "skipped"
        self.assertIn("native-platforms: expected success", "\n".join(
            MODULE.validate_required_results("pull_request", pull_request)
        ))

    def test_missing_renamed_and_unexpected_dependencies_fail_closed(self):
        missing = results()
        del missing["valgrind"]
        errors = "\n".join(MODULE.validate_required_results("merge_group", missing))
        self.assertIn("missing expected job result", errors)

        renamed = results()
        renamed["format"] = renamed.pop("fmt")
        errors = "\n".join(MODULE.validate_required_results("merge_group", renamed))
        self.assertIn("fmt", errors)
        self.assertIn("unexpected job result", errors)

    def test_workflow_dispatch_preserves_merge_only_remote_skip(self):
        manual = results()
        manual["remote-execution"]["result"] = "skipped"
        self.assertEqual(
            MODULE.validate_required_results("workflow_dispatch", manual), []
        )


if __name__ == "__main__":
    unittest.main()
