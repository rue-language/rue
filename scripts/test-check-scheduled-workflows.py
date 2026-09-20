#!/usr/bin/env python3
"""Pin the classification logic of scripts/check-scheduled-workflows.py.

The gate under test runs on every pull request, so its two ways of being wrong
are not symmetric. A false green restores the silence RUE-1507 was filed about.
A false *red* blocks every merge in the repository — worse than the bug being
fixed. The cases below are chosen around that asymmetry: what blocks, what
merely warns, and above all what must never block.

Run directly; no network and no credentials. The API is replaced at the
`Transport` seam, and the transport's own error handling is driven through a
fake `urlopen`, so the client, the classifier, and the driver under test are
the same code CI runs.
"""

from __future__ import annotations

import io
import os
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

csw = load_script("check-scheduled-workflows.py", __file__)

NOW = datetime(2026, 8, 14, 12, 0, tzinfo=timezone.utc)
DAILY = "0 6 * * *"
WEEKLY = "17 7 * * 1"


def workflows_dir() -> Path:
    root = os.environ.get("RUE_SCHEDULED_WORKFLOWS_ROOT")
    return Path(root) / ".github/workflows" if root else csw.WORKFLOWS_DIR


def workflow(name: str, cron: str = WEEKLY, source: str = "") -> csw.Scheduled:
    return csw.Scheduled(name, Path(name), (cron,), source)


def history(
    *,
    state: str = "active",
    scheduled_runs: int = 10,
    successful_runs: int = 10,
    success_days_ago: float | None = 1.0,
) -> csw.History:
    return csw.History(
        state=state,
        scheduled_runs=scheduled_runs,
        successful_runs=successful_runs,
        last_success=(
            None if success_days_ago is None else NOW - timedelta(days=success_days_ago)
        ),
    )


class BlockingIsNarrowTests(unittest.TestCase):
    """Exactly one condition may block a merge. These pin its edges."""

    def test_never_succeeded_blocks(self):
        finding = csw.classify(
            workflow("w.yml"),
            history(scheduled_runs=2, successful_runs=0, success_days_ago=None),
            NOW,
        )
        self.assertEqual(finding.severity, csw.BLOCK)
        self.assertIn("never succeeded", finding.summary)

    def test_a_single_failed_run_only_warns(self):
        """One red run is a red run; two-for-two is a workflow that never worked."""
        finding = csw.classify(
            workflow("w.yml"),
            history(scheduled_runs=1, successful_runs=0, success_days_ago=None),
            NOW,
        )
        self.assertEqual(finding.severity, csw.WARN)

    def test_staleness_never_blocks(self):
        """Heuristic. It may inform a human; it may not stop every merge."""
        finding = csw.classify(
            workflow("w.yml", cron=DAILY), history(success_days_ago=400.0), NOW
        )
        self.assertEqual(finding.severity, csw.WARN)

    def test_disabled_never_blocks(self):
        finding = csw.classify(
            workflow("w.yml"), history(state="disabled_inactivity"), NOW
        )
        self.assertEqual(finding.severity, csw.WARN)

    def test_never_fired_never_blocks(self):
        """This is also how adding a schedule to an existing file looks."""
        finding = csw.classify(
            workflow("w.yml"),
            history(scheduled_runs=0, successful_runs=0, success_days_ago=None),
            NOW,
        )
        self.assertEqual(finding.severity, csw.WARN)

    def test_unregistered_workflow_passes(self):
        self.assertEqual(csw.classify(workflow("new.yml"), None, NOW).severity, csw.OK)


