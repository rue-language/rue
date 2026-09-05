#!/usr/bin/env python3
"""Regression tests for the tutorial snippet checker and wrapper wiring."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


INPUT_ROOT = Path(
    os.environ.get("RUE_TUTORIAL_TEST_ROOT", Path(__file__).resolve().parent.parent)
)
CHECKER_PATH = INPUT_ROOT / "scripts" / "check-tutorial-snippets.py"


def load_checker():
    spec = importlib.util.spec_from_file_location("tutorial_snippet_checker", CHECKER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {CHECKER_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


checker = load_checker()


def snippet(action="compile-fail", expected_codes=frozenset({"E0203"})):
    return checker.Snippet(
        path=Path("chapter.md"),
        line=1,
        action=action,
        source="fn main() -> i32 { 0 }\n",
        expected_codes=expected_codes,
    )


def outcome(returncode, stderr="", stdout=""):
    return subprocess.CompletedProcess([], returncode, stdout, stderr)


def diagnostics(*codes):
    return json.dumps(
        [
            {"code": code, "message": "probe", "severity": "error", "spans": []}
            for code in codes
        ]
    )


class SnippetMetadataTests(unittest.TestCase):
    def test_compile_fail_requires_and_records_diagnostic_codes(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            chapter = root / "chapter.md"
            chapter.write_text(
                "```rue compile-fail E0203 E0902\n"
                "fn main() -> i32 { 0 }\n"
                "```\n"
            )
            snippets = checker.collect_snippets(root)
            self.assertEqual(len(snippets), 1)
            self.assertEqual(snippets[0].expected_codes, {"E0203", "E0902"})

            chapter.write_text(
                "```rue compile-fail\nfn main() -> i32 { 0 }\n```\n"
            )
            with self.assertRaisesRegex(ValueError, "requires at least one"):
                checker.collect_snippets(root)

    def test_diagnostic_code_on_success_case_is_rejected(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            chapter = Path(temp_dir) / "chapter.md"
            chapter.write_text("```rue check E0203\nfn main() -> i32 { 0 }\n```\n")
            with self.assertRaisesRegex(ValueError, "only valid with compile-fail"):
                checker.collect_snippets(Path(temp_dir))

    def test_unknown_attributes_and_internal_codes_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            chapter = Path(temp_dir) / "chapter.md"
            chapter.write_text(
                "```rue compile-fail E0203 typo\nfn main() -> i32 { 0 }\n```\n"
            )
            with self.assertRaisesRegex(ValueError, "unknown rue fence attribute"):
                checker.collect_snippets(Path(temp_dir))

            chapter.write_text(
                "```rue compile-fail E9000\nfn main() -> i32 { 0 }\n```\n"
            )
            with self.assertRaisesRegex(ValueError, "cannot expect internal"):
                checker.collect_snippets(Path(temp_dir))

            chapter.write_text(
                "```rue check\nfn main() -> i32 { 0 }\n```\n"
                "```rue compile-fial E0203\nfn main() -> i32 { 0 }\n```\n"
            )
            with self.assertRaisesRegex(ValueError, "compile-fial"):
                checker.collect_snippets(Path(temp_dir))


    def test_unmarked_rue_fences_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            chapter = Path(temp_dir) / "chapter.md"
            chapter.write_text("```rue\nfn main() -> i32 { 0 }\n```\n")
            with self.assertRaisesRegex(ValueError, "unmarked rue fence"):
                checker.collect_snippets(Path(temp_dir))

    def test_run_fence_reads_its_expected_output_and_attributes(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            chapter = root / "chapter.md"
            chapter.write_text(
                "```rue run stdin=\"7\\n5\\n\" exit=3\n"
                "fn main() -> i32 { 3 }\n"
                "```\n"
                "Run it like this:\n"
                "```bash\n"
                "scripts/rue exec stats.rue\n"
                "```\n"
                "```text\n"
                "count: 2\n"
                "```\n"
            )
            (snippet,) = checker.collect_snippets(root)
            self.assertEqual(snippet.action, "run")
            self.assertEqual(snippet.stdin, "7\n5\n")
            self.assertEqual(snippet.exit_code, 3)
            self.assertEqual(snippet.expected_output, "count: 2\n")

            chapter.write_text("```rue run\nfn main() -> i32 { 0 }\n```\n")
            with self.assertRaisesRegex(ValueError, "must be followed by a ```text fence"):
                checker.collect_snippets(root)

            chapter.write_text(
                "```rue run\nfn main() -> i32 { 0 }\n```\n"
                "```rue check\nfn main() -> i32 { 0 }\n```\n"
                "```text\n\n```\n"
            )
            with self.assertRaisesRegex(ValueError, "must be followed by a ```text fence"):
                checker.collect_snippets(root)

            chapter.write_text("```rue check stdin=\"x\"\nfn main() -> i32 { 0 }\n```\n")
            with self.assertRaisesRegex(ValueError, "only valid with run"):
                checker.collect_snippets(root)

    def test_file_fences_accumulate_for_later_snippets_in_the_chapter(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "a.md").write_text(
                "```rue check\nfn main() -> i32 { 0 }\n```\n"
                "```rue file=lib/math.rue\npub fn double(x: i32) -> i32 { x * 2 }\n```\n"
                "```rue check\nconst m = @import(\"lib/math.rue\");\nfn main() -> i32 { 0 }\n```\n"
            )
            (root / "b.md").write_text("```rue check\nfn main() -> i32 { 0 }\n```\n")
            first, second, other = checker.collect_snippets(root)
            self.assertEqual(first.files, ())
            self.assertEqual(
                second.files, (("lib/math.rue", "pub fn double(x: i32) -> i32 { x * 2 }\n"),)
            )
            self.assertEqual(other.files, ())

            (root / "a.md").write_text("```rue file=../escape.rue\n```\n")
            with self.assertRaisesRegex(ValueError, "relative path inside the chapter"):
                checker.collect_snippets(root)

            (root / "a.md").write_text("```rue file=x.rue check\n```\n")
            with self.assertRaisesRegex(ValueError, "takes no other attributes"):
                checker.collect_snippets(root)

    def test_file_fences_are_written_next_to_the_snippet(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            fake = root / "fake-rue"
            fake.write_text("#!/bin/sh\nexit 0\n")
            fake.chmod(0o755)
            snip = checker.Snippet(
                path=Path("chapter.md"),
                line=9,
                action="check",
                source="fn main() -> i32 { 0 }\n",
                files=(("lib/math.rue", "pub fn double(x: i32) -> i32 { x * 2 }\n"),),
            )
            checker.compile_snippet(str(fake), snip, root, os.environ.copy(), 5)
            self.assertEqual(
                (root / "chapter" / "lib" / "math.rue").read_text(),
                "pub fn double(x: i32) -> i32 { x * 2 }\n",
            )
            self.assertTrue((root / "chapter" / "chapter-9.rue").exists())


class CompilerOutcomeTests(unittest.TestCase):
    def test_expected_success_and_compile_failure_are_accepted(self):
        self.assertIsNone(checker.snippet_failure(snippet("check", frozenset()), outcome(0)))
        self.assertIsNone(
            checker.snippet_failure(snippet(), outcome(1, diagnostics("E0203")))
        )

    def test_wrong_or_malformed_diagnostic_is_rejected(self):
        failure = checker.snippet_failure(snippet(), outcome(1, diagnostics("E0902")))
        self.assertIn("missing expected diagnostic code(s) E0203", failure)

        failure = checker.snippet_failure(snippet(), outcome(1, "not json"))
        self.assertIn("not valid JSON", failure)

        failure = checker.snippet_failure(snippet(), outcome(1, "[]"))
        self.assertIn("empty diagnostic array", failure)

    def test_signal_ice_and_infrastructure_status_are_distinct(self):
        self.assertEqual(
            checker.snippet_failure(snippet(), outcome(-11)),
            "compiler terminated by signal 11",
        )
        self.assertEqual(
            checker.snippet_failure(snippet(), outcome(101, "panicked at probe")),
            "compiler reported an internal compiler error",
        )
        self.assertEqual(
            checker.snippet_failure(snippet(), outcome(2)),
            "compiler returned unexpected failure status 2",
        )
        self.assertEqual(
            checker.snippet_failure(snippet(), outcome(1, diagnostics("E9001"))),
            "compiler reported internal compiler diagnostic code(s): E9001",
        )
        self.assertEqual(
            checker.snippet_failure(
                snippet("check", frozenset()), outcome(1, diagnostics("E9001"))
            ),
            "compiler reported internal compiler diagnostic code(s): E9001",
        )

    def test_run_outcome_compares_exit_status_and_output(self):
        snip = checker.Snippet(
            path=Path("chapter.md"),
            line=1,
            action="run",
            source="",
            exit_code=0,
            expected_output="hello\n",
        )
        self.assertIsNone(checker.run_failure(snip, outcome(0, stdout="hello\n")))
        self.assertIsNone(checker.run_failure(snip, outcome(0, stdout="hello")))
        self.assertIn(
            "did not match", checker.run_failure(snip, outcome(0, stdout="goodbye\n"))
        )
        self.assertIn(
            "expected the program to exit 0, but it exited 7",
            checker.run_failure(snip, outcome(7, stdout="hello\n")),
        )
        self.assertEqual(
            checker.run_failure(snip, outcome(-11)), "program terminated by signal 11"
        )

    def test_compile_timeout_kills_the_compiler_process_group(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            fake = root / "fake-rue"
            fake.write_text("#!/bin/sh\nsleep 10 &\nwait\n")
            fake.chmod(0o755)

            started = time.monotonic()
            with self.assertRaises(subprocess.TimeoutExpired):
                checker.compile_snippet(
                    str(fake), snippet("check", frozenset()), root, os.environ.copy(), 0.05
                )
            self.assertLess(time.monotonic() - started, 2)


class GateWiringTests(unittest.TestCase):
    def test_snippet_environment_removes_tracing_configuration(self):
        with patch.dict(os.environ, {"RUST_LOG": "debug"}):
            self.assertNotIn("RUST_LOG", checker.snippet_env())

    def test_empty_discovery_fails_before_compiler_lookup(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            env = os.environ.copy()
            env["RUE_BINARY"] = "/does/not/exist"
            process = subprocess.run(
                [sys.executable, str(CHECKER_PATH), "--quiet", temp_dir],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
        self.assertEqual(process.returncode, 1)
        self.assertIn("no checked tutorial snippets found", process.stderr)

    def test_missing_compiler_reports_infrastructure_failure(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "chapter.md").write_text(
                "```rue check\nfn main() -> i32 { 0 }\n```\n"
            )
            env = os.environ.copy()
            env["RUE_BINARY"] = str(root / "missing-rue")
            process = subprocess.run(
                [sys.executable, str(CHECKER_PATH), "--quiet", str(root)],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
        self.assertEqual(process.returncode, 1)
        self.assertIn("could not start compiler", process.stderr)

    def test_filtered_wrapper_runs_the_repository_quality_gates(self):
        # RUE-1163: the four gates moved from a bash array in test.sh into the
        # //:repository-quality-gates test_suite. The contract is unchanged — a
        # filtered run must still execute the tutorial gates — but it is now
        # BUCK that says which targets those are, so assert both halves rather
        # than grepping the script for target names.
        wrapper = (INPUT_ROOT / "test.sh").read_text()
        self.assertIn("//:repository-quality-gates", wrapper)

        buck = (INPUT_ROOT / "BUCK").read_text()
        suite = buck[buck.index('name = "repository-quality-gates"') :]
        suite = suite[: suite.index(")")]
        self.assertIn(":tutorial-snippet-tool-tests", suite)
        self.assertIn(":tutorial-snippet-tests", suite)


if __name__ == "__main__":
    unittest.main()
