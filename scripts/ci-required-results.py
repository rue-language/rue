#!/usr/bin/env python3
"""Fail-closed evaluation for the stable required CI aggregate.

`ci-success` passes `toJSON(needs)`, which is exactly its own `needs:` list;
`scripts/validate-ci-gate.py` proves that list names every job in the
workflow. So the aggregate has nothing to enumerate on its own: every listed
result must be `success`, with one exception — the merge-group-only
`remote-execution` canary is `skipped` on every other event (RUE-320).
"""

from __future__ import annotations

import argparse
import json
from collections.abc import Mapping

SUPPORTED_EVENTS = ("pull_request", "merge_group", "workflow_dispatch")
MERGE_GROUP_ONLY = "remote-execution"


def validate_required_results(event: str, needs: Mapping[str, object]) -> list[str]:
    errors: list[str] = []
    if event not in SUPPORTED_EVENTS:
        errors.append(f"unsupported CI event {event!r}")
    if not needs:
        errors.append("aggregate received no job results; ci-success has no needs")
    if MERGE_GROUP_ONLY not in needs:
        errors.append(f"aggregate is missing the {MERGE_GROUP_ONLY} canary")
    for job in sorted(needs):
        record = needs[job]
        if not isinstance(record, Mapping):
            errors.append(f"{job}: malformed result record")
            continue
        result = record.get("result")
        if job == MERGE_GROUP_ONLY and event != "merge_group":
            expected = "skipped"
        else:
            expected = "success"
        if result != expected:
            errors.append(f"{job}: expected {expected}, got {result!r}")
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
    print(f"CI success: all {len(needs)} required dependencies satisfied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