class RegressionTests(unittest.TestCase):
    """The specific false positives an adversarial review found. Each blocked
    the whole repository on real history before these were fixed."""

    def test_release_yml_august_cluster_does_not_block(self):
        """Real timings: last success completed 07-31T08:45:04Z, next 08-04T08:47:20Z."""
        last = datetime(2026, 7, 31, 8, 45, 4, tzinfo=timezone.utc)
        now = datetime(2026, 8, 4, 8, 47, 20, tzinfo=timezone.utc)
        entry = workflow("release.yml", cron=DAILY)
        finding = csw.classify(
            entry,
            csw.History("active", 30, 12, last),
            now,
        )
        self.assertNotEqual(finding.severity, csw.BLOCK)

    def test_adding_a_schedule_to_an_existing_workflow_does_not_block(self):
        """RUE-1488's actual change: a cron added to an already-registered file."""
        finding = csw.classify(
            workflow("performance-collect.yml", cron=DAILY),
            history(scheduled_runs=0, successful_runs=0, success_days_ago=None),
            NOW,
        )
        self.assertNotEqual(finding.severity, csw.BLOCK)

    def test_fuzz_full_history_does_not_block(self):
        """226 retained runs, 67.7% red, 75-day gaps between successes."""
        finding = csw.classify(
            workflow("fuzz.yml", cron="0 0 * * *"),
            history(scheduled_runs=226, successful_runs=73, success_days_ago=75.0),
            NOW,
        )
        self.assertEqual(finding.severity, csw.OK)
        self.assertIn("staleness not assessed", finding.summary)

    def test_waiver_expiry_does_not_block(self):
        """When RUE-1222 lands, the green run must not stop every merge."""
        with patch.dict(csw.POLICIES, {
            "stub.yml": csw.Policy(known_broken="RUE-1222", note="fixed")
        }):
            finding = csw.classify(workflow("stub.yml"), history(), NOW)
        self.assertEqual(finding.severity, csw.WARN)
        self.assertIn("no longer needed", finding.summary)
        self.assertFalse(finding.escalate)

    def test_correctness_repetitions_has_no_obsolete_waiver(self):
        finding = csw.classify(
            workflow("correctness-repetitions.yml"), history(), NOW
        )
        self.assertEqual(finding.severity, csw.OK)


class CronPeriodTests(unittest.TestCase):
    def test_cadences_in_use(self):
        self.assertEqual(csw.cron_period_hours("0 0 * * *"), 24.0)
        self.assertEqual(csw.cron_period_hours("17 7 * * 1"), 168.0)
        self.assertEqual(csw.cron_period_hours("0 */6 * * *"), 6.0)

    def test_calendar_constraints_are_read_rarest_first(self):
        """A month restriction fires yearly however permissive dow looks."""
        self.assertEqual(csw.cron_period_hours("0 0 1 1 *"), 8760.0)
        self.assertEqual(csw.cron_period_hours("0 0 1 */3 *"), 8760.0)
        self.assertEqual(csw.cron_period_hours("0 0 1 * *"), 720.0)

    def test_unparseable_cron_widens_the_window(self):
        self.assertEqual(csw.cron_period_hours("nonsense"), 8760.0)

    def test_subdaily_cron_gets_the_budget_floor(self):
        """Four periods of a 15-minute cron is an hour; an outage is not death."""
        entry = workflow("w.yml", cron="*/15 * * * *")
        finding = csw.classify(entry, history(success_days_ago=2.0), NOW)
        self.assertEqual(finding.severity, csw.OK)


