#!/usr/bin/env python3
"""Fail-closed evaluation for the stable required CI aggregate."""

from __future__ import annotations

import argparse
import json
from collections.abc import Mapping

EXPECTED_REQUIRED_JOBS = (
    "fmt",
    "clippy",
    "actionlint",
    "remote-execution",
    "rust-project",
    "linux-premerge",
    "native-platforms",
    "compiler-reproducibility",
    "rue-program-digests",
    "affected-targets",
    "platform-corpus",
    "release",
    "valgrind",
    "asan",
    "ci-contract",
)


def validate_required_results(event: str, needs: Mapping[str, object]) -> list[str]:
    errors: list[str] = []
    expected = set(EXPECTED_REQUIRED_JOBS)
    actual = set(needs)
    if missing := sorted(expected - actual):
        errors.append(f"aggregate is missing expected job result(s): {', '.join(missing)}")
    if extra := sorted(actual - expected):
        errors.append(f"aggregate received unexpected job result(s): {', '.join(extra)}")

    if event not in {"pull_request", "merge_group", "workflow_dispatch"}:
        errors.append(f"unsupported CI event {event!r}")

    for job in EXPECTED_REQUIRED_JOBS:
        record = needs.get(job)
        if not isinstance(record, Mapping):
            if job in needs:
                errors.append(f"{job}: malformed result record")
            continue
        result = record.get("result")
        if job == "remote-execution":
            expected_result = "success" if event == "merge_group" else "skipped"
        else:
            expected_result = "success"
        if result != expected_result:
            errors.append(f"{job}: expected {expected_result}, got {result!r}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--event", required=True)
    parser.add_argument("--needs-json", required=True)
    args = parser.parse_args()
    try:
        needs = json.loads(args.needs_json)
    except json.JSONDecodeError as error:
        print(f"error: invalid needs JSON: {error}")
        return 2
    if not isinstance(needs, dict):
        print("error: needs JSON must be an object")
        return 2
    errors = validate_required_results(args.event, needs)
    if errors:
        for error in errors:
            print(f"::error::{error}")
        return 1
    print(f"CI success: all {len(EXPECTED_REQUIRED_JOBS)} expected dependencies satisfied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
