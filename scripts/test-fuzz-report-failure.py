#!/usr/bin/env python3
"""Pin fuzz-failure reporting: fingerprinting, dedup, payloads, and fallback.

RUE-802 moved nightly fuzz crash tracking from GitHub Issues to Linear. Nothing
here can talk to Linear — CI has no key on a pull request, and a test that filed
real issues would be a menace — so every case drives the real client code
through the injected `Transport` seam with a mock. That covers exactly the parts
that decide whether a crash is reported once, twice, or not at all:

* the fingerprint is stable across run-to-run noise and distinguishes bugs;
* a fingerprint already tracked by an open issue produces a comment, not a
  second issue;
* the create payload carries the team, both labels, the title marker, and the
  fingerprint line the dedup search looks for;
* backend selection prefers Linear, falls back to GitHub, and fails loudly with
  no credentials at all — the "silently dropped failures" case;
* the workflow actually invokes the script, with the secret wired in.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
SCRIPT = SCRIPTS / "fuzz-report-failure.py"

#: Root used for the workflow-contract cases. Buck hands the test a filegroup
#: directory; a direct run falls back to the repository checkout.
ROOT = Path(os.environ.get("RUE_FUZZ_REPORT_ROOT", SCRIPTS.parent))

sys.path.insert(0, str(SCRIPTS))
from gatelib import load_script

fr = load_script("fuzz-report-failure.py", __file__)


class MockTransport(fr.Transport):
    """Scripted stand-in for the network.

    `responses` maps a GraphQL operation name (or `"<METHOD> <url-substring>"`
    for REST) to the decoded body to return. Every request is recorded so a test
    can assert on the payload that would have been sent.
    """

    def __init__(self, responses: dict | None = None) -> None:
        self.responses = responses or {}
        self.calls: list[tuple[str, str, dict | None]] = []

    def request(self, method, url, headers, payload):
        self.calls.append((method, url, payload))
        self.headers = headers
        operation = (payload or {}).get("operationName")
        if operation is not None:
            if operation not in self.responses:
                raise AssertionError(f"unscripted GraphQL operation: {operation}")
            return self.responses[operation]
        for key, value in self.responses.items():
            method_and_needle = key.split(" ", 1)
            if len(method_and_needle) == 2:
                want_method, needle = method_and_needle
                if want_method == method and needle in url:
                    return value
        return {}

    def operations(self) -> list[str]:
        return [(payload or {}).get("operationName") for _, _, payload in self.calls]

    def payload_for(self, operation: str) -> dict:
        for _, _, payload in self.calls:
            if (payload or {}).get("operationName") == operation:
                return payload
        raise AssertionError(f"{operation} was never sent")


TEAM_AND_LABELS = {
    "data": {
        "teams": {"nodes": [{"id": "team-rue", "key": "RUE"}]},
        "issueLabels": {
            "nodes": [
                {"id": "label-bug", "name": "Bug"},
                {"id": "label-auto", "name": "found-by:autonomous"},
                {"id": "label-other", "name": "chore"},
            ]
        },
    }
}

NO_MATCH = {"data": {"issues": {"nodes": []}}}

CREATED = {
    "data": {
        "issueCreate": {
            "success": True,
            "issue": {
                "id": "issue-1",
                "identifier": "RUE-900",
                "url": "https://linear.app/rue/issue/RUE-900",
            },
        }
    }
}

COMMENTED = {"data": {"commentCreate": {"success": True}}}


def crash(**overrides) -> "fr.Crash":
    fields = {
        "target": "compiler",
        "signature": "panic at crates/rue-sema/src/check.rs:120:9: unwrap on None",
        "outcome": "panic: unwrap on None",
        "repro": "crash-compiler-deadbeef-0123456789abcdef.txt",
    }
    fields.update(overrides)
    return fr.Crash(**fields)


class FingerprintTests(unittest.TestCase):
    def test_run_to_run_noise_does_not_change_the_fingerprint(self):
        """The same bug seen twice must dedup, or every night refiles it."""
        first = (
            "panic at /home/runner/work/rue/rue/crates/rue-sema/src/check.rs:120:9: "
            "index out of bounds at 0x7ffd41a2b0c8 (len 65536)"
        )
        second = (
            "panic at /tmp/build-9/crates/rue-sema/src/check.rs:134:11: "
            "index out of bounds at 0x55c19ab34210 (len 131072)"
        )
        self.assertEqual(
            fr.fingerprint("sema", first),
            fr.fingerprint("sema", second),
        )

    def test_generator_seed_is_erased(self):
        self.assertEqual(
            fr.fingerprint("differential", "oracle/compiled disagree, seed 4"),
            fr.fingerprint("differential", "oracle/compiled disagree, seed 918273"),
        )

    def test_same_basename_in_two_crates_is_not_one_bug(self):
        """Path normalization must not fold `rue-sema` and `rue-codegen`."""
        self.assertNotEqual(
            fr.fingerprint("sema", "panic at /w/rue/crates/rue-sema/src/check.rs:1:1"),
            fr.fingerprint(
                "sema", "panic at /w/rue/crates/rue-codegen/src/check.rs:1:1"
            ),
        )

    def test_relative_paths_are_left_alone(self):
        """A relative path is already run-stable; rewriting it only loses detail."""
        self.assertEqual(
            fr.normalize_signature("panic at crates/rue-sema/src/check.rs:9:1"),
            "panic at crates/rue-sema/src/check.rs:LINE",
        )

    def test_distinct_crashes_do_not_collide(self):
        self.assertNotEqual(
            fr.fingerprint("sema", "panic: unwrap on None"),
            fr.fingerprint("sema", "panic: division by zero"),
        )

    def test_same_signature_on_different_targets_is_a_different_bug(self):
        """A shared panic string in the lexer and the emitter is two bugs."""
        self.assertNotEqual(
            fr.fingerprint("lexer", "signal 11"),
            fr.fingerprint("emitter", "signal 11"),
        )

    def test_short_numbers_survive_normalization(self):
        """`signal 6` and `signal 11` must stay distinguishable."""
        self.assertNotEqual(
            fr.fingerprint("emitter", "killed by signal 6"),
            fr.fingerprint("emitter", "killed by signal 11"),
        )

    def test_fingerprint_is_a_short_stable_hex_string(self):
        value = fr.fingerprint("lexer", "panic: boom")
        self.assertEqual(len(value), 16)
        self.assertTrue(all(character in "0123456789abcdef" for character in value))


class CollectionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def write_rue_fuzz_crash(self, target, sig_hash, input_hash, signature, outcome):
        path = self.dir / f"crash-{target}-{sig_hash}-{input_hash}.txt"
        path.write_text("fn main() { }")
        path.with_name(path.name + ".meta").write_text(
            f"target: {target}\nsignature: {signature}\noutcome: {outcome}\n"
        )
        return path

    def test_reads_rue_fuzz_reproducer_and_its_meta_sibling(self):
        self.write_rue_fuzz_crash(
            "sema", "aabbccdd", "0011223344556677", "panic:check.rs", "panic: boom"
        )
        [found] = fr.collect_crashes(self.dir)
        self.assertEqual(found.target, "sema")
        self.assertEqual(found.signature, "panic:check.rs")
        self.assertEqual(found.outcome, "panic: boom")

    def test_two_inputs_with_one_signature_report_once(self):
        """Otherwise a night that hit one bug 40 times files 40 issues."""
        self.write_rue_fuzz_crash(
            "sema", "aabbccdd", "0011223344556677", "panic:check.rs", "panic: boom"
        )
        self.write_rue_fuzz_crash(
            "sema", "aabbccdd", "ffeeddccbbaa9988", "panic:check.rs", "panic: boom"
        )
        self.assertEqual(len(fr.collect_crashes(self.dir)), 1)

    def test_missing_meta_still_reports_a_distinct_crash(self):
        (self.dir / "crash-lexer-12345678-0123456789abcdef.txt").write_text("\x00\x01")
        (self.dir / "crash-lexer-87654321-fedcba9876543210.txt").write_text("\x02")
        found = fr.collect_crashes(self.dir)
        self.assertEqual(len(found), 2)
        self.assertEqual({c.target for c in found}, {"lexer"})

    def test_oracle_diff_repro_is_recognized(self):
        (self.dir / "oracle-diff-seed-77.rue").write_text(
            "// rue-oracle-diff differential miscompile (seed 77)\n"
            "// reason: exit code differs: oracle=0 compiled=1\n"
            "// regenerate: rue-oracle-diff fuzz --start 77 --seeds 1\n"
            "\nfn main() {}\n"
        )
        [found] = fr.collect_crashes(self.dir)
        self.assertEqual(found.target, "differential")
        self.assertIn("exit code differs", found.signature)

    def test_two_seeds_of_one_disagreement_report_once(self):
        for seed in (77, 91):
            (self.dir / f"oracle-diff-seed-{seed}.rue").write_text(
                f"// rue-oracle-diff differential miscompile (seed {seed})\n"
                "// reason: exit code differs: oracle=0 compiled=1\n"
                "\nfn main() {}\n"
            )
        self.assertEqual(len(fr.collect_crashes(self.dir)), 1)

    def test_missing_directory_is_not_an_error(self):
        self.assertEqual(fr.collect_crashes(self.dir / "absent"), [])

    def test_failed_targets_synthesize_reports_when_no_repro_survives(self):
        """A wedged or timed-out step must still reach the tracker."""
        found = fr.crashes_from_failed_targets(["lexer", "emitter_aarch64"])
        self.assertEqual([c.target for c in found], ["lexer", "emitter_aarch64"])
        self.assertEqual(len({c.fingerprint for c in found}), 2)

    def test_job_level_failure_still_produces_one_report(self):
        self.assertEqual(len(fr.crashes_from_failed_targets([])), 1)


class PayloadTests(unittest.TestCase):
    def test_title_carries_marker_target_and_fingerprint(self):
        title = fr.issue_title(crash())
        self.assertTrue(title.startswith(fr.FUZZ_MARKER))
        self.assertIn("compiler", title)
        self.assertIn(crash().fingerprint, title)

    def test_body_carries_the_dedup_needle_verbatim(self):
        """The description must contain exactly what `find_open` searches for."""
        body = fr.issue_body(crash(), "https://ci/run/1")
        self.assertIn(fr.LinearTracker._needle(crash().fingerprint), body)

    def test_body_records_run_target_and_reproducer(self):
        body = fr.issue_body(crash(), "https://ci/run/1")
        self.assertIn("https://ci/run/1", body)
        self.assertIn("crash-compiler-deadbeef-0123456789abcdef.txt", body)
        self.assertIn("rue-fuzz", body)

    def test_github_fallback_body_says_it_is_a_fallback(self):
        body = fr.issue_body(
            crash(), "https://ci/run/1", fr.GitHubTracker.FALLBACK_NOTE
        )
        self.assertIn("LINEAR_API_KEY", body)
        self.assertIn("fallback", body.lower())


class LinearTrackerTests(unittest.TestCase):
    def test_new_crash_is_filed_with_team_labels_and_marker(self):
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": TEAM_AND_LABELS,
                "FuzzIssueCreate": CREATED,
            }
        )
        tracker = fr.LinearTracker(transport, "lin_api_test")
        result = fr.report(tracker, [crash()], "https://ci/run/1")

        self.assertEqual((result.created, result.recurrences), (1, 0))
        payload = transport.payload_for("FuzzIssueCreate")["variables"]["input"]
        self.assertEqual(payload["teamId"], "team-rue")
        self.assertEqual(sorted(payload["labelIds"]), ["label-auto", "label-bug"])
        self.assertIn(fr.FUZZ_MARKER, payload["title"])
        self.assertIn(fr.FINGERPRINT_FIELD, payload["description"])

    def test_known_crash_comments_instead_of_filing_again(self):
        transport = MockTransport(
            {
                "FuzzDedup": {
                    "data": {
                        "issues": {
                            "nodes": [{"id": "issue-7", "identifier": "RUE-801"}]
                        }
                    }
                },
                "FuzzCommentCreate": COMMENTED,
            }
        )
        result = fr.report(
            fr.LinearTracker(transport, "lin_api_test"), [crash()], "https://ci/run/2"
        )

        self.assertEqual((result.created, result.recurrences), (0, 1))
        self.assertNotIn("FuzzIssueCreate", transport.operations())
        comment = transport.payload_for("FuzzCommentCreate")["variables"]["input"]
        self.assertEqual(comment["issueId"], "issue-7")
        self.assertIn("https://ci/run/2", comment["body"])

    def test_dedup_searches_the_labelled_field_not_the_bare_hash(self):
        """A bare hash matches unrelated issues; the labelled field does not."""
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": TEAM_AND_LABELS,
                "FuzzIssueCreate": CREATED,
            }
        )
        fr.report(fr.LinearTracker(transport, "k"), [crash()], "u")
        needle = transport.payload_for("FuzzDedup")["variables"]["needle"]
        self.assertIn(fr.FINGERPRINT_FIELD, needle)
        self.assertIn(crash().fingerprint, needle)

    def test_dedup_query_excludes_closed_issues_and_other_teams(self):
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": TEAM_AND_LABELS,
                "FuzzIssueCreate": CREATED,
            }
        )
        fr.report(fr.LinearTracker(transport, "k"), [crash()], "u")
        sent = transport.payload_for("FuzzDedup")
        self.assertIn('nin: ["completed", "canceled"]', sent["query"])
        self.assertEqual(sent["variables"]["teamKey"], "RUE")

    def test_two_distinct_crashes_file_two_issues(self):
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": TEAM_AND_LABELS,
                "FuzzIssueCreate": CREATED,
            }
        )
        result = fr.report(
            fr.LinearTracker(transport, "k"),
            [crash(), crash(target="lexer", signature="panic: other")],
            "u",
        )
        self.assertEqual(result.created, 2)
        # Team/label resolution is cached, not repeated per crash.
        self.assertEqual(transport.operations().count("FuzzTeamAndLabels"), 1)

    def test_missing_label_warns_but_still_files(self):
        """Losing a label beats losing the crash report."""
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": {
                    "data": {
                        "teams": {"nodes": [{"id": "team-rue", "key": "RUE"}]},
                        "issueLabels": {"nodes": [{"id": "label-bug", "name": "Bug"}]},
                    }
                },
                "FuzzIssueCreate": CREATED,
            }
        )
        result = fr.report(fr.LinearTracker(transport, "k"), [crash()], "u")
        self.assertEqual(result.created, 1)
        payload = transport.payload_for("FuzzIssueCreate")["variables"]["input"]
        self.assertEqual(payload["labelIds"], ["label-bug"])

    def test_graphql_errors_are_surfaced(self):
        transport = MockTransport({"FuzzDedup": {"errors": [{"message": "nope"}]}})
        with self.assertRaises(fr.TransportError):
            fr.report(fr.LinearTracker(transport, "k"), [crash()], "u")

    def test_unsuccessful_create_is_an_error(self):
        transport = MockTransport(
            {
                "FuzzDedup": NO_MATCH,
                "FuzzTeamAndLabels": TEAM_AND_LABELS,
                "FuzzIssueCreate": {"data": {"issueCreate": {"success": False}}},
            }
        )
        with self.assertRaises(fr.TransportError):
            fr.report(fr.LinearTracker(transport, "k"), [crash()], "u")

    def test_personal_key_is_sent_raw_and_oauth_token_as_bearer(self):
        self.assertEqual(
            fr.LinearTracker(MockTransport(), "lin_api_abc")._headers()["Authorization"],
            "lin_api_abc",
        )
        self.assertEqual(
            fr.LinearTracker(MockTransport(), "lin_oauth_abc")._headers()[
                "Authorization"
            ],
            "Bearer lin_oauth_abc",
        )


class GitHubFallbackTests(unittest.TestCase):
    def test_fallback_dedups_per_fingerprint_not_per_label(self):
        """The pre-RUE-802 behavior folded every crash into one open issue."""
        existing = [
            {
                "number": 12,
                "body": "Fuzz-Fingerprint: `" + fr.fingerprint("lexer", "other") + "`",
            }
        ]
        transport = MockTransport({"GET /issues": existing})
        result = fr.report(
            fr.GitHubTracker(transport, "token", "rue-lang/rue"), [crash()], "u"
        )
        self.assertEqual((result.created, result.recurrences), (1, 0))

    def test_fallback_comments_on_a_matching_open_issue(self):
        existing = [
            {"number": 12, "body": fr.LinearTracker._needle(crash().fingerprint)}
        ]
        transport = MockTransport({"GET /issues": existing})
        result = fr.report(
            fr.GitHubTracker(transport, "token", "rue-lang/rue"), [crash()], "u"
        )
        self.assertEqual((result.created, result.recurrences), (0, 1))
        method, url, _ = transport.calls[-1]
        self.assertEqual(method, "POST")
        self.assertTrue(url.endswith("/issues/12/comments"))

    def test_fallback_issue_keeps_the_fuzz_crash_label(self):
        transport = MockTransport({"GET /issues": []})
        fr.report(fr.GitHubTracker(transport, "token", "rue-lang/rue"), [crash()], "u")
        _, _, payload = transport.calls[-1]
        self.assertIn("fuzz-crash", payload["labels"])
        self.assertIn("LINEAR_API_KEY", payload["body"])


class BackendSelectionTests(unittest.TestCase):
    def test_linear_is_preferred_when_the_key_is_present(self):
        tracker = fr.select_tracker(
            fr.Environment(linear_api_key="k", github_token="t", repo="a/b"),
            MockTransport(),
        )
        self.assertIsInstance(tracker, fr.LinearTracker)

    def test_github_is_used_when_the_key_is_absent(self):
        tracker = fr.select_tracker(
            fr.Environment(github_token="t", repo="a/b"), MockTransport()
        )
        self.assertIsInstance(tracker, fr.GitHubTracker)

    def test_no_credentials_fails_loudly(self):
        """Reporting nothing and exiting 0 is the failure mode RUE-802 fixes."""
        with self.assertRaises(fr.TransportError):
            fr.select_tracker(fr.Environment(), MockTransport())

    def test_explicit_linear_backend_does_not_silently_fall_back(self):
        with self.assertRaises(fr.TransportError):
            fr.select_tracker(
                fr.Environment(github_token="t", repo="a/b"),
                MockTransport(),
                backend="linear",
            )

    def test_environment_reads_the_documented_variable_names(self):
        environment = fr.Environment.from_os(
            {
                "LINEAR_API_KEY": " k ",
                "GITHUB_TOKEN": "t",
                "GITHUB_REPOSITORY": "rue-lang/rue",
            }
        )
        self.assertEqual(environment.linear_api_key, "k")
        self.assertEqual(environment.repo, "rue-lang/rue")


class EndToEndTests(unittest.TestCase):
    def test_dry_run_files_every_collected_crash(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "crash-sema-aabbccdd-0011223344556677.txt"
            path.write_text("fn main() {}")
            path.with_name(path.name + ".meta").write_text(
                "target: sema\nsignature: panic:check.rs\noutcome: panic: boom\n"
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--dry-run",
                    "--crash-dir",
                    tmp,
                    "--run-url",
                    "https://ci/run/9",
                ],
                capture_output=True,
                text=True,
                env={**os.environ, "LINEAR_API_KEY": ""},
            )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("FuzzIssueCreate", completed.stdout)
        self.assertIn("1 issue(s) filed", completed.stdout)

    def test_dry_run_without_reproducers_reports_the_failed_targets(self):
        with tempfile.TemporaryDirectory() as tmp:
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--dry-run",
                    "--crash-dir",
                    tmp,
                    "--failed-targets",
                    "lexer,emitter_aarch64",
                ],
                capture_output=True,
                text=True,
            )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("2 issue(s) filed", completed.stdout)

    def test_missing_credentials_exits_non_zero(self):
        completed = subprocess.run(
            [sys.executable, str(SCRIPT), "--crash-dir", "/nonexistent"],
            capture_output=True,
            text=True,
            env={
                key: value
                for key, value in os.environ.items()
                if key not in ("LINEAR_API_KEY", "GITHUB_TOKEN", "GITHUB_REPOSITORY")
            },
        )
        self.assertEqual(completed.returncode, 1)
        self.assertIn("no usable issue tracker", completed.stderr)


class WorkflowContractTests(unittest.TestCase):
    """The script only helps if the nightly workflow actually runs it."""

    def setUp(self):
        self.workflow = (ROOT / ".github/workflows/fuzz.yml").read_text()

    def test_workflow_reports_through_the_script(self):
        self.assertIn("scripts/fuzz-report-failure.py", self.workflow)

    def test_workflow_passes_the_linear_secret_and_the_github_fallback_token(self):
        self.assertIn("LINEAR_API_KEY: ${{ secrets.LINEAR_API_KEY }}", self.workflow)
        self.assertIn("GITHUB_TOKEN:", self.workflow)

    def test_workflow_keeps_issues_write_for_the_fallback_path(self):
        self.assertIn("issues: write", self.workflow)

    def test_workflow_no_longer_files_issues_inline(self):
        """The inline github-script filing path is what RUE-802 replaced."""
        self.assertNotIn("github.rest.issues.create", self.workflow)
        self.assertNotIn("github.rest.issues.listForRepo", self.workflow)

    def test_reporting_runs_even_when_the_crash_upload_is_all_that_survived(self):
        self.assertIn("if: failure()", self.workflow)

    def test_every_aggregated_target_is_also_named_to_the_reporter(self):
        """A target the reporter does not name degrades to `(job-level failure)`.

        The workflow already fails when a registered fuzz target has no
        `--max-time` step. This closes the other half: a target that runs and is
        aggregated, but was forgotten in the reporting step's `record` list,
        would still be reported — just anonymously, with no target to triage by.
        """
        aggregation, _, reporting = self.workflow.partition(
            "- name: Report crashes"
        )
        _, _, aggregation = aggregation.partition(
            "- name: Fail if any fuzz step found crashes"
        )
        pattern = re.compile(r"steps\.([\w-]+)\.outcome")
        self.assertEqual(
            set(pattern.findall(aggregation)),
            set(pattern.findall(reporting)),
        )


if __name__ == "__main__":
    unittest.main()