class DiscoveryTests(unittest.TestCase):
    def test_schedule_outside_the_on_block_is_not_a_trigger(self):
        source = (
            "name: X\n"
            "on:\n"
            "  workflow_dispatch:\n"
            "\n"
            "jobs:\n"
            "  a:\n"
            "    steps:\n"
            "      - run: echo '- cron: 0 0 * * *'\n"
        )
        self.assertEqual(csw.schedule_expressions(source), ())

    def test_a_commented_out_cron_is_not_a_trigger(self):
        source = "on:\n  schedule:\n    # - cron: '0 0 * * *'\n  push:\n\njobs: {}\n"
        self.assertEqual(csw.schedule_expressions(source), ())

    def test_block_style_with_comments_and_quotes(self):
        source = (
            "name: X\n"
            "on:\n"
            "  push:\n"
            "    branches: [trunk]\n"
            "  # a comment between triggers\n"
            "  schedule:\n"
            "    - cron: '0 5 * * 1'  # trailing comment\n"
            '    - cron: "23 6 * * *"\n'
            "  workflow_dispatch:\n"
            "\njobs: {}\n"
        )
        self.assertEqual(csw.schedule_expressions(source), ("0 5 * * 1", "23 6 * * *"))

    def test_quoted_on_key_is_read(self):
        """YAML 1.1 reads a bare `on` as boolean true, so some repos quote it."""
        source = '"on":\n  schedule:\n    - cron: "0 0 * * *"\n\njobs: {}\n'
        self.assertEqual(csw.schedule_expressions(source), ("0 0 * * *",))

    def test_trailing_comment_on_the_on_key(self):
        source = "on:  # triggers\n  schedule:\n    - cron: '0 0 * * *'\n\njobs: {}\n"
        self.assertEqual(csw.schedule_expressions(source), ("0 0 * * *",))

    def test_flow_style_schedule_is_read(self):
        source = "on:\n  schedule: [{cron: '0 3 * * *'}]\n\njobs: {}\n"
        self.assertEqual(csw.schedule_expressions(source), ("0 3 * * *",))

    def test_bare_unquoted_cron_is_read(self):
        source = "on:\n  schedule:\n    - cron: 0 0 * * *\n\njobs: {}\n"
        self.assertEqual(csw.schedule_expressions(source), ("0 0 * * *",))

    def test_unreadable_schedule_is_warned_about_not_skipped(self):
        """A workflow the parser cannot read must not vanish from the audit."""
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / "weird.yml").write_text(
                "on:\n  schedule:\n    - cronx: '0 0 * * *'\n\njobs: {}\n"
            )
            (directory / "real.yml").write_text(
                "on:\n  schedule:\n    - cron: '0 0 * * *'\n\njobs: {}\n"
            )
            result = csw.discover(directory)
            self.assertEqual([w.name for w in result.scheduled], ["real.yml"])
            self.assertTrue(any("weird.yml" in w for w in result.warnings))

    def test_the_real_tree_has_scheduled_workflows(self):
        result = csw.discover(workflows_dir())
        self.assertGreaterEqual(len(result.scheduled), 5, f"got {result.scheduled}")
        for entry in result.scheduled:
            self.assertTrue(entry.crons, f"{entry.name} discovered with no cron")

    def test_the_real_tree_has_no_unreadable_triggers(self):
        self.assertEqual(csw.discover(workflows_dir()).warnings, [])


class WaiverTests(unittest.TestCase):
    def test_waiver_suppresses_a_blocking_verdict(self):
        csw.POLICIES["stub.yml"] = csw.Policy(known_broken="RUE-1222", note="x")
        try:
            finding = csw.classify(
                workflow("stub.yml"),
                history(scheduled_runs=5, successful_runs=0, success_days_ago=None),
                NOW,
            )
            self.assertEqual(finding.severity, csw.OK)
            self.assertIn("RUE-1222", finding.summary)
        finally:
            del csw.POLICIES["stub.yml"]

    def test_a_malformed_issue_reference_is_a_problem(self):
        csw.POLICIES["stub.yml"] = csw.Policy(known_broken="x", note="y")
        try:
            problems = csw.policy_problems([workflow("stub.yml")])
            self.assertTrue(any("not a RUE-NN issue" in p for p in problems))
        finally:
            del csw.POLICIES["stub.yml"]

    def test_a_waiver_without_a_reason_is_a_problem(self):
        csw.POLICIES["stub.yml"] = csw.Policy(known_broken="RUE-1", note="  ")
        try:
            problems = csw.policy_problems([workflow("stub.yml")])
            self.assertTrue(any("no note" in p for p in problems))
        finally:
            del csw.POLICIES["stub.yml"]

    def test_an_orphaned_policy_is_reported(self):
        csw.POLICIES["gone.yml"] = csw.Policy(known_broken="RUE-1", note="x")
        try:
            problems = csw.policy_problems([workflow("still-here.yml")])
            self.assertTrue(any("gone.yml" in p for p in problems))
        finally:
            del csw.POLICIES["gone.yml"]
        self.assertFalse(
            any("gone.yml" in p for p in csw.policy_problems([workflow("x.yml")]))
        )

    def test_the_shipped_policies_are_well_formed(self):
        self.assertEqual(csw.policy_problems(csw.discover(workflows_dir()).scheduled), [])


