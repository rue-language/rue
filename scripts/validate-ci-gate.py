#!/usr/bin/env python3
"""Validate the stable CI aggregate, job inventory, and platform ownership."""

from __future__ import annotations

import argparse
import importlib.util
import re
from pathlib import Path

RESULTS_SCRIPT = Path(__file__).with_name("ci-required-results.py")
NATIVE_RUNNER_SCRIPT = Path(__file__).with_name("run-native-platform-corpus.sh")
SPEC = importlib.util.spec_from_file_location("ci_required_results", RESULTS_SCRIPT)
RESULTS = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(RESULTS)

ACTION_ID = r"[A-Za-z_][A-Za-z0-9_-]*"
JOB_RE = re.compile(rf"^  ({ACTION_ID}):\s*$", re.MULTILINE)


def job_blocks(workflow: str) -> dict[str, str]:
    jobs_marker = workflow.find("\njobs:\n")
    if jobs_marker < 0:
        raise ValueError("workflow has no top-level jobs mapping")
    body = workflow[jobs_marker + len("\njobs:\n") :]
    matches = list(JOB_RE.finditer(body))
    return {
        match.group(1): body[match.start() : matches[index + 1].start()]
        if index + 1 < len(matches)
        else body[match.start() :]
        for index, match in enumerate(matches)
    }


def list_needs(block: str) -> set[str]:
    match = re.search(
        rf"^    needs:\n((?:      - {ACTION_ID}\n)+)", block, re.MULTILINE
    )
    if not match:
        return set()
    return set(re.findall(rf"^      - ({ACTION_ID})$", match.group(1), re.MULTILINE))


def validate(ci_path: Path, native_runner_path: Path = NATIVE_RUNNER_SCRIPT) -> list[str]:
    workflow = ci_path.read_text()
    native_runner = native_runner_path.read_text()
    errors: list[str] = []
    try:
        jobs = job_blocks(workflow)
    except ValueError as error:
        return [f"{ci_path}: {error}"]

    expected_jobs = set(RESULTS.EXPECTED_REQUIRED_JOBS) | {"ci-success"}
    if set(jobs) != expected_jobs:
        missing = sorted(expected_jobs - set(jobs))
        extra = sorted(set(jobs) - expected_jobs)
        if missing:
            errors.append(f"CI job inventory missing: {', '.join(missing)}")
        if extra:
            errors.append(f"CI job inventory has unaggregated jobs: {', '.join(extra)}")

    gate = jobs.get("ci-success", "")
    if "    name: CI success\n" not in gate:
        errors.append("ci-success must expose the stable displayed name 'CI success'")
    if "    if: ${{ always() }}\n" not in gate:
        errors.append("ci-success must use if: always()")
    actual_needs = list_needs(gate)
    expected_needs = set(RESULTS.EXPECTED_REQUIRED_JOBS)
    if actual_needs != expected_needs:
        errors.append(
            "ci-success needs drift: expected "
            + ", ".join(sorted(expected_needs))
            + "; got "
            + ", ".join(sorted(actual_needs))
        )
    for required in (
        "${{ toJSON(needs) }}",
        "scripts/ci-required-results.py",
        "${{ github.event_name }}",
    ):
        if required not in gate:
            errors.append(f"ci-success no longer evaluates {required}")

    contract = jobs.get("ci-contract", "")
    if "    if: ${{ always() }}\n" not in contract:
        errors.append("ci-contract must be independent and always run")
    if "    needs:" in contract:
        errors.append("ci-contract must not depend on another CI job")
    if "scripts/validate-ci-gate.py .github/workflows/ci.yml" not in contract:
        errors.append("ci-contract no longer runs the structural validator")
    if "scripts/validate-tier-ci-selectors.py" not in contract:
        errors.append("ci-contract no longer proves every test tier is CI-selected")

    remote = jobs.get("remote-execution", "")
    if "    if: github.event_name == 'merge_group'\n" not in remote:
        errors.append("remote-execution must remain merge-group-only")

    linux = jobs.get("linux-premerge", "")
    for required in (
        "runs-on: ubuntu-latest",
        "Run complete target-independent premerge suite",
        "RUE_TEST_TIER: premerge",
        "./test.sh",
        "Run explicit cross-backend compilation and encoding coverage",
        "//crates/rue-codegen:rue-codegen-test",
    ):
        if required not in linux:
            errors.append(f"linux-premerge responsibility missing {required!r}")

    native = jobs.get("native-platforms", "")
    for required in (
        "os: ubuntu-24.04-arm",
        "name: linux-arm64",
        "os: macos-15",
        "name: macos-arm64",
        "//crates/rue-compiler:rue-compiler-test",
        "//crates/rue-codegen:rue-codegen-test",
        "//crates/rue-linker:rue-linker-test",
        "//crates/rue-runtime:rue-runtime-test",
        "//crates/rue-runtime-abi:rue-runtime-abi-test",
        "scripts/run-native-platform-corpus.sh",
        "scripts/rue cli abi",
        "scripts/rue cli cli.linker",
        "scripts/rue cli cli.fs_file_io",
    ):
        if required not in native:
            errors.append(f"native-platforms responsibility missing {required!r}")
    if "./test.sh" in native or "//:spec-tests" in native:
        errors.append("native-platforms must not duplicate the target-independent broad/spec suite")
    for required in (
        "export RUE_PLATFORM_CASE_SELECTION=native",
        "RUE_CLI_CASE_TIER=premerge",
        "//crates/rue-spec:rue-spec",
        "//crates/rue-cli-tests:rue-cli-tests",
    ):
        if required not in native_runner:
            errors.append(
                f"native platform corpus runner no longer guarantees {required!r}"
            )

    corpus = jobs.get("platform-corpus", "")
    check_names = set(re.findall(r"check_name: ([a-z0-9-]+)", corpus))
    expected_checks = {
        "linux-x64-cli-shard-0",
        "linux-x64-cli-shard-1",
        "linux-x64-cli-shard-2",
        "linux-x64-cli-shard-3",
        "linux-x64-spec",
        "linux-x64-oracle-diff",
        "linux-x64-oracle-diff-spec",
    }
    if check_names != expected_checks:
        errors.append(
            "platform-corpus responsibility drift: expected "
            + ", ".join(sorted(expected_checks))
            + "; got "
            + ", ".join(sorted(check_names))
        )

    for sanitizer in ("valgrind", "asan"):
        block = jobs.get(sanitizer, "")
        if "runs-on: ubuntu-latest" not in block:
            errors.append(f"{sanitizer} is no longer consolidated into CI")
    if "inputs.large_program" not in jobs.get("valgrind", ""):
        errors.append("manual Valgrind large_program selection was not preserved")

    if "  pull_request:\n" not in workflow or "  merge_group:\n" not in workflow:
        errors.append("CI must run on both pull_request and merge_group")
    return [f"{ci_path}: {error}" for error in errors]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("workflow", type=Path)
    args = parser.parse_args()
    errors = validate(args.workflow)
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    print(
        "CI gate valid: stable aggregate covers the exact required inventory "
        "and platform responsibilities"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
