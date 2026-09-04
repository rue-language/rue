#!/usr/bin/env python3
"""Focused tests for the dotslash bootstrap-centralization gate (RUE-1825)
and the cache-key rule it holds the action to (RUE-1854)."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

bootstrap = load_script("validate-dotslash-bootstrap.py", __file__)

CALLER = (
    "jobs:\n  build:\n    steps:\n"
    "      - uses: actions/checkout@v6\n"
    "      - name: Bootstrap dotslash\n"
    "        uses: ./.github/actions/bootstrap-dotslash\n"
)
ACTION = (
    "name: Bootstrap dotslash\n"
    "runs:\n  using: composite\n  steps:\n"
    "    - uses: facebook/install-dotslash@v2\n"
    "    - uses: actions/cache@v5\n"
    "      with:\n"
    "        path: ~/.cache/dotslash\n"
    "        key: dotslash-linux-x64-${{ hashFiles('buck2-bin') }}\n"
)


class DotslashBootstrapTests(unittest.TestCase):
    def validate(
        self,
        workflows: dict[str, str] | None = None,
        action: str | None = ACTION,
    ) -> list[str]:
        """Run the gate over a synthetic `.github` tree."""

        with tempfile.TemporaryDirectory() as directory:
            github = Path(directory) / ".github"
            (github / "workflows").mkdir(parents=True)
            for name, text in (workflows or {"ci.yml": CALLER}).items():
                (github / "workflows" / name).write_text(text)
            if action is not None:
                canonical = github / "actions" / "bootstrap-dotslash"
                canonical.mkdir(parents=True)
                (canonical / "action.yml").write_text(action)
            return bootstrap.validate(github)

    def test_accepts_a_workflow_that_goes_through_the_action(self) -> None:
        self.assertEqual(self.validate(), [])

    def test_rejects_a_direct_upstream_install(self) -> None:
        # The regression itself: a job that installs dotslash on its own is a
        # job whose cache step can go missing, which is how the seven
        # performance-workflow gaps were introduced.
        errors = self.validate(
            {
                "ci.yml": CALLER,
                "perf.yml": (
                    "jobs:\n  measure:\n    steps:\n"
                    "      - name: Install dotslash\n"
                    "        uses: facebook/install-dotslash@v2\n"
                ),
            }
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("installs dotslash directly", errors[0])
        self.assertIn("perf.yml:5", errors[0])

    def test_rejects_a_workflow_declaring_its_own_dotslash_cache(self) -> None:
        # The other half of the same copy: the cache without the install is
        # just as much a fork of the policy the action owns.
        errors = self.validate(
            {
                "ci.yml": CALLER
                + "      - uses: actions/cache@v5\n"
                "        with:\n"
                "          key: dotslash-linux-x64-${{ hashFiles('buck2-bin') }}\n"
            }
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("declares its own dotslash cache key", errors[0])

    def test_rejects_a_missing_canonical_action(self) -> None:
        errors = self.validate(action=None)
        self.assertEqual(len(errors), 1)
        self.assertIn("missing", errors[0])

    def test_rejects_an_action_that_stopped_installing(self) -> None:
        # Without this the gate would pass over workflows that conform to a
        # bootstrap which no longer bootstraps anything.
        errors = self.validate(
            action=ACTION.replace("    - uses: facebook/install-dotslash@v2\n", "")
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("no longer installs dotslash", errors[0])

    def test_rejects_an_action_that_stopped_caching(self) -> None:
        errors = self.validate(
            action="name: Bootstrap dotslash\nruns:\n  using: composite\n  steps:\n"
            "    - uses: facebook/install-dotslash@v2\n"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("no longer declares a dotslash cache", errors[0])

    def test_rejects_a_key_hashing_the_wrapper_instead_of_the_manifest(self) -> None:
        # RUE-1854: the wrapper does not change on a pin bump, so a key on it
        # stays put and the stale store is never replaced. Both halves are
        # reported: the wrong file hashed, and the right one missing.
        errors = self.validate(action=ACTION.replace("hashFiles('buck2-bin')", "hashFiles('buck2')"))
        self.assertEqual(len(errors), 2, errors)
        self.assertIn("hashes the 'buck2' wrapper", errors[0])
        self.assertIn("does not hash 'buck2-bin'", errors[1])

    def test_accepts_additional_tool_manifests_in_the_key(self) -> None:
        # The affected-targets job shares the store with btd, so that variant
        # of the key legitimately hashes both manifests.
        errors = self.validate(
            action=ACTION.replace("hashFiles('buck2-bin')", "hashFiles('buck2-bin', 'btd')")
        )
        self.assertEqual(errors, [])

    def test_rejects_a_tree_where_nothing_calls_the_action(self) -> None:
        # A renamed bootstrap leaves every workflow trivially conforming; the
        # caller count is what stops that from reading as a pass (RUE-1152).
        errors = self.validate({"ci.yml": "jobs:\n  build:\n    steps: []\n"})
        self.assertEqual(len(errors), 1)
        self.assertIn("no workflow uses", errors[0])

    def test_covers_a_later_added_yaml_workflow(self) -> None:
        # The gate takes the directory, so a workflow added tomorrow — in
        # either spelling of the extension — is checked without editing it.
        errors = self.validate(
            {
                "ci.yml": CALLER,
                "later.yaml": (
                    "jobs:\n  build:\n    steps:\n"
                    "      - uses: facebook/install-dotslash@v2\n"
                ),
            }
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("later.yaml:4", errors[0])

    def test_rejects_direct_buck_manifest_in_a_second_workflow(self) -> None:
        errors = self.validate(
            {
                "ci.yml": CALLER,
                "release.yml": (
                    "jobs:\n  release:\n    steps:\n"
                    "      - run: dotslash ./buck2-bin build //crates/rue:rue\n"
                ),
            }
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("release.yml:4", errors[0])
        self.assertIn("must reach Buck through repository `./buck2`", errors[0])

    def test_reports_every_offending_site(self) -> None:
        errors = self.validate(
            {
                "ci.yml": CALLER,
                "a.yml": "steps:\n  - uses: facebook/install-dotslash@v2\n",
                "b.yml": "steps:\n  - uses: facebook/install-dotslash@v2\n",
            }
        )
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("a.yml" in error for error in errors))
        self.assertTrue(any("b.yml" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