class StructuralTests(unittest.TestCase):
    def test_ci_timed_without_an_upload_is_a_finding(self):
        entry = workflow(
            "w.yml", source="run: scripts/ci-timed 'x' -- ./buck2 test //...\n"
        )
        problems = csw.structural_problems([entry])
        self.assertEqual(len(problems), 1)
        self.assertIn(csw.FAILURE_LOG_DIR, problems[0])

    def test_ci_timed_with_a_failure_upload_is_clean(self):
        entry = workflow(
            "w.yml",
            source=(
                "run: scripts/ci-timed 'x' -- ./buck2 test //...\n"
                "if: failure()\n"
                "path: ${{ runner.temp }}/rue-ci-failed-logs\n"
            ),
        )
        self.assertEqual(csw.structural_problems([entry]), [])

    def test_a_comment_cannot_satisfy_the_artifact_rule(self):
        """Prose mentioning the directory is the false-green shape being hunted."""
        entry = workflow(
            "w.yml",
            source=(
                "run: scripts/ci-timed 'x' -- ./buck2 test //...\n"
                "# we should upload rue-ci-failed-logs on if: failure() someday\n"
            ),
        )
        self.assertEqual(len(csw.structural_problems([entry])), 1)

    def test_a_workflow_that_never_uses_ci_timed_is_not_asked_for_the_artifact(self):
        entry = workflow("w.yml", source="run: ./buck2 build //...\n")
        self.assertEqual(csw.structural_problems([entry]), [])

    def test_the_real_tree_is_structurally_clean(self):
        self.assertEqual(
            csw.structural_problems(csw.discover(workflows_dir()).scheduled), []
        )


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


LISTING = {
    "workflows": [
        {"path": ".github/workflows/fuzz.yml", "state": "active"},
        {"path": ".github/workflows/release.yml", "state": "disabled_inactivity"},
    ]
}


class ClientTests(unittest.TestCase):
    def test_history_uses_completion_time_for_the_last_success(self):
        """A run enters the success filter when it finishes, not when it starts."""
        transport = MockTransport(
            {
                "actions/workflows?": LISTING,
                "status=success": {
                    "total_count": 12,
                    "workflow_runs": [
                        {
                            "created_at": "2026-08-04T08:23:04Z",
                            "updated_at": "2026-08-04T08:47:20Z",
                        }
                    ],
                },
            }
        )
        result = csw.Client(transport, "r/r").history("fuzz.yml")
        self.assertEqual(result.last_success, csw.parse_time("2026-08-04T08:47:20Z"))

    def test_run_queries_are_scoped_to_the_schedule_trigger(self):
        """A workflow kept green by manual dispatch is the false pass being hunted."""
        transport = MockTransport(
            {"actions/workflows?": LISTING, "status=success": {"total_count": 5,
             "workflow_runs": [{"updated_at": "2026-08-13T00:00:00Z"}]}}
        )
        csw.Client(transport, "r/r").history("fuzz.yml")
        for url in transport.urls:
            if "/runs?" in url:
                self.assertIn("event=schedule", url)

    def test_the_second_query_is_skipped_when_successes_exist(self):
        """Budget matters: this runs on every PR against a shared token bucket."""
        transport = MockTransport(
            {"actions/workflows?": LISTING, "status=success": {"total_count": 5,
             "workflow_runs": [{"updated_at": "2026-08-13T00:00:00Z"}]}}
        )
        csw.Client(transport, "r/r").history("fuzz.yml")
        self.assertEqual(sum(1 for u in transport.urls if "/runs?" in u), 1)

    def test_state_listing_is_fetched_once_for_all_workflows(self):
        transport = MockTransport(
            {"actions/workflows?": LISTING, "status=success": {"total_count": 1,
             "workflow_runs": [{"updated_at": "2026-08-13T00:00:00Z"}]}}
        )
        client = csw.Client(transport, "r/r")
        client.history("fuzz.yml")
        client.history("release.yml")
        self.assertEqual(sum(1 for u in transport.urls if "actions/workflows?" in u), 1)

    def test_disabled_state_comes_from_the_listing(self):
        transport = MockTransport(
            {"actions/workflows?": LISTING, "status=success": {"total_count": 1,
             "workflow_runs": [{"updated_at": "2026-08-13T00:00:00Z"}]}}
        )
        self.assertEqual(
            csw.Client(transport, "r/r").history("release.yml").state,
            "disabled_inactivity",
        )

    def test_a_workflow_absent_from_the_listing_is_not_registered(self):
        transport = MockTransport({"actions/workflows?": LISTING})
        with self.assertRaises(csw.NotRegistered):
            csw.Client(transport, "r/r").history("brand-new.yml")

    def test_a_truncated_success_payload_does_not_raise(self):
        """A partial page must not become an AssertionError on every PR."""
        transport = MockTransport(
            {
                "actions/workflows?": LISTING,
                "status=success": {"total_count": 4, "workflow_runs": []},
            }
        )
        result = csw.Client(transport, "r/r").history("fuzz.yml")
        self.assertIsNone(result.last_success)
        self.assertEqual(
            csw.classify(workflow("fuzz.yml"), result, NOW).severity, csw.OK
        )

    def test_offset_timestamps_parse(self):
        self.assertEqual(csw.parse_time("2026-08-14T02:14:05Z").hour, 2)
        self.assertEqual(csw.parse_time("2025-12-31T16:23:09.000-06:00").day, 31)


