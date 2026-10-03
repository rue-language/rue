#!/usr/bin/env python3
"""Focused tests for the dotslash bootstrap-centralization gate (RUE-1825)
and the cache-key rule it holds the action to (RUE-1854)."""

from __future__ import annotations

import sys
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

bootstrap = load_script("validate-dotslash-bootstrap.py", __file__)

CALLER = (
    "name: test\njobs:\n  build:\n    steps:\n"
    "      - uses: actions/checkout@v6\n"
    "      - name: Bootstrap dotslash\n"
    "        uses: ./.github/actions/bootstrap-dotslash\n"
)
PEER_CALLER = CALLER.replace(
    "        uses: ./.github/actions/bootstrap-dotslash\n",
    "        uses: ./.github/actions/bootstrap-dotslash\n"
    "        with:\n          with-peers: 'true'\n",
)
WEBSITE_CALLER = CALLER.replace(
    "        uses: ./.github/actions/bootstrap-dotslash\n",
    "        uses: ./.github/actions/bootstrap-dotslash\n"
    "        with:\n          with-website: 'true'\n",
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
FULL_ACTION = ACTION + (
    "inputs:\n"
    "  with-peers: 'false'\n"
    "  with-website: 'false'\n"
    "runs:\n  using: composite\n  steps:\n"
    "    - name: Cache pinned peer artifacts\n"
    "      if: inputs.with-peers == 'true'\n"
    "      uses: actions/cache@v5\n"
    "      with:\n        path: |\n          ~/.cache/dotslash\n"
    "          ~/Library/Caches/dotslash\n"
    "        key: dotslash-peers-${{ inputs.cache-name }}-${{ hashFiles('hugo', 'zola') }}\n"
    "    - name: Warm pinned peer artifacts\n"
    "      if: inputs.with-peers == 'true'\n"
    "      run: scripts/warm-dotslash-tools.sh peers\n"
    "    - name: Cache pinned website artifacts\n"
    "      if: inputs.with-website == 'true'\n"
    "      uses: actions/cache@v5\n"
    "      with:\n        path: |\n          ~/.cache/dotslash\n"
    "          ~/Library/Caches/dotslash\n"
    "        key: dotslash-website-${{ inputs.cache-name }}-${{ hashFiles('tailwindcss') }}\n"
    "    - name: Warm pinned website artifacts\n"
    "      if: inputs.with-website == 'true'\n"
    "      run: scripts/warm-dotslash-tools.sh website\n"
)


class DotslashBootstrapTests(unittest.TestCase):
    def validate(
        self,
        workflows: dict[str, str] | None = None,
        action: str | None = FULL_ACTION,
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
            action=FULL_ACTION.replace("    - uses: facebook/install-dotslash@v2\n", "")
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("no longer installs dotslash", errors[0])

    def test_rejects_an_action_that_stopped_caching(self) -> None:
        errors = self.validate(
            action="name: Bootstrap dotslash\nruns:\n  using: composite\n  steps:\n"
            "    - uses: facebook/install-dotslash@v2\n"
        )
        self.assertTrue(any("no longer declares a dotslash cache" in error for error in errors))

    def test_rejects_a_key_hashing_the_wrapper_instead_of_the_manifest(self) -> None:
        # RUE-1854: the wrapper does not change on a pin bump, so a key on it
        # stays put and the stale store is never replaced. Both halves are
        # reported: the wrong file hashed, and the right one missing.
        errors = self.validate(action=FULL_ACTION.replace("hashFiles('buck2-bin')", "hashFiles('buck2')"))
        self.assertEqual(len(errors), 2, errors)
        self.assertIn("hashes the 'buck2' wrapper", errors[0])
        self.assertIn("does not hash 'buck2-bin'", errors[1])

    def test_accepts_additional_tool_manifests_in_the_key(self) -> None:
        # The affected-targets job shares the store with btd, so that variant
        # of the key legitimately hashes both manifests.
        errors = self.validate(
            action=FULL_ACTION.replace("hashFiles('buck2-bin')", "hashFiles('buck2-bin', 'btd')")
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

    def test_real_bootstrap_has_separate_pinned_peer_and_website_caches(self) -> None:
        errors = bootstrap.check_action_text(FULL_ACTION, Path("bootstrap-dotslash/action.yml"))
        self.assertEqual(errors, [])

    def test_requires_peer_and_website_consumers_to_opt_into_their_cache(self) -> None:
        workflows = {
            "ci.yml": PEER_CALLER + "      - run: scripts/gazette-corpus-diff.py peers\n",
            "deploy-website.yml": WEBSITE_CALLER + "      - run: website/build.sh\n",
            "performance-collect.yml": PEER_CALLER + "      - run: --peer-state-dir\n",
            "performance-calibration.yml": PEER_CALLER + "      - run: --peer-state-dir\n",
        }
        errors = bootstrap.check_tool_cache_callers(workflows)
        self.assertEqual(errors, [])

    def test_reproduces_missing_tool_cache_coverage_and_pin_invalidation(self) -> None:
        # The current action only keys its shared store to buck2-bin (and btd
        # for one caller). A fast exact hit can therefore be saved without
        # Hugo, Zola, or Tailwind and suppress those later downloads forever.
        errors = bootstrap.check_action_text(ACTION, Path("action.yml"))
        self.assertTrue(any("peer cache key" in error for error in errors), errors)
        self.assertTrue(any("website cache key" in error for error in errors), errors)

    def test_rejects_callers_that_skip_verified_warming(self) -> None:
        workflows = {
            "ci.yml": CALLER + "      - run: scripts/gazette-corpus-diff.py peers\n",
            "deploy-website.yml": CALLER + "      - run: website/build.sh\n",
            "performance-collect.yml": CALLER + "      - run: --peer-state-dir\n",
            "performance-calibration.yml": CALLER + "      - run: --peer-state-dir\n",
        }
        errors = bootstrap.check_tool_cache_callers(workflows)
        self.assertEqual(len(errors), 4, errors)

    def test_rejects_opt_in_on_a_different_job_or_after_the_consumer(self) -> None:
        wrong_job = (
            "name: test\njobs:\n"
            "  bootstrap:\n    steps:\n"
            "      - name: Bootstrap dotslash\n"
            "        uses: ./.github/actions/bootstrap-dotslash\n"
            "        with:\n          with-peers: 'true'\n"
            "  peers:\n    steps:\n"
            "      - run: scripts/gazette-corpus-diff.py peers\n"
        )
        after_consumer = (
            "name: test\njobs:\n  peers:\n    steps:\n"
            "      - run: scripts/gazette-corpus-diff.py peers\n"
            "      - name: Bootstrap dotslash\n"
            "        uses: ./.github/actions/bootstrap-dotslash\n"
            "        with:\n          with-peers: 'true'\n"
        )
        for workflow in (wrong_job, after_consumer):
            errors = bootstrap.check_tool_cache_callers({"ci.yml": workflow})
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("before its", errors[0])

    def test_peer_key_requires_both_manifest_hashes_and_platform_identity(self) -> None:
        action = FULL_ACTION.replace("hashFiles('hugo', 'zola')", "hashFiles('hugo')")
        action = action.replace("dotslash-peers-${{ inputs.cache-name }}", "dotslash-peers")
        errors = bootstrap.check_action_text(action, Path("action.yml"))
        self.assertTrue(any("does not hash the pinned 'zola'" in error for error in errors), errors)
        self.assertTrue(any("does not include the platform" in error for error in errors), errors)

    def test_rejects_warming_before_cache_restore(self) -> None:
        action = FULL_ACTION.replace(
            "        key: dotslash-peers-${{ inputs.cache-name }}-${{ hashFiles('hugo', 'zola') }}\n",
            "      run: scripts/warm-dotslash-tools.sh peers\n"
            "        key: dotslash-peers-${{ inputs.cache-name }}-${{ hashFiles('hugo', 'zola') }}\n",
        )
        errors = bootstrap.check_action_text(action, Path("action.yml"))
        self.assertTrue(any("warming must follow cache restore" in error for error in errors), errors)

    def test_rejects_unconditionally_warming_optional_tools(self) -> None:
        action = FULL_ACTION.replace(
            "      if: inputs.with-peers == 'true'\n"
            "      run: scripts/warm-dotslash-tools.sh peers\n",
            "      run: scripts/warm-dotslash-tools.sh peers\n",
        )
        errors = bootstrap.check_action_text(action, Path("action.yml"))
        self.assertTrue(any("warming step must use" in error for error in errors), errors)

    def test_failed_fetch_must_remain_fatal_and_use_verified_fetch_mode(self) -> None:
        warmer = bootstrap.ROOT / "scripts/warm-dotslash-tools.sh"
        with tempfile.TemporaryDirectory() as directory:
            temp = Path(directory)
            fakebin = temp / "bin"
            fakebin.mkdir()
            log = temp / "fetch.log"
            fake = fakebin / "dotslash"
            fake.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FETCH_LOG\"\n"
                "[ \"${FAIL_TOOL:-}\" != \"$3\" ]\n"
            )
            fake.chmod(0o755)
            env = dict(os.environ, PATH=str(fakebin), FETCH_LOG=str(log), FAIL_TOOL="./hugo")
            result = subprocess.run(
                ["/bin/bash", str(warmer), "peers"], env=env, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertEqual(log.read_text().splitlines(), ["-- fetch ./hugo"])


if __name__ == "__main__":
    unittest.main()
