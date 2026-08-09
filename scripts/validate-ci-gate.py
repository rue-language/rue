#!/usr/bin/env python3
"""Validate the stable CI aggregate, job inventory, and platform ownership."""

from __future__ import annotations

import argparse
import importlib.util
import re
from pathlib import Path

RESULTS_SCRIPT = Path(__file__).with_name("ci-required-results.py")
NATIVE_RUNNER_SCRIPT = Path(__file__).with_name("run-native-platform-corpus.sh")
TEST_RUNNER_SOURCE = (
    Path(__file__).resolve().parents[1] / "crates/rue-test-runner/src/lib.rs"
)
ROOT_BUCK = Path(__file__).resolve().parents[1] / "BUCK"

# RUE-1161: the platform each entry of the harness's CI_EXECUTED_TARGETS claims
# a required lane for, and the workflow text that proves that lane exists. The
# harness refuses to credit specification coverage to a case scoped to a
# platform outside this list, so a lane removed from ci.yml without updating the
# constant would silently keep crediting cases nothing runs.
PLATFORM_LANES = {
    "x86-64-linux": ("linux-premerge", "runs-on: ubuntu-latest"),
    "aarch64-linux": ("native-platforms", "os: ubuntu-24.04-arm"),
    "aarch64-macos": ("native-platforms", "os: macos-15"),
}

CI_EXECUTED_TARGETS_RE = re.compile(
    r"pub const CI_EXECUTED_TARGETS: &\[&str\] = &\[(?P<body>.*?)\];", re.DOTALL
)

DEDICATED_LANE_LABEL = "rue_ci_dedicated_lane"
DEDICATED_LANE_RE = re.compile(
    r'name = "(?P<name>[a-z0-9-]+)",\s*\n\s*labels = \[[^\]]*"'
    + DEDICATED_LANE_LABEL
    + r'"',
    re.MULTILINE,
)


def dedicated_lane_targets(buck_source: str) -> list[str]:
    """Corpora BUCK marks as owning their own required-CI job."""
    return [f"//:{match.group('name')}" for match in DEDICATED_LANE_RE.finditer(buck_source)]


def uncovered_dedicated_lanes(buck_source: str, corpus_block: str) -> list[str]:
    """Labeled corpora that no platform-corpus matrix entry actually runs.

    A corpus carrying the label is skipped by the linux-premerge suite, so if
    the matrix does not run it — directly or through its shards — it stops being
    covered at all. That is the RUE-924 false-green shape, reached by way of a
    label instead of a log line, so it fails the CI contract.
    """
    matrix_targets = set(re.findall(r"target: (\S+)", corpus_block))
    uncovered = []
    for target in dedicated_lane_targets(buck_source):
        sharded = {
            candidate
            for candidate in matrix_targets
            if candidate.startswith(f"{target}-shard-")
        }
        if target not in matrix_targets and not sharded:
            uncovered.append(target)
    return uncovered


def ci_executed_targets(runner_source: str) -> list[str]:
    """The platform names the shared test harness believes required CI runs."""
    match = CI_EXECUTED_TARGETS_RE.search(runner_source)
    if not match:
        raise ValueError("CI_EXECUTED_TARGETS not found in the test-runner source")
    return re.findall(r'"([^"]+)"', match.group("body"))


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



def declared_outputs(block: str) -> set[str]:
    """Output names a job block declares under `outputs:`."""
    match = re.search(r"^    outputs:\n((?:      \S.*\n|      #.*\n)+)", block, re.MULTILINE)
    if not match:
        return set()
    return set(re.findall(r"^      ([A-Za-z_][A-Za-z0-9_-]*):", match.group(1), re.MULTILINE))


def undeclared_need_outputs(workflow: str, jobs: dict[str, str]) -> list[str]:
    """`needs.<job>.outputs.<name>` references with no matching declaration.

    GitHub resolves an undeclared job output to the empty string rather than
    failing, so a typo or a forgotten declaration becomes a silent behaviour
    change in whatever consumes it. For the RUE-1130 lane gates that means
    every lane deselecting on a selective run, which is invisible on any pull
    request that touches CI (those force a full run) and therefore exactly the
    kind of drop RUE-924 exists to prevent.
    """
    problems: list[str] = []
    for job, name in set(re.findall(r"needs\.([A-Za-z0-9_-]+)\.outputs\.([A-Za-z0-9_-]+)", workflow)):
        block = jobs.get(job)
        if block is None:
            problems.append(f"needs.{job}.outputs.{name} references unknown job {job!r}")
        elif name not in declared_outputs(block):
            problems.append(
                f"needs.{job}.outputs.{name} is referenced but {job!r} does not declare it; "
                "it would silently resolve to the empty string"
            )
    return sorted(problems)