class FailOpenTests(unittest.TestCase):
    """Nothing about the API's availability may block a merge."""

    def test_a_failing_history_query_warns_and_passes(self):
        transport = MockTransport(
            {"actions/workflows?": LISTING,
             "status=success": csw.TransportError("HTTP 403")}
        )
        report = csw.check(
            [workflow("fuzz.yml")], csw.Client(transport, "r/r"), now=NOW
        )
        # Scoped to the history verdict: `check` also runs the repository-wide
        # policy audit, which is not meaningful against a one-element list.
        self.assertFalse(any(f.blocks for f in report.findings))
        self.assertTrue(any("unavailable" in w for w in report.warnings))

    def test_an_unclassifiable_workflow_warns_and_passes(self):
        class Boom(csw.Client):
            def history(self, name):
                return "not a History"

        report = csw.check([workflow("w.yml")], Boom(MockTransport({}), "r/r"), now=NOW)
        self.assertFalse(any(f.blocks for f in report.findings))
        self.assertTrue(any("could not be classified" in w for w in report.warnings))

    def test_a_failing_listing_is_requested_once_not_once_per_workflow(self):
        """An outage must not multiply into one retry storm per workflow."""
        transport = MockTransport({"actions/workflows?": csw.TransportError("HTTP 500")})
        client = csw.Client(transport, "r/r")
        report = csw.check(
            [workflow("a.yml"), workflow("b.yml"), workflow("c.yml")], client, now=NOW
        )
        self.assertFalse(any(f.blocks for f in report.findings))
        self.assertEqual(len(transport.urls), 1)
        self.assertEqual(len(report.warnings), 3)

    def test_main_exits_zero_when_the_api_is_unreachable(self):
        """End to end through `main`, with the network guaranteed to fail."""
        argv = ["--repo", "r/r", "--workflows", str(workflows_dir())]

        def opener(*a, **k):
            raise urllib.error.URLError("no network")

        real = csw.urllib.request.urlopen
        csw.urllib.request.urlopen = opener
        try:
            self.assertEqual(csw.main(argv), 0)
        finally:
            csw.urllib.request.urlopen = real


