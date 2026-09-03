#!/usr/bin/env python3
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

MODULE = load_script("ci-required-results.py", __file__)

JOBS = ("fmt", "clippy", "remote-execution", "linux-premerge", "asan", "ci-contract")


def results(value="success"):
    return {job: {"result": value} for job in JOBS}


class RequiredResultsTests(unittest.TestCase):
    def test_merge_group_requires_every_listed_job_including_remote_execution(self):
        self.assertEqual(MODULE.validate_required_results("merge_group", results()), [])
        failed = results()
        failed["asan"]["result"] = "failure"
        self.assertIn("asan: expected success", "\n".join(
            MODULE.validate_required_results("merge_group", failed)
        ))
        skipped = results()
        skipped["remote-execution"]["result"] = "skipped"
        self.assertIn("remote-execution: expected success", "\n".join(
            MODULE.validate_required_results("merge_group", skipped)
        ))

    def test_other_events_allow_only_remote_execution_to_be_skipped(self):
        for event in ("pull_request", "workflow_dispatch"):
            needs = results()
            needs["remote-execution"]["result"] = "skipped"
            self.assertEqual(MODULE.validate_required_results(event, needs), [])
            needs["linux-premerge"]["result"] = "skipped"
            self.assertIn("linux-premerge: expected success", "\n".join(
                MODULE.validate_required_results(event, needs)
            ))

    def test_the_map_is_the_inventory_so_a_new_job_is_required_as_listed(self):
        # No list to keep in sync: whatever ci-success needs is required, and
        # validate-ci-gate.py proves that list covers every job.
        needs = results()
        needs["later-added-lane"] = {"result": "cancelled"}
        self.assertIn("later-added-lane: expected success", "\n".join(
            MODULE.validate_required_results("merge_group", needs)
        ))

    def test_an_empty_or_canary_less_map_fails_closed(self):
        errors = "\n".join(MODULE.validate_required_results("merge_group", {}))
        self.assertIn("no job results", errors)
        self.assertIn("missing the remote-execution canary", errors)
        malformed = results()
        malformed["fmt"] = "success"
        self.assertIn("fmt: malformed result record", "\n".join(
            MODULE.validate_required_results("merge_group", malformed)
        ))

    def test_unsupported_event_is_rejected(self):
        self.assertIn("unsupported CI event", "\n".join(
            MODULE.validate_required_results("push", results())
        ))


if __name__ == "__main__":
    unittest.main()
