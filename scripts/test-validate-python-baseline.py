#!/usr/bin/env python3
"""Focused tests for the Python interpreter-floor policy.

Every fixture here is 3.9 syntax, so the scan means the same thing on every
interpreter that can run this file. That is deliberate: the one part of the
policy that is NOT host-independent -- what happens to a file the scanner
cannot parse -- is asserted on both sides of the floor instead of assumed away.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

SCRIPT = Path(__file__).with_name("validate-python-baseline.py")
policy = load_script("validate-python-baseline.py", __file__)


def descriptions(source: str) -> list[str]:
    findings, _ = policy.scan(source, "probe.py")
    return [finding.construct.description for finding in findings]


def repository_root() -> Path | None:
    """The checkout, or None when this runs from a Buck sandbox.

    Under Buck the target materializes only its declared inputs, so a
    whole-tree scan would find nothing and pass vacuously -- the exact
    fail-open this policy exists to prevent. The authoritative scan is the
    `fmt` job's CI step.
    """
    root = SCRIPT.resolve().parent.parent
    return root if (root / "AGENTS.md").exists() else None


class ScanTests(unittest.TestCase):
    def test_flags_the_import_that_caused_rue_1509(self) -> None:
        self.assertEqual(descriptions("import tomllib\n"), ["the `tomllib` module"])

    def test_a_version_guard_no_longer_exempts(self) -> None:
        # RUE-1524 retired the split floor and its guard idiom. A guard above
        # the import announced a requirement the repository no longer grants,
        # so it must not silence the finding.
        guarded = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            '    raise SystemExit("needs Python 3.11 or newer")\n\n'
            "import tomllib\n"
        )
        self.assertEqual(descriptions(guarded), ["the `tomllib` module"])

    def test_a_handled_import_is_no_exemption_either(self) -> None:
        # The vendored-backport idiom would run on 3.9, but since RUE-1524 the
        # annotation is the one reviewed escape; an unannotated fallback is
        # still a finding rather than a silent allowance.
        source = (
            "try:\n"
            "    import tomllib\n"
            "except ImportError:\n"
            "    import tomli as tomllib\n"
        )
        self.assertEqual(descriptions(source), ["the `tomllib` module"])

    def test_constructs_at_or_below_the_floor_are_silent(self) -> None:
        for source in ("import graphlib\n", "import zoneinfo\n", "import json\n"):
            self.assertEqual(descriptions(source), [], source)

    def test_flags_constructs_newer_than_the_floor(self) -> None:
        cases = {
            "from datetime import UTC\n": "`datetime.UTC`",
            "from typing import Self\n": "`typing.Self`",
            "from enum import StrEnum\n": "`enum.StrEnum`",
            "import itertools\nitertools.pairwise([])\n": "`itertools.pairwise`",
            "import hashlib\nhashlib.file_digest(f, 'sha256')\n": "`hashlib.file_digest`",
            "zip(a, b, strict=True)\n": "the `zip(strict=...)` argument",
            "raise ExceptionGroup('x', [])\n": "the `ExceptionGroup` builtin",
        }
        for source, description in cases.items():
            self.assertIn(description, descriptions(source), source)

    def test_a_method_named_zip_is_not_the_builtin(self) -> None:
        # `zip` must be a bare name; anything.zip(...) is somebody else's API.
        self.assertEqual(descriptions("archive.zip(a, strict=True)\n"), [])
        # `dataclass` is as legitimate spelled through its module, so that one
        # is still found.
        self.assertIn(
            "the `dataclass(slots=...)` argument",
            descriptions("import dataclasses\n@dataclasses.dataclass(slots=True)\nclass C: pass\n"),
        )

    def test_a_finding_names_the_floor_and_the_annotation(self) -> None:
        findings, _ = policy.scan("import itertools\nitertools.batched([], 1)\n", "p.py")
        self.assertEqual(len(findings), 1)
        message = str(findings[0])
        self.assertIn("above the repository floor of 3.9", message)
        self.assertIn("python-baseline-ok", message)
        # The retired guard idiom must not be recommended.
        self.assertNotIn("sys.version_info", message)

    def test_an_alias_does_not_hide_the_construct(self) -> None:
        self.assertIn(
            "`itertools.batched`",
            descriptions("import itertools as it\nit.batched([], 1)\n"),
        )

    def test_an_unrelated_attribute_of_the_same_name_is_not_a_finding(self) -> None:
        self.assertEqual(descriptions("import shutil\nshutil.batched\n"), [])

    def test_the_annotation_silences_its_line(self) -> None:
        self.assertEqual(
            descriptions("import tomllib  # python-baseline-ok: reviewed\n"), []
        )

    def test_the_annotation_has_to_be_a_comment(self) -> None:
        self.assertEqual(
            descriptions('QUOTED = "# python-baseline-ok: no"\nimport tomllib\n'),
            ["the `tomllib` module"],
        )


class UnparseableTests(unittest.TestCase):
    """The one host-dependent behaviour, asserted on whichever side we are on.

    Below the floor a parse error is ambiguous -- PEP 614 decorator grammar
    needs 3.9, which the floor allows -- so guessing would produce a finding
    wrong about the version, wrong about the floor, and backwards about the
    fix. At or above the floor it is unambiguous. Both branches assert, so
    neither can quietly become a no-op.
    """

    def test_a_parse_error_is_judged_only_from_at_or_above_the_floor(self) -> None:
        findings, unscanned = policy.scan("def f(:\n", "p.py")
        if policy.SCANNER < policy.FLOOR:
            self.assertEqual(findings, [])
            self.assertEqual(len(unscanned), 1)
            self.assertIn("not scanned", str(unscanned[0]))
            self.assertIn("authoritative", str(unscanned[0]))
        else:
            self.assertEqual(unscanned, [])
            self.assertEqual(len(findings), 1)


class DiscoveryTests(unittest.TestCase):
    def discover(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            for relative, body in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(body, encoding="utf-8")
            return [
                path.relative_to(root).as_posix() for path in policy.sources(root)
            ]

    def test_an_extensionless_python_program_is_scanned(self) -> None:
        # scripts/ci-test-failure-report is exactly this, and ci.yml runs it
        # twice. The shebang decides the interpreter; the filename does not.
        self.assertEqual(
            self.discover({"scripts/report": "#!/usr/bin/env python3\nimport json\n"}),
            ["scripts/report"],
        )

    def test_an_extensionless_shell_program_is_not(self) -> None:
        self.assertEqual(
            self.discover({"scripts/run": "#!/usr/bin/env bash\necho hi\n"}), []
        )

    def test_the_repository_discovers_its_extensionless_program(self) -> None:
        root = repository_root()
        if root is None:
            self.skipTest("not a repository checkout; scan runs as a CI step")
        found = {path.relative_to(root).as_posix() for path in policy.sources(root)}
        self.assertIn("scripts/ci-test-failure-report", found)

    def test_vendored_and_build_trees_are_skipped(self) -> None:
        self.assertEqual(
            self.discover(
                {
                    "third-party/vendor/x.py": "import tomllib\n",
                    "buck-out/y.py": "import tomllib\n",
                    "keep.py": "import json\n",
                }
            ),
            ["keep.py"],
        )


class DocumentationTests(unittest.TestCase):
    def check(self, text: str) -> list[str]:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / "AGENTS.md").write_text(text, encoding="utf-8")
            return policy.check_docs(root)

    SECTION = policy.FLOOR_SECTION + "\n\nThe tooling requires Python 3.9 or newer.\n"

    def test_the_repository_documents_the_floor_this_gate_enforces(self) -> None:
        root = repository_root()
        if root is None:
            self.skipTest("not a repository checkout; scan runs as a CI step")
        self.assertEqual(policy.check_docs(root), [])

    def test_a_well_formed_section_satisfies_it(self) -> None:
        self.assertEqual(self.check(self.SECTION), [])

    def test_a_missing_section_is_a_finding(self) -> None:
        problems = self.check("# Rue\n\nThe tooling requires Python 3.9 or newer.\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("no \"## Repository tooling baseline\" section", problems[0])

    def test_the_claim_must_be_inside_the_section(self) -> None:
        # An unrelated sentence elsewhere in a long file is not the floor.
        text = (
            "# Rue\n\nThe tooling requires Python 3.9 or newer.\n\n"
            + policy.FLOOR_SECTION
            + "\n\nSee elsewhere.\n\n## Next\n"
        )
        problems = self.check(text)
        self.assertEqual(len(problems), 1)
        self.assertIn("no documented Python floor", problems[0])

    def test_the_claim_must_be_outside_a_code_fence(self) -> None:
        text = (
            policy.FLOOR_SECTION
            + "\n\n```\nrequires Python 3.9 or newer\n```\n"
        )
        problems = self.check(text)
        self.assertEqual(len(problems), 1)
        self.assertIn("outside a code fence", problems[0])

    def test_a_documented_floor_that_disagrees_is_a_finding(self) -> None:
        # 3.11 is exactly the number RUE-1524 retired, so it is the mismatch a
        # stale document would most plausibly state.
        text = policy.FLOOR_SECTION + "\n\nrequires Python 3.11 or newer\n"
        problems = self.check(text)
        self.assertEqual(len(problems), 1)
        self.assertIn("Move both together", problems[0])

    def test_a_second_document_stating_a_different_floor_is_a_finding(self) -> None:
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            (root / "AGENTS.md").write_text(self.SECTION, encoding="utf-8")
            (root / "CONTRIBUTING.md").write_text(
                "requires Python 3.13 or newer\n", encoding="utf-8"
            )
            problems = policy.check_docs(root)
            self.assertEqual(len(problems), 1)
            self.assertIn("One floor, one place", problems[0])


class RepositoryTests(unittest.TestCase):
    def test_the_tree_is_clean(self) -> None:
        root = repository_root()
        if root is None:
            self.skipTest("not a repository checkout; scan runs as a CI step")
        findings, unscanned, documentation, scanned = policy.validate(root)
        self.assertEqual([str(finding) for finding in findings], [])
        self.assertEqual(documentation, [])
        self.assertEqual([str(note) for note in unscanned], [])
        # A real lower bound: a handful of sandboxed files must not satisfy it.
        self.assertGreater(scanned, 50)


if __name__ == "__main__":
    unittest.main()