class UrllibTransportTests(unittest.TestCase):
    """The real transport's error handling, which is where fail-open lives."""

    def _transport(self, opener, **kwargs):
        transport = csw.UrllibTransport(token="t", sleep=lambda _: None, **kwargs)
        csw.urllib.request.urlopen = opener
        return transport

    def setUp(self):
        self._real = csw.urllib.request.urlopen

    def tearDown(self):
        csw.urllib.request.urlopen = self._real

    @staticmethod
    def _http_error(code, headers=None):
        return urllib.error.HTTPError(
            "u", code, "err", headers or {}, io.BytesIO(b"body")
        )

    def test_404_becomes_not_registered(self):
        transport = self._transport(lambda *a, **k: (_ for _ in ()).throw(
            self._http_error(404)))
        with self.assertRaises(csw.NotRegistered):
            transport.get("https://x/y")

    def test_403_is_not_retried(self):
        calls = []

        def opener(*a, **k):
            calls.append(1)
            raise self._http_error(403)

        transport = self._transport(opener)
        with self.assertRaises(csw.TransportError):
            transport.get("https://x/y")
        self.assertEqual(len(calls), 1)

    def test_429_is_retried_then_surfaces(self):
        calls = []

        def opener(*a, **k):
            calls.append(1)
            raise self._http_error(429, {"Retry-After": "0"})

        transport = self._transport(opener, attempts=3)
        with self.assertRaises(csw.TransportError):
            transport.get("https://x/y")
        self.assertEqual(len(calls), 3)

    def test_500_retry_can_succeed(self):
        calls = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        def opener(*a, **k):
            calls.append(1)
            if len(calls) == 1:
                raise self._http_error(500)
            return Response(b'{"total_count": 1}')

        transport = self._transport(opener, attempts=3)
        self.assertEqual(transport.get("https://x/y"), {"total_count": 1})
        self.assertEqual(len(calls), 2)

    def test_non_json_body_is_a_transport_error(self):
        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *a):
                return False

        transport = self._transport(lambda *a, **k: Response(b"<html>"))
        with self.assertRaises(csw.TransportError):
            transport.get("https://x/y")


