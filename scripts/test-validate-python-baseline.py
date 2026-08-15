#!/usr/bin/env python3
"""Focused tests for the Python interpreter-floor policy.

Every fixture here is 3.9 syntax, so the scan means the same thing on every
interpreter that can run this file. That is deliberate: the one part of the
policy that is NOT host-independent -- what happens to a file the scanner
cannot parse -- is asserted on both sides of the floor instead of assumed away.

What needed the most care is that these fail when the thing they describe
breaks. Asserting the tree is clean passes just as well when the gate has
stopped looking, so the shipped guard is tested by deleting it from a copy of
the real file and requiring the finding to come back.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

SCRIPT = Path(__file__).with_name("validate-python-baseline.py")
policy = load_script("validate-python-baseline.py", __file__)

TOOL = Path(__file__).with_name("cli-timeout-policy.py")

GUARD = textwrap.dedent(
    """
    import sys

    if sys.version_info < (3, 11):
        raise SystemExit("needs Python 3.11 or newer")
    """
)


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

    def test_a_guard_above_the_import_settles_it(self) -> None:
        self.assertEqual(descriptions(GUARD + "import tomllib\n"), [])

    def test_a_guard_below_the_import_does_not(self) -> None:
        # The program has already died by then, so ordering is the whole point.
        self.assertEqual(
            descriptions("import tomllib\n" + GUARD), ["the `tomllib` module"]
        )

    def test_a_raise_in_the_else_branch_is_not_a_guard(self) -> None:
        # This one runs the import on 3.10 and reproduces RUE-1509 verbatim, so
        # accepting it would certify the bug the gate exists to prevent.
        inverted = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            "    pass\n"
            "else:\n"
            '    raise SystemExit("needs Python 3.11 or newer")\n'
        )
        self.assertEqual(
            descriptions(inverted + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_a_conditional_raise_is_not_a_guard(self) -> None:
        # Reachability is the claim; a raise under another condition does not
        # make it.
        nested = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            "    if False:\n"
            '        raise SystemExit("needs Python 3.11 or newer")\n'
        )
        self.assertEqual(
            descriptions(nested + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_a_guard_that_does_not_name_the_version_does_not_count(self) -> None:
        silent = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            '    raise SystemExit("unsupported")\n'
        )
        self.assertEqual(
            descriptions(silent + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_a_version_named_outside_the_raise_does_not_count(self) -> None:
        # The message the user sees is the one in the raise, not a nearby
        # string that happens to contain the number.
        decoy = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            '    note = "3.11"\n'
            '    raise SystemExit("unsupported")\n'
        )
        self.assertEqual(
            descriptions(decoy + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_a_guard_that_does_not_exit_does_not_count(self) -> None:
        warning = (
            "import sys\n\n"
            "if sys.version_info < (3, 11):\n"
            '    print("needs Python 3.11 or newer")\n'
        )
        self.assertEqual(
            descriptions(warning + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_a_guard_demanding_too_little_does_not_count(self) -> None:
        weak = (
            "import sys\n\n"
            "if sys.version_info < (3, 10):\n"
            '    raise SystemExit("needs Python 3.10 or newer")\n'
        )
        self.assertEqual(
            descriptions(weak + "import tomllib\n"), ["the `tomllib` module"]
        )

    def test_subscripted_version_info_is_the_same_guard(self) -> None:
        subscripted = (
            "import sys\n\n"
            "if sys.version_info[:2] < (3, 11):\n"
            '    raise SystemExit("needs Python 3.11 or newer")\n'
        )
        self.assertEqual(descriptions(subscripted + "import tomllib\n"), [])

    def test_a_handled_import_needs_no_guard(self) -> None:
        # The vendored-backport idiom. The objection to a bare `import tomllib`
        # is that it fails unhandled; this one does not, so the rule does not
        # apply -- and it must not, or the gate would reject the fallback on the
        # day someone adopts it.
        for handler in ("ModuleNotFoundError", "ImportError", ""):
            source = (
                "try:\n"
                "    import tomllib\n"
                f"except {handler}:\n".replace("except :", "except:")
                + "    import tomli as tomllib\n"
            )
            self.assertEqual(descriptions(source), [], handler)

    def test_a_try_that_catches_something_else_is_not_a_handled_import(self) -> None:
        source = (
            "try:\n"
            "    import tomllib\n"
            "except ValueError:\n"
            "    tomllib = None\n"
        )
        self.assertEqual(descriptions(source), ["the `tomllib` module"])

    def test_constructs_at_or_below_the_stock_host_are_silent(self) -> None:
        for source in ("import graphlib\n", "import zoneinfo\n", "import json\n"):
            self.assertEqual(descriptions(source), [], source)

    def test_flags_the_apis_between_the_stock_host_and_the_floor(self) -> None:
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

    def test_above_the_floor_is_a_finding_a_guard_cannot_answer(self) -> None:
        findings, _ = policy.scan("import itertools\nitertools.batched([], 1)\n", "p.py")
        self.assertEqual(len(findings), 1)
        self.assertTrue(findings[0].above_floor)
        self.assertIn("above the repository floor", str(findings[0]))
        self.assertNotIn("sys.version_info", str(findings[0]))
        guarded, _ = policy.scan(
            GUARD + "import itertools\nitertools.batched([], 1)\n", "p.py"
        )
        self.assertEqual(len(guarded), 1)

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

    Below the floor a parse error is ambiguous -- `match` needs 3.10, which the
    floor allows -- so guessing would produce a finding wrong about the version,
    wrong about the floor, and backwards about the fix. At or above the floor it
    is unambiguous. Both branches assert, so neither can quietly become a no-op.
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
            self.assertTrue(findings[0].above_floor)

    def test_a_guarded_construct_above_this_host_is_never_a_false_finding(self) -> None:
        # A correctly guarded `match` is legal at the floor. On 3.9 it does not
        # parse; the answer must be silence plus a note, never a finding that
        # tells the author to remove something the floor allows.
        source = (
            "import sys\n\n"
            "if sys.version_info < (3, 10):\n"
            '    raise SystemExit("needs Python 3.10 or newer")\n\n'
            "def f(x):\n"
            "    match x:\n"
            "        case 1:\n"
            "            return 2\n"
            "    return 0\n"
        )
        findings, unscanned = policy.scan(source, "p.py")
        self.assertEqual([str(finding) for finding in findings], [])
        if policy.SCANNER < (3, 10):
            self.assertEqual(len(unscanned), 1)
        else:
            self.assertEqual(unscanned, [])


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

    SECTION = policy.FLOOR_SECTION + "\n\nThe tooling requires Python 3.11 or newer.\n"

    def test_the_repository_documents_the_floor_this_gate_enforces(self) -> None:
        root = repository_root()
        if root is None:
            self.skipTest("not a repository checkout; scan runs as a CI step")
        self.assertEqual(policy.check_docs(root), [])

    def test_a_well_formed_section_satisfies_it(self) -> None:
        self.assertEqual(self.check(self.SECTION), [])

    def test_a_missing_section_is_a_finding(self) -> None:
        problems = self.check("# Rue\n\nThe tooling requires Python 3.11 or newer.\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("no \"## Repository tooling baseline\" section", problems[0])

    def test_the_claim_must_be_inside_the_section(self) -> None:
        # An unrelated sentence elsewhere in a long file is not the floor.
        text = (
            "# Rue\n\nThe tooling requires Python 3.11 or newer.\n\n"
            + policy.FLOOR_SECTION
            + "\n\nSee elsewhere.\n\n## Next\n"
        )
        problems = self.check(text)
        self.assertEqual(len(problems), 1)
        self.assertIn("no documented Python floor", problems[0])

    def test_the_claim_must_be_outside_a_code_fence(self) -> None:
        text = (
            policy.FLOOR_SECTION
            + "\n\n```\nrequires Python 3.11 or newer\n```\n"
        )
        problems = self.check(text)
        self.assertEqual(len(problems), 1)
        self.assertIn("outside a code fence", problems[0])

    def test_a_documented_floor_that_disagrees_is_a_finding(self) -> None:
        text = policy.FLOOR_SECTION + "\n\nrequires Python 3.9 or newer\n"
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

    def test_the_shipped_tool_is_clean_only_because_it_is_guarded(self) -> None:
        # The bug this whole change answers, pinned by removing its fix: with
        # the guard deleted, scripts/cli-timeout-policy.py is a bare `import
        # tomllib` again and the gate must say so.
        source = TOOL.read_text(encoding="utf-8")
        findings, unscanned = policy.scan(source, "cli-timeout-policy.py")
        self.assertEqual([str(finding) for finding in findings], [])
        self.assertEqual(unscanned, [])
        stripped = re.sub(
            r"\nif sys\.version_info < \(3, 11\):\n(?:    .*\n|\n)*?    \)\n",
            "\n",
            source,
        )
        self.assertNotEqual(stripped, source, "the guard was not found to remove")
        self.assertEqual(descriptions(stripped), ["the `tomllib` module"])


class HostTests(unittest.TestCase):
    def test_the_tool_reports_the_floor_rather_than_a_missing_module(self) -> None:
        """Run the real tool and check which failure this host gets.

        Both branches assert something real, so this case cannot quietly become
        a no-op on either side of the floor -- which is how the RUE-1506
        mechanism tests first went wrong.
        """
        result = subprocess.run(
            [sys.executable, str(TOOL), "--help"],
            capture_output=True,
            text=True,
        )
        if sys.version_info < policy.FLOOR:
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("ModuleNotFoundError", result.stderr)
            self.assertIn("requires Python 3.11 or newer", result.stderr)
            self.assertIn("AGENTS.md", result.stderr)
        else:
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("--policy", result.stdout)


if __name__ == "__main__":
    unittest.main()
