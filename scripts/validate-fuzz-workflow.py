#!/usr/bin/env python3
"""Validate the per-target contracts in the nightly fuzz workflow."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Optional


STEP_HEADER = re.compile(r"^      - name: ", re.MULTILINE)
AGGREGATED_TARGET = re.compile(
    r"steps\.fuzz-[a-z0-9-]+\.outcome == 'failure' \|\|"
)


def step_blocks(workflow: str) -> list[str]:
    """Return workflow step blocks without requiring a YAML dependency."""
    starts = [match.start() for match in STEP_HEADER.finditer(workflow)]
    return [
        workflow[start : starts[index + 1] if index + 1 < len(starts) else None]
        for index, start in enumerate(starts)
    ]


def matching_block(blocks: list[str], line: str, indent: int = 8) -> Optional[str]:
    matches = [block for block in blocks if has_line(block, line, indent)]
    if len(matches) != 1:
        return None
    return matches[0]


def has_line(block: str, line: str, indent: int) -> bool:
    return f"{' ' * indent}{line}" in block.splitlines()


def has_step_field(block: str, field: str) -> bool:
    prefix = f"        {field}:"
    return any(line.startswith(prefix) for line in block.splitlines())


def scalar_lines(block: str, field: str) -> list[str]:
    """Return one top-level step block scalar without adjacent YAML fields."""
    lines = block.splitlines()
    marker = f"        {field}: |"
    folded_marker = f"        {field}: >-"
    starts = [
        index
        for index, line in enumerate(lines)
        if line in (marker, folded_marker)
    ]
    if len(starts) != 1:
        return []
    body = []
    for line in lines[starts[0] + 1 :]:
        if line.startswith("        ") and not line.startswith("          "):
            break
        if line.startswith("          "):
            body.append(line[10:])
    return body


def matching_line_block(blocks: list[str], line: str, indent: int) -> Optional[str]:
    matches = [block for block in blocks if has_line(block, line, indent)]
    if len(matches) != 1:
        return None
    return matches[0]


def validate_target(workflow: str, target: str) -> list[str]:
    """Return missing or ambiguous nightly contracts for one fuzz target."""
    blocks = step_blocks(workflow)
    step_id = f"fuzz-{target.replace('_', '-')}"
    generation_key = (
        f"key: rue-fuzz-corpus-v3-{target}-"
        "${{ github.run_id }}-${{ github.run_attempt }}"
    )
    errors = []

    command = (
        "--mutate --max-time=300 "
        f"--evolve-corpus=crates/rue-fuzz/nightly-corpus/{target} "
        f"{target} crates/rue-fuzz/nightly-input/{target}"
    )
    mutation = matching_block(blocks, f"run: ./buck2 run //crates/rue-fuzz:rue-fuzz -- {command}")
    if (
        mutation is None
        or has_step_field(mutation, "if")
        or not all(
            has_line(mutation, field, 8)
            for field in (
                f"id: {step_id}",
                "continue-on-error: true",
                "timeout-minutes: 10",
            )
        )
    ):
        errors.append(f"{target}:five-minute-step")

    restore_path = f"path: crates/rue-fuzz/nightly-restored/{target}"
    restore = matching_line_block(
        [block for block in blocks if has_line(block, "uses: actions/cache/restore@v5", 8)],
        restore_path,
        10,
    )
    if restore is None or has_step_field(restore, "if"):
        errors.append(f"{target}:restore-path")
    else:
        if not has_line(restore, generation_key, 10):
            errors.append(f"{target}:restore-generation-key")
        if not has_line(restore, f"restore-keys: rue-fuzz-corpus-v3-{target}-", 10):
            errors.append(f"{target}:restore-prefix")

    save = matching_line_block(
        [block for block in blocks if has_line(block, "uses: actions/cache/save@v5", 8)],
        restore_path,
        10,
    )
    if save is None:
        errors.append(f"{target}:save-path")
    else:
        if not has_line(save, generation_key, 10):
            errors.append(f"{target}:save-generation-key")
        if not has_line(
            save, "if: always() && steps.publish_clean_corpus.outcome == 'success'", 8
        ):
            errors.append(f"{target}:save-condition")
        if not has_line(save, "continue-on-error: true", 8):
            errors.append(f"{target}:save-error-policy")

    aggregation = matching_block(
        blocks, "- name: Fail if any fuzz step found crashes", 6
    )
    aggregation_line = f"steps.{step_id}.outcome == 'failure' ||"
    aggregation_body = [] if aggregation is None else scalar_lines(aggregation, "if")
    aggregation_body = [line for line in aggregation_body if line]
    canonical_aggregation = (
        len(aggregation_body) >= 2
        and all(AGGREGATED_TARGET.fullmatch(line) for line in aggregation_body[:-1])
        and aggregation_body[-1] == "steps.fuzz-differential.outcome == 'failure'"
    )
    if not canonical_aggregation or aggregation_line not in aggregation_body:
        errors.append(f"{target}:failure-aggregation")

    reporting = matching_block(
        blocks, "- name: Report crashes (Linear, GitHub Issues fallback)", 6
    )
    report_line = f"record {target} '${{{{ steps.{step_id}.outcome }}}}'"
    if (
        reporting is None
        or not has_line(reporting, "if: failure()", 8)
        or report_line not in scalar_lines(reporting, "run")
    ):
        errors.append(f"{target}:reporting")

    return errors


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("workflow", type=Path)
    parser.add_argument("targets", nargs="+")
    args = parser.parse_args(argv)

    workflow = args.workflow.read_text()
    errors = []
    for target in args.targets:
        errors.extend(validate_target(workflow, target))
    if errors:
        print(
            "registered fuzz target contract(s) missing or ambiguous: "
            + " ".join(errors),
            file=sys.stderr,
        )
        return 1
    print(f"validated {len(args.targets)} registered nightly fuzz target(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