class EscalationTests(unittest.TestCase):
    def test_persistent_findings_are_explicit_without_changing_merge_verdict(self):
        stale = csw.classify(workflow("release.yml", DAILY), history(success_days_ago=22), NOW)
        self.assertTrue(stale.escalate)
        self.assertFalse(stale.blocks)
        never = csw.classify(workflow("new.yml"), history(
            scheduled_runs=2, successful_runs=0, success_days_ago=None), NOW)
        self.assertTrue(never.escalate)
        self.assertTrue(never.blocks)
        disabled = csw.classify(workflow("old.yml"), history(state="disabled_inactivity"), NOW)
        self.assertTrue(disabled.escalate)
        self.assertFalse(disabled.blocks)

    def test_transient_and_intentional_warnings_do_not_file(self):
        for name, past in [
            ("new.yml", history(scheduled_runs=0, successful_runs=0, success_days_ago=None)),
            ("new.yml", history(scheduled_runs=1, successful_runs=0, success_days_ago=None)),
            ("fuzz.yml", history(success_days_ago=75)),
            ("fuzz.yml", history(scheduled_runs=200, successful_runs=0, success_days_ago=None)),
            ("release.yml", history(success_days_ago=2)),
        ]:
            with self.subTest(name=name, past=past):
                self.assertFalse(csw.classify(workflow(name, DAILY), past, NOW).escalate)
        self.assertFalse(csw.classify(workflow("new.yml"), None, NOW).escalate)

    def test_unknown_state_and_missing_timestamp_cannot_file(self):
        unknown = csw.classify(workflow("release.yml"), history(state="unknown"), NOW)
        self.assertFalse(unknown.escalate)
        for past in [history(state="unknown"), history(success_days_ago=None)]:
            class Partial(csw.Client):
                def history(self, name):
                    return past
            report = csw.check([workflow("release.yml")], Partial(MockTransport({}), "r/r"), NOW)
            self.assertTrue(any("incomplete" in warning for warning in report.warnings))
            self.assertFalse(any(finding.escalate for finding in report.findings))
            self.assertFalse(any(finding.blocks for finding in report.findings))

    def test_a_waiver_keeps_the_existing_issue_as_owner(self):
        with patch.dict(csw.POLICIES, {
            "release.yml": csw.Policy(known_broken="RUE-2297", note="repair tracked")
        }):
            result = csw.classify(workflow("release.yml", DAILY), history(success_days_ago=22), NOW)
        self.assertFalse(result.escalate)

    def test_reporter_requires_live_history(self):
        self.assertEqual(csw.main(["--report-linear", "--offline", "--repo", "r/r"]), 1)

    def test_history_uncertainty_fails_only_the_reporting_mode(self):
        report = csw.Report(warnings=["history unavailable"])
        with patch.object(csw, "check", return_value=report), patch.dict(os.environ, {
            "LINEAR_API_KEY": "key", "GITHUB_TOKEN": "token"
        }):
            self.assertEqual(csw.main(["--repo", "r/r", "--workflows", str(workflows_dir())]), 0)
            self.assertEqual(csw.main(["--repo", "r/r", "--workflows", str(workflows_dir()),
                                      "--report-linear"]), 1)

    def test_successful_escalation_is_healthy_even_for_a_blocking_finding(self):
        finding = csw.Finding("broken.yml", csw.BLOCK, "never succeeded", escalate=True)
        with patch.object(csw, "check", return_value=csw.Report(findings=[finding])), \
             patch.object(csw, "ReportingTransport", return_value=IssueTransport()), \
             patch.dict(os.environ, {"LINEAR_API_KEY": "key"}):
            argv = ["--repo", "r/r", "--workflows", str(workflows_dir())]
            self.assertEqual(csw.main(argv), 1)
            self.assertEqual(csw.main(argv + ["--report-linear"]), 0)

    def test_missing_credentials_fail_reporting_even_without_findings(self):
        with patch.object(csw, "check", return_value=csw.Report()), patch.dict(os.environ, {
            "LINEAR_API_KEY": ""
        }):
            self.assertEqual(csw.main(["--repo", "r/r", "--workflows", str(workflows_dir()),
                                      "--report-linear"]), 1)

    def test_trusted_workflow_owns_the_secret_and_serializes_reporting(self):
        source = (workflows_dir() / "scheduled-health.yml").read_text()
        self.assertEqual(csw.schedule_expressions(source), ("23 10 * * *",))
        self.assertNotIn("pull_request", source)
        self.assertNotIn("workflow_run:", source)
        self.assertIn("github.ref == format('refs/heads/{0}', github.event.repository.default_branch)", source)
        self.assertIn("ref: ${{ github.event.repository.default_branch }}", source)
        self.assertIn("persist-credentials: false", source)
        self.assertIn("cancel-in-progress: false", source)
        self.assertIn("LINEAR_API_KEY: ${{ secrets.LINEAR_API_KEY }}", source)
        self.assertIn('--repo "$GITHUB_REPOSITORY" --report-linear', source)


class IssueTransport:
    def __init__(self, existing=False):
        self.calls = []
        self.existing = existing
        self.owner = {"viewer": {"id": "owner"}, "teams": {"nodes": [{
            "id": "team", "states": {"nodes": [{
                "id": "todo", "name": "Todo", "type": "unstarted"
            }]}
        }]}}
        self.created = {"success": True, "issue": {"url": "https://linear.app/issue/new"}}

    def request(self, method, url, headers, payload):
        self.calls.append(payload)
        operation = payload["operationName"]
        if operation == "ScheduledDedup":
            return {"data": {"issues": {"nodes": [{"url": "existing"}] if self.existing else []}}}
        if operation == "ScheduledOwner":
            return {"data": self.owner}
        if operation == "ScheduledCreate":
            return {"data": {"issueCreate": self.created}}
        raise AssertionError(operation)


