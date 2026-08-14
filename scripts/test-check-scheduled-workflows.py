#!/usr/bin/env python3
"""Pin the classification logic of scripts/check-scheduled-workflows.py.

The gate this exercises blocks pull requests, so both of its ways of being
wrong are expensive: a false red stops everyone's work, and a false green
restores exactly the silence RUE-1507 was filed about. The cases below are
chosen for those two edges rather than for coverage — what a workflow that has
never succeeded does, what an ordinary red night does *not* do, and what
happens when a waiver outlives the breakage it was written for.

Run directly; no network and no credentials. The API is replaced at the
`Transport` seam, so the client, the classifier, and the driver under test are
the same code CI runs.
"""

from __future__ import annotations

import importlib.util
import os
import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "check_scheduled_workflows", HERE / "check-scheduled-workflows.py"
)
assert SPEC and SPEC.loader
csw = importlib.util.module_from_spec(SPEC)
# `@dataclass` resolves its own module through `sys.modules`, and fails outright
# when it is missing (see scripts/test-fuzz-report-failure.py, same idiom).
sys.modules[SPEC.name] = csw
SPEC.loader.exec_module(csw)

NOW = datetime(2026, 8, 14, 12, 0, tzinfo=timezone.utc)
DAILY = "0 6 * * *"
WEEKLY = "17 7 * * 1"


def workflow(name: str, cron: str = WEEKLY, source: str = "") -> csw.Scheduled:
    return csw.Scheduled(name, Path(name), (cron,), source)


def history(
    *,
    state: str = "active",
    created_days_ago: float = 90.0,
    scheduled_runs: int = 10,
    successful_runs: int = 10,
    success_days_ago: float | None = 1.0,
) -> csw.History:
    return csw.History(
        state=state,
        created_at=NOW - timedelta(days=created_days_ago),
        scheduled_runs=scheduled_runs,
        successful_runs=successful_runs,
        last_success=(
            None if success_days_ago is None else NOW - timedelta(days=success_days_ago)
        ),
    )


class CronPeriodTests(unittest.TestCase):
    def test_cadences_in_use(self):
        self.assertEqual(csw.cron_period_hours("0 0 * * *"), 24.0)
        self.assertEqual(csw.cron_period_hours("17 7 * * 1"), 168.0)
        self.assertEqual(csw.cron_period_hours("*/6 * * * *"), 1.0)
        self.assertEqual(csw.cron_period_hours("0 */6 * * *"), 6.0)

    def test_unparseable_cron_widens_the_window(self):
        """An expression we cannot read must make the gate quieter, not louder."""
        self.assertEqual(csw.cron_period_hours("nonsense"), 168.0)


class DiscoveryTests(unittest.TestCase):
    def test_schedule_outside_the_on_block_is_not_a_trigger(self):
        source = (
            "name: X\n"
            "on:\n"
            "  workflow_dispatch:\n"
            "\n"
            "# schedule: this is prose about scheduling\n"
            "jobs:\n"
            "  a:\n"
            "    steps:\n"
            "      - run: echo '- cron: 0 0 * * *'\n"
        )
        self.assertEqual(csw.schedule_expressions(source), ())

    def test_cron_is_read_from_the_on_block(self):
        source = (
            "name: X\n"
            "on:\n"
            "  push:\n"
            "    branches: [trunk]\n"
            "  # a comment between triggers\n"
            "  schedule:\n"
            "    - cron: '0 5 * * 1'  # trailing comment\n"
            "    - cron: \"23 6 * * *\"\n"
            "  workflow_dispatch:\n"
            "\n"
            "jobs: {}\n"
        )
        self.assertEqual(csw.schedule_expressions(source), ("0 5 * * 1", "23 6 * * *"))

    def test_the_real_tree_has_scheduled_workflows(self):
        """Discovery over the actual repository, not a fixture.

        `main` refuses an empty result, but only this proves the parser still
        matches the workflows as they are really written.
        """
        root = os.environ.get("RUE_SCHEDULED_WORKFLOWS_ROOT")
        directory = (
            Path(root) / ".github/workflows" if root else csw.WORKFLOWS_DIR
        )
        found = csw.discover(directory)
        self.assertGreaterEqual(len(found), 5, f"discovered only {found}")
        for entry in found:
            self.assertTrue(entry.crons, f"{entry.name} discovered with no cron")


