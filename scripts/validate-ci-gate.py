#!/usr/bin/env python3
"""Validate the stable CI aggregate, job inventory, and platform ownership."""

from __future__ import annotations

import argparse
import importlib.util
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import job_blocks

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

# RUE-1265: the duplication gate models these three steps as scheduled work, so
# the filters have to be one fact. Importing them here means a filter added to
# the workflow without teaching the gate about it fails this validator, rather
# than quietly running a corpus slice the duplication comparison cannot see.
DUPLICATION_SCRIPT = Path(__file__).with_name("validate-test-duplication.py")
_DUP_SPEC = importlib.util.spec_from_file_location(
    "validate_test_duplication", DUPLICATION_SCRIPT
)
DUPLICATION = importlib.util.module_from_spec(_DUP_SPEC)
assert _DUP_SPEC.loader is not None
# Registered before execution: the module defines dataclasses, and @dataclass
# resolves annotations through sys.modules[cls.__module__].
sys.modules["validate_test_duplication"] = DUPLICATION
_DUP_SPEC.loader.exec_module(DUPLICATION)

ACTION_ID = r"[A-Za-z_][A-Za-z0-9_-]*"


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



AFFECTED_TARGETS_SCRIPT = Path(__file__).with_name("affected-targets")


def lane_targets(lane: str, script: Path = AFFECTED_TARGETS_SCRIPT) -> set[str]:
    """The Buck targets the determinator believes a gated lane executes."""
    result = subprocess.run(
        ["bash", str(script), "lane-targets", lane],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return set()
    return set(result.stdout.split())


def lane_target_drift(workflow: str, script: Path = AFFECTED_TARGETS_SCRIPT) -> list[str]:
    """Targets a lane's job runs that its determinator entry does not represent.

    RUE-1130 gates and narrows lanes against `lane_targets`, so a target the job
    runs but the determinator does not know about is invisible to selection: the
    lane can be deselected, or narrowed away, by a diff that actually reaches
    it. That is the RUE-924 failure mode arriving through a stale list rather
    than a missing job, and the two lists are far apart in the tree, so nothing
    but a gate keeps them together.
    """
    jobs = job_blocks(workflow)
    native = jobs.get("native-platforms", "")
    ran = set(re.findall(r"^\s+(//[A-Za-z0-9_./-]+:[A-Za-z0-9_.-]+)$", native, re.MULTILINE))
    if not ran:
        return ["native-platforms declares no Buck targets; the lane gate cannot be checked"]
    known = lane_targets("native-linux-arm64", script)
    if not known:
        return ["scripts/affected-targets lane-targets native-linux-arm64 produced nothing"]
    missing = sorted(ran - known)
    return [
        f"native-platforms runs {target} but scripts/affected-targets does not list it "
        "for native-linux-arm64, so selection cannot see it"
        for target in missing
    ]


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
    # RUE-1507: the scheduled-workflow health check is the only thing that reads
    # unattended run history on a path a human actually looks at. Dropping the
    # step would restore the original silence without changing a single
    # scheduled workflow, so its presence is part of the CI contract. The
    # `actions: read` scope is pinned with it: without that, the step fails
    # closed on every run rather than reporting.
    if "scripts/check-scheduled-workflows.py" not in contract:
        errors.append(
            "ci-contract no longer checks that scheduled workflows still succeed; "
            "without it a weekly safeguard can fail every run unnoticed (RUE-1507)"
        )
    elif not any(
        line.strip() == "actions: read"
        for line in contract.splitlines()
        if not line.lstrip().startswith("#")
    ):
        # Matched as an executable line, not a substring: the comment above the
        # permissions block explains the scope and so contains its own name.
        # A substring test would be satisfied by that prose alone, leaving the
        # scope deletable and the next run dead on an opaque 403.
        errors.append(
            "ci-contract runs the scheduled-workflow check without `actions: read`; "
            "the run-history query needs that scope"
        )

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
        # RUE-1258: the pin gate. Losing it restores the silence that let the
        # published series freeze for ten days while every job stayed green,
        # so its presence is part of the CI contract rather than a step someone
        # may quietly drop. Its sibling staleness gate is pinned below, in the
        # job RUE-1504 moved it to.
        "check-pins",
        # RUE-1265: the ADR-0069 §2 duplication gate. It is the only check that
        # compares test contents rather than target lists, so nothing else in
        # CI can see a target becoming a superset of another; losing the step
        # restores the blind spot that hid 223s of re-executed work for weeks.
        "scripts/validate-test-duplication.py",
    ):
        if required not in linux:
            errors.append(f"linux-premerge responsibility missing {required!r}")
    if "scripts/validate-performance-stall.py" in linux:
        errors.append(
            "the staleness gate belongs to performance-staleness (RUE-1504); "
            "running it inside linux-premerge puts ~1m50s back on the critical path"
        )

    # RUE-1504. The staleness gate is required work that merely stopped being
    # premerge's work, so the contract follows it rather than relaxing. Both
    # halves matter: the step itself, and the deep checkout without which it
    # fails instead of passing.
    staleness = jobs.get("performance-staleness", "")
    for required in (
        "runs-on: ubuntu-latest",
        "scripts/validate-performance-stall.py",
        "//crates/rue-bench:rue-bench",
        # The gate counts trunk commits back to the newest measured one, which
        # a depth-1 checkout cannot reach (RUE-1258).
        "fetch-depth: 0",
        # The store is append-only and unbounded, while the question is about
        # the live epoch alone. Checking out and parsing all of it made this
        # job the run's long pole, and would again: the cost returns silently,
        # as a slow job rather than a failing one (RUE-1542).
        "staleness-inputs",
    ):
        if required not in staleness:
            errors.append(f"performance-staleness responsibility missing {required!r}")
    # ADR-0072 Decision 9 keeps the runtime series outside this gate on purpose:
    # its remedy is parser work rather than a manifest edit, so a stalled
    # runtime series is a triage item, never a repository-wide block. The step
    # says so in a comment, so only an executable line counts as a violation.
    if any(
        "--runtime-manifest" in line and not line.lstrip().startswith("#")
        for line in staleness.splitlines()
    ):
        errors.append(
            "performance-staleness must stay compile-time only; ADR-0072 "
            "Decision 9 keeps the runtime series out of this gate"
        )
    if "    needs:" in staleness:
        errors.append(
            "performance-staleness must not depend on another CI job; it asks a "
            "repository-wide question and gains nothing by waiting"
        )
    # A required check that cannot fail is not a gate. `continue-on-error` is
    # the one-line version of that, on the job or on the step: the run stays
    # green and `ci-required-results.py` still sees `success`, so nothing
    # downstream notices. The gate used to be one step among many in a busy
    # lane; it is now alone in a small job where such a line would be easy to
    # add and hard to see.
    if "continue-on-error" in staleness:
        errors.append(
            "performance-staleness must not use continue-on-error; the gate has "
            "no bypass, and a required check that reports success regardless of "
            "the gate's result is one"
        )

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
    ) + tuple(
        f"scripts/rue cli {filter_}" for filter_ in DUPLICATION.NATIVE_CLI_FILTERS
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
    errors.extend(lane_target_drift(workflow))

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