class LinearReporterTests(unittest.TestCase):
    def finding(self):
        return csw.classify(workflow("release.yml", DAILY), history(success_days_ago=22), NOW)

    def test_new_issue_is_todo_owned_and_carries_evidence(self):
        transport = IssueTransport()
        reporter = csw.LinearReporter(transport, "key", "rue-language/rue")
        self.assertEqual(reporter.file(self.finding()), "https://linear.app/issue/new")
        payload = transport.calls[-1]["variables"]["input"]
        self.assertEqual(payload["stateId"], "todo")
        self.assertEqual(payload["assigneeId"], "owner")
        self.assertEqual(payload["teamId"], "team")
        self.assertEqual(payload["priority"], 2)
        self.assertIn("22.0 days ago", payload["description"])
        self.assertIn("event%3Aschedule", payload["description"])
        self.assertIn("Scheduled-Workflow: `rue-language/rue/.github/workflows/release.yml`", payload["description"])

    def test_existing_open_issue_does_not_create_or_comment(self):
        transport = IssueTransport(existing=True)
        reporter = csw.LinearReporter(transport, "key", "rue-language/rue")
        for _ in range(2):
            self.assertEqual(reporter.file(self.finding()), "existing")
        self.assertEqual([call["operationName"] for call in transport.calls], ["ScheduledDedup"] * 2)
        self.assertIn('nin: ["completed", "canceled"]', transport.calls[0]["query"])
        self.assertIn('key: { eq: "RUE" }', transport.calls[0]["query"])

    def test_repositories_have_distinct_dedup_keys(self):
        markers = []
        for repo in ["rue-language/rue", "fork/rue"]:
            transport = IssueTransport(existing=True)
            csw.LinearReporter(transport, "key", repo).file(self.finding())
            markers.append(transport.calls[0]["variables"]["marker"])
        self.assertNotEqual(*markers)

    def test_owner_and_state_resolution_is_cached_for_multiple_findings(self):
        transport = IssueTransport()
        reporter = csw.LinearReporter(transport, "key", "rue-language/rue")
        reporter.file(self.finding())
        reporter.file(csw.Finding("other.yml", csw.WARN, "disabled", escalate=True))
        self.assertEqual(sum(call["operationName"] == "ScheduledOwner" for call in transport.calls), 1)

    def test_unknown_owner_or_todo_cannot_create_an_unowned_backlog_issue(self):
        for field in ["owner", "state", "team"]:
            transport = IssueTransport()
            if field == "owner":
                transport.owner["viewer"]["id"] = None
            elif field == "state":
                transport.owner["teams"]["nodes"][0]["states"]["nodes"] = []
            else:
                transport.owner["teams"]["nodes"] = []
            with self.subTest(field=field), self.assertRaises(csw.ReportingError):
                csw.LinearReporter(transport, "key", "r/r").file(self.finding())
            self.assertFalse(any(call["operationName"] == "ScheduledCreate" for call in transport.calls))

    def test_unconfirmed_create_is_an_error(self):
        transport = IssueTransport()
        transport.created = {"success": False}
        with self.assertRaises(csw.ReportingError):
            csw.LinearReporter(transport, "key", "r/r").file(self.finding())

    def test_null_dedup_is_not_an_empty_search(self):
        class Incomplete(IssueTransport):
            def request(self, method, url, headers, payload):
                if payload["operationName"] == "ScheduledDedup":
                    return {"data": {"issues": {"nodes": None}}}
                raise AssertionError("must not create after an incomplete search")
        with self.assertRaises(csw.ReportingError):
            csw.LinearReporter(Incomplete(), "key", "r/r").file(self.finding())

    def test_unconfirmed_issue_url_is_an_error(self):
        for url in [None, "", 42]:
            transport = IssueTransport()
            transport.created["issue"]["url"] = url
            with self.subTest(url=url), self.assertRaises(csw.ReportingError):
                csw.LinearReporter(transport, "key", "r/r").file(self.finding())

    def test_dedup_error_is_not_an_empty_search(self):
        class Broken(IssueTransport):
            def request(self, *args):
                return {"errors": [{"message": "unavailable"}]}
        with self.assertRaises(csw.ReportingError):
            csw.LinearReporter(Broken(), "key", "r/r").file(self.finding())


if __name__ == "__main__":
    unittest.main(verbosity=2)