class NeverSucceededTests(unittest.TestCase):
    def test_all_failures_is_a_failure(self):
        finding = csw.classify(
            workflow("w.yml"),
            history(successful_runs=0, success_days_ago=None),
            NOW,
        )
        self.assertTrue(finding.failed)
        self.assertIn("never succeeded", finding.summary)

    def test_one_red_run_after_a_recent_success_is_not_a_failure(self):
        """Durable on purpose: an ordinary red night is not this gate's business."""
        finding = csw.classify(workflow("w.yml"), history(success_days_ago=3.0), NOW)
        self.assertFalse(finding.failed)


class StalenessTests(unittest.TestCase):
    def test_success_inside_the_budget_passes(self):
        # Weekly, default 4 periods = 28 days.
        finding = csw.classify(workflow("w.yml"), history(success_days_ago=27.0), NOW)
        self.assertFalse(finding.failed)

    def test_success_outside_the_budget_fails(self):
        finding = csw.classify(workflow("w.yml"), history(success_days_ago=29.0), NOW)
        self.assertTrue(finding.failed)
        self.assertIn("budget", finding.summary)

    def test_a_declared_policy_widens_the_budget(self):
        """fuzz.yml's real shape: red by design, and reported by its own path."""
        csw.POLICIES["stub-fuzz.yml"] = csw.Policy(stale_periods=30)
        try:
            entry = workflow("stub-fuzz.yml", cron=DAILY)
            self.assertFalse(
                csw.classify(entry, history(success_days_ago=14.0), NOW).failed
            )
            self.assertTrue(
                csw.classify(entry, history(success_days_ago=31.0), NOW).failed
            )
        finally:
            del csw.POLICIES["stub-fuzz.yml"]


class DisabledAndUnregisteredTests(unittest.TestCase):
    def test_disabled_workflow_fails_despite_a_green_history(self):
        """The case run history cannot see: it stops producing runs, last one green."""
        finding = csw.classify(
            workflow("w.yml"),
            history(state="disabled_inactivity", success_days_ago=0.5),
            NOW,
        )
        self.assertTrue(finding.failed)
        self.assertIn("disabled", finding.summary)

    def test_unregistered_workflow_passes(self):
        """A pull request that adds a scheduled workflow must not fail on it."""
        self.assertFalse(csw.classify(workflow("new.yml"), None, NOW).failed)

    def test_freshly_registered_workflow_passes_before_its_cron_fires(self):
        finding = csw.classify(
            workflow("new.yml"),
            history(created_days_ago=3.0, scheduled_runs=0, successful_runs=0,
                    success_days_ago=None),
            NOW,
        )
        self.assertFalse(finding.failed)

    def test_registered_long_ago_and_never_fired_fails(self):
        finding = csw.classify(
            workflow("w.yml"),
            history(created_days_ago=60.0, scheduled_runs=0, successful_runs=0,
                    success_days_ago=None),
            NOW,
        )
        self.assertTrue(finding.failed)
        self.assertIn("never run on its schedule", finding.summary)


class WaiverTests(unittest.TestCase):
    def test_waiver_suppresses_a_real_failure(self):
        csw.POLICIES["stub.yml"] = csw.Policy(known_broken="RUE-1222")
        try:
            finding = csw.classify(
                workflow("stub.yml"),
                history(successful_runs=0, success_days_ago=None),
                NOW,
            )
            self.assertFalse(finding.failed)
            self.assertIn("RUE-1222", finding.summary)
        finally:
            del csw.POLICIES["stub.yml"]

    def test_waiver_that_outlived_its_breakage_fails(self):
        """Otherwise the exemption silently covers the *next* regression too."""
        csw.POLICIES["stub.yml"] = csw.Policy(known_broken="RUE-1222")
        try:
            finding = csw.classify(
                workflow("stub.yml"), history(success_days_ago=1.0), NOW
            )
            self.assertTrue(finding.failed)
            self.assertIn("stale", finding.summary)
        finally:
            del csw.POLICIES["stub.yml"]

    def test_declared_policies_name_real_workflows(self):
        """A POLICIES key that matches nothing exempts nothing and hides its own rot."""
        root = os.environ.get("RUE_SCHEDULED_WORKFLOWS_ROOT")
        directory = Path(root) / ".github/workflows" if root else csw.WORKFLOWS_DIR
        self.assertEqual(csw.orphaned_policies(csw.discover(directory)), [])

    def test_an_orphaned_policy_is_reported(self):
        csw.POLICIES["gone.yml"] = csw.Policy(known_broken="RUE-1")
        try:
            # Every real POLICIES key is absent from this one-element list too,
            # so assert the entry under test is named rather than counting.
            problems = csw.orphaned_policies([workflow("still-here.yml")])
            self.assertTrue(any("gone.yml" in problem for problem in problems))
        finally:
            del csw.POLICIES["gone.yml"]
        self.assertFalse(
            any("gone.yml" in p for p in csw.orphaned_policies([workflow("x.yml")])),
            "the orphan must disappear once the declaration is removed",
        )


