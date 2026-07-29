#!/usr/bin/env python3
"""Focused tests for the required-CI container pin policy."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-required-ci-container-pins.py")
SPEC = importlib.util.spec_from_file_location("required_ci_container_pins", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
pins = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pins)


class RequiredCiContainerPinTests(unittest.TestCase):
    def validate(self, workflow: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ci.yml"
            path.write_text(workflow)
            return pins.validate(path)

    def test_rejects_latest_in_docker_run(self) -> None:
        errors = self.validate(
            "jobs:\n  lint:\n    steps:\n"
            "      - run: docker run --rm example/linter:latest@sha256:"
            + "a" * 64
            + " -color\n"
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("example/linter:latest", errors[0])

    def test_rejects_latest_in_job_or_service_image(self) -> None:
        workflow = (
            "jobs:\n"
            "  test:\n"
            "    container:\n      image: rust:latest\n"
            "    services:\n      db:\n        image: postgres:latest\n"
        )
        errors = self.validate(workflow)
        self.assertEqual(len(errors), 2)

    def test_accepts_release_with_immutable_digest(self) -> None:
        self.assertEqual(
            self.validate(
                "steps:\n"
                "  - run: docker run example/linter:1.2.3@sha256:"
                + "a" * 64
                + " -color\n"
            ),
            [],
        )

    def digest_pins(self, source: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "remote_cache.bzl"
            path.write_text(source)
            return pins.validate_digest_pins(path)

    def test_accepts_digest_pinned_executor_image(self) -> None:
        self.assertEqual(
            self.digest_pins(
                '                    "container-image": "docker://gcr.io/x/y@sha256:'
                + "0" * 64
                + '",\n'
            ),
            [],
        )

    def test_rejects_executor_image_without_digest(self) -> None:
        errors = self.digest_pins('"container-image": "docker://gcr.io/x/y:latest",\n')
        self.assertEqual(len(errors), 1)
        self.assertIn("is not pinned", errors[0])

    def test_rejects_truncated_executor_digest(self) -> None:
        errors = self.digest_pins(
            '"container-image": "docker://gcr.io/x/y@sha256:' + "0" * 63 + '",\n'
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("is not pinned", errors[0])

    def test_rejects_digest_pinned_file_with_no_image(self) -> None:
        # A vacuous pass is the failure mode this gate exists to prevent.
        errors = self.digest_pins('remote_execution_properties = {"OSFamily": "Linux"}\n')
        self.assertEqual(len(errors), 1)
        self.assertIn("no container-image property", errors[0])

    def test_ignores_commented_executor_image(self) -> None:
        errors = self.digest_pins(
            '# "container-image": "docker://gcr.io/x/y:latest",\n'
            '"container-image": "docker://gcr.io/x/y@sha256:' + "0" * 64 + '",\n'
        )
        self.assertEqual(errors, [])

    def test_ignores_latest_in_comments_and_quoted_hashes(self) -> None:
        self.assertEqual(
            self.validate(
                "# Never use example/linter:latest here.\n"
                "env:\n  MESSAGE: \"the # character is data\"\n"
                "steps:\n  - run: echo ok # example/linter:latest is forbidden\n"
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