def validate(
    ci_path: Path,
    native_runner_path: Path = NATIVE_RUNNER_SCRIPT,
    test_runner_path: Path = TEST_RUNNER_SOURCE,
    buck_path: Path = ROOT_BUCK,
) -> list[str]:
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
        # RUE-1258: both performance gates. Losing either restores the silence
        # that let the published series freeze for ten days while every job
        # stayed green, so their presence is part of the CI contract rather
        # than a step someone may quietly drop.
        "check-pins",
        "scripts/validate-performance-stall.py",
        # The staleness gate counts trunk commits back to the newest measured
        # one, which a depth-1 checkout cannot reach.
        "fetch-depth: 0",
    ):
        if required not in linux:
            errors.append(f"linux-premerge responsibility missing {required!r}")

    # RUE-1163: the corpora that own a platform-corpus job are marked in BUCK
    # with `rue_ci_dedicated_lane`, and test.sh subtracts that label on CI. The
    # workflow must not also name them in an environment variable — that was the
    # protocol the label replaced, and two sources would drift.
    if "RUE_CI_DEFER_HEAVY_SUITES" in workflow:
        errors.append(
            "RUE_CI_DEFER_HEAVY_SUITES is retired; mark the corpus "
            "`rue_ci_dedicated_lane` in BUCK instead"
        )

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

    # RUE-1161: the harness's platform responsibility matrix must name exactly
    # the platforms required CI executes cases on.
    try:
        declared = ci_executed_targets(test_runner_path.read_text())
    except (OSError, ValueError) as error:
        errors.append(f"platform responsibility matrix unreadable: {error}")
    else:
        if set(declared) != set(PLATFORM_LANES):
            errors.append(
                "CI_EXECUTED_TARGETS drift: harness declares "
                + ", ".join(sorted(declared))
                + "; workflow lanes cover "
                + ", ".join(sorted(PLATFORM_LANES))
            )
        for platform in declared:
            lane = PLATFORM_LANES.get(platform)
            if lane is None:
                continue
            job, marker = lane
            if marker not in jobs.get(job, ""):
                errors.append(
                    f"platform {platform} is declared CI-executed but job {job} "
                    f"no longer contains {marker!r}"
                )

    # RUE-1163: every corpus the linux-premerge suite skips by label must be run
    # by a platform-corpus entry, directly or through its shards.
    try:
        buck_source = buck_path.read_text()
    except OSError as error:
        errors.append(f"root BUCK file unreadable: {error}")
    else:
        labeled = dedicated_lane_targets(buck_source)
        if not labeled:
            errors.append(
                "no corpus carries rue_ci_dedicated_lane; linux-premerge would "
                "re-run the corpora that have their own jobs"
            )
        for target in uncovered_dedicated_lanes(buck_source, corpus):
            errors.append(
                f"{target} is marked {DEDICATED_LANE_LABEL} (so the premerge "
                "suite skips it) but no platform-corpus entry runs it"
            )

    for sanitizer in ("valgrind", "asan"):
        block = jobs.get(sanitizer, "")
        if "runs-on: ubuntu-latest" not in block:
            errors.append(f"{sanitizer} is no longer consolidated into CI")
    if "inputs.large_program" not in jobs.get("valgrind", ""):
        errors.append("manual Valgrind large_program selection was not preserved")

    errors.extend(undeclared_need_outputs(workflow, jobs))

    if "  pull_request:\n" not in workflow or "  merge_group:\n" not in workflow:
        errors.append("CI must run on both pull_request and merge_group")
    return [f"{ci_path}: {error}" for error in errors]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("workflow", type=Path)
    parser.add_argument(
        "--buck",
        type=Path,
        default=ROOT_BUCK,
        help="root BUCK file declaring which corpora own a required-CI job",
    )
    parser.add_argument(
        "--test-runner-source",
        type=Path,
        default=TEST_RUNNER_SOURCE,
        help="rue-test-runner source declaring the platform responsibility matrix",
    )
    args = parser.parse_args()
    errors = validate(
        args.workflow, NATIVE_RUNNER_SCRIPT, args.test_runner_source, args.buck
    )
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