class StructuralTests(unittest.TestCase):
    def test_ci_timed_without_an_upload_is_a_finding(self):
        entry = workflow(
            "w.yml", source="run: scripts/ci-timed 'x' -- ./buck2 test //...\n"
        )
        problems = csw.structural_problems([entry])
        self.assertEqual(len(problems), 1)
        self.assertIn("rue-ci-failed-logs", problems[0])

    def test_ci_timed_with_an_upload_is_clean(self):
        entry = workflow(
            "w.yml",
            source=(
                "run: scripts/ci-timed 'x' -- ./buck2 test //...\n"
                "path: ${{ runner.temp }}/rue-ci-failed-logs\n"
            ),
        )
        self.assertEqual(csw.structural_problems([entry]), [])

    def test_a_workflow_that_never_uses_ci_timed_is_not_asked_for_the_artifact(self):
        """An always-empty artifact that looks like coverage is the bug, not the fix."""
        entry = workflow("w.yml", source="run: ./buck2 build //...\n")
        self.assertEqual(csw.structural_problems([entry]), [])


class ClientTests(unittest.TestCase):
    """The API reading itself, against a mock, so the query shape stays pinned."""

    class MockTransport(csw.Transport):
        def __init__(self, responses: dict):
            self.responses = responses
            self.urls: list[str] = []

        def get(self, url: str):
            self.urls.append(url)
            for needle, response in self.responses.items():
                if needle in url:
                    if isinstance(response, Exception):
                        raise response
                    return response
            raise AssertionError(f"unexpected URL {url}")

    def test_history_reads_state_counts_and_latest_success(self):
        transport = self.MockTransport(
            {
                "status=success": {
                    "total_count": 73,
                    "workflow_runs": [{"created_at": "2026-08-14T02:14:05Z"}],
                },
                "runs?": {"total_count": 100, "workflow_runs": []},
                "workflows/fuzz.yml": {
                    "state": "active",
                    "created_at": "2025-12-31T16:23:09.000-06:00",
                },
            }
        )
        result = csw.Client(transport, "rue-language/rue").history("fuzz.yml")
        self.assertEqual(result.state, "active")
        self.assertEqual(result.successful_runs, 73)
        self.assertEqual(result.scheduled_runs, 100)
        self.assertEqual(result.last_success.year, 2026)
        # Every run query must be scoped to the schedule trigger: a workflow
        # kept green by manual dispatch is exactly the false pass being hunted.
        for url in transport.urls:
            if "/runs?" in url:
                self.assertIn("event=schedule", url)

    def test_unregistered_workflow_surfaces_as_not_registered(self):
        transport = self.MockTransport({"workflows/new.yml": csw.NotRegistered("u")})
        with self.assertRaises(csw.NotRegistered):
            csw.Client(transport, "r/r").history("new.yml")

    def test_check_treats_not_registered_as_a_pass(self):
        transport = self.MockTransport({"workflows/": csw.NotRegistered("u")})
        report = csw.check(
            [workflow("new.yml")], csw.Client(transport, "r/r"), now=NOW
        )
        # Scoped to the history verdict: `check` also runs the repository-wide
        # policy audit, which is not meaningful against a one-element list.
        self.assertEqual(len(report.findings), 1)
        self.assertFalse(report.findings[0].failed)

    def test_offset_timestamps_parse(self):
        """GitHub answers with both `Z` and `-05:00`; 3.10 rejects the former raw."""
        self.assertEqual(csw.parse_time("2026-08-14T02:14:05Z").hour, 2)
        self.assertEqual(csw.parse_time("2025-12-31T16:23:09.000-06:00").day, 31)


if __name__ == "__main__":
    unittest.main(verbosity=2)
