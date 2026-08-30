#!/usr/bin/env python3
"""Validate the stable CI aggregate, job inventory, and platform ownership."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import job_blocks, load_script

NATIVE_RUNNER_SCRIPT = Path(__file__).with_name("run-native-platform-corpus.sh")
TEST_RUNNER_SOURCE = (
    Path(__file__).resolve().parents[1] / "crates/rue-test-runner/src/lib.rs"
)
ROOT_BUCK = Path(__file__).resolve().parents[1] / "BUCK"
VALGRIND_INSTALL_SCRIPT = Path(__file__).with_name("install-valgrind")

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


def _workflow_mentions_target(block: str, target: str) -> bool:
    """Whether executable workflow text names a dedicated target."""
    for line in block.splitlines():
        stripped = line.strip()
        if stripped.startswith("#"):
            continue
        if re.search(rf"(?<![A-Za-z0-9_.-]){re.escape(target)}(?![A-Za-z0-9_.-])", stripped):
            return True
    return False


def uncovered_dedicated_lanes(
    buck_source: str,
    corpus_block: str,
    owner_blocks=None,
) -> list[str]:
    """Labeled corpora that do not have exactly one dedicated owner.

    A corpus carrying the label is skipped by the linux-premerge suite, so if
    the matrix or an explicitly supplied owner job does not run it — directly or
    through its shards — it stops being covered at all. Conversely, two owners
    would reintroduce the same work under two lane contracts. Both are the
    RUE-924 false-green shape, reached by way of a label instead of a log line,
    so they fail the CI contract.
    """
    matrix_targets = set(re.findall(r"target: (\S+)", corpus_block))
    uncovered = []
    for target in dedicated_lane_targets(buck_source):
        sharded = {
            candidate
            for candidate in matrix_targets
            if candidate.startswith(f"{target}-shard-")
        }
        owners = []
        if target in matrix_targets or sharded:
            owners.append("platform-corpus")
        if owner_blocks:
            owners.extend(
                owner
                for owner, block in owner_blocks.items()
                if _workflow_mentions_target(block, target)
            )
        if not owners:
            uncovered.append(target)
        elif len(owners) > 1:
            uncovered.append(f"{target} (owned by {', '.join(sorted(owners))})")
    return uncovered


def ci_executed_targets(runner_source: str) -> list[str]:
    """The platform names the shared test harness believes required CI runs."""
    match = CI_EXECUTED_TARGETS_RE.search(runner_source)
    if not match:
        raise ValueError("CI_EXECUTED_TARGETS not found in the test-runner source")
    return re.findall(r'"([^"]+)"', match.group("body"))


RESULTS = load_script("ci-required-results.py", __file__)

# RUE-1265: the duplication gate models these three steps as scheduled work, so
# the filters have to be one fact. Importing them here means a filter added to
# the workflow without teaching the gate about it fails this validator, rather
# than quietly running a corpus slice the duplication comparison cannot see.
DUPLICATION = load_script("validate-test-duplication.py", __file__)

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


def narrowing_scope_registry(script: Path = AFFECTED_TARGETS_SCRIPT) -> dict[str, tuple[str, str]]:
    """Read the single registry that owns every impacted-lane scope."""
    result = subprocess.run(
        ["bash", str(script), "scope-registry"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        return {}
    registry: dict[str, tuple[str, str]] = {}
    for line in result.stdout.splitlines():
        fields = line.split("|")
        if len(fields) != 3 or not all(fields):
            return {}
        lane, kind, scope = fields
        if lane in registry:
            return {}
        registry[lane] = (kind, scope)
    return registry


def narrowing_contract_errors(
    workflow: str, script: Path = AFFECTED_TARGETS_SCRIPT
) -> list[str]:
    """Require every impacted-closure consumer to use a registered scope.

    The workflow may gate many jobs, but only jobs that consume the
    determinator's ``narrowed``/``impacted`` outputs are narrowing consumers.
    Their scope must be resolved by the registry-backed commands; direct
    ``build-scope``/``intersect`` calls are deliberately rejected because they
    can widen or drift without changing the registry.
    """
    registry = narrowing_scope_registry(script)
    if not registry:
        return ["narrowing scope registry is unavailable or malformed"]
    jobs = job_blocks(workflow)
    errors: list[str] = []
    expected_by_job = {
        "linux-premerge": {"linux-premerge-build", "linux-premerge-tests"},
        "native-platforms": {"native-platforms-units"},
    }
    expected_command_lines = {
        "linux-premerge-build": (
            'elif ! scope="$(scripts/affected-targets narrow-scope '
            'linux-premerge-build "$NARROW_FILE")"; then'
        ),
        "linux-premerge-tests": (
            'if scripts/affected-targets narrow-scope linux-premerge-tests '
            '"$file" >"$test_file"; then'
        ),
        "native-platforms-units": (
            'if ! narrowed="$(scripts/affected-targets narrow-scope '
            'native-platforms-units "$NARROW_FILE")"; then'
        ),
    }
    used_scopes: set[str] = set()
    for consumer, (kind, scope) in registry.items():
        if kind not in {"pattern", "graph"} or not scope.strip():
            errors.append(f"registered scope {consumer!r} has invalid metadata")

    for job, block in jobs.items():
        if job in {"affected-targets", "ci-success"}:
            continue
        executable = [line.split("#", 1)[0] for line in block.splitlines()]
        stripped_lines = [line.strip() for line in executable if line.strip()]
        executable_text = "\n".join(executable)
        consumes_impacted = "needs.affected-targets.outputs.impacted" in executable_text
        consumes_narrowed = "needs.affected-targets.outputs.narrowed" in executable_text
        if not (consumes_narrowed or consumes_impacted):
            continue
        expected_scopes = expected_by_job.get(job)
        if expected_scopes is None:
            errors.append(
                f"{job} consumes impacted narrowing but is absent from the scope registry"
            )
            continue
        if executable_text.count("needs.affected-targets.outputs.impacted") != 1:
            errors.append(f"{job} must materialize the impacted output exactly once")
        raw_file_ref = "${{ steps.narrow.outputs.file }}"
        if executable_text.count(raw_file_ref) != 1:
            errors.append(f"{job} must expose its raw impacted file to exactly one scope preparer")
        for line in executable:
            if raw_file_ref in line and f"NARROW_FILE: {raw_file_ref}" not in line:
                errors.append(
                    f"{job} has a raw impacted-file consumer outside a registered narrow-scope preparer"
                )
        expected_narrow_file_line = {
            "linux-premerge": expected_command_lines["linux-premerge-build"],
            "native-platforms": expected_command_lines["native-platforms-units"],
        }[job]
        narrow_file_uses = [line for line in stripped_lines if "$NARROW_FILE" in line]
        if narrow_file_uses != [expected_narrow_file_line]:
            errors.append(
                f"{job} must expose $NARROW_FILE only to its registered narrow-scope command"
            )
        allowed_local_file_uses = {
            "linux-premerge": {
                ': >"$file"',
                'printf \'%s\\n\' "$RUE_AFFECTED_IMPACTED" | sed \'/^$/d\' >"$file"',
                expected_command_lines["linux-premerge-tests"],
                'count="$(wc -l <"$file" | tr -d \' \')"',
                'echo "file=$file"',
            },
            "native-platforms": {
                ': >"$file"',
                'printf \'%s\\n\' "$RUE_AFFECTED_IMPACTED" | sed \'/^$/d\' >"$file"',
                'count="$(wc -l <"$file" | tr -d \' \')"',
                'echo "file=$file" >>"$GITHUB_OUTPUT"',
            },
        }[job]
        local_file_uses = [line for line in stripped_lines if "$file" in line]
        if len(local_file_uses) != len(allowed_local_file_uses) or set(
            local_file_uses
        ) != allowed_local_file_uses:
            errors.append(
                f"{job} must use its local raw impacted file only for "
                "materialization and registered scope preparation"
            )
        for line in executable:
            if "$RUE_AFFECTED_IMPACTED" not in line:
                continue
            if "printf '%s\\n' \"$RUE_AFFECTED_IMPACTED\"" not in line:
                errors.append(
                    f"{job} has a raw impacted-closure consumer outside its materialization step"
                )
        if any(
            "affected-targets build-scope" in line
            or "affected-targets intersect" in line
            for line in executable
        ):
            errors.append(
                f"{job} must use the registry-backed narrow-scope command, not a direct scope operation"
            )
        for consumer in expected_scopes:
            if consumer not in registry:
                errors.append(f"{job} requires missing registered scope {consumer!r}")
                continue
            required_line = expected_command_lines[consumer]
            if stripped_lines.count(required_line) != 1:
                errors.append(
                    f"{job} narrowing is not computed by registered scope {consumer!r}"
                )
            else:
                used_scopes.add(consumer)
        if job == "linux-premerge":
            if stripped_lines.count(
                'if scope="$(scripts/affected-targets scope-targets linux-premerge-build)"; then'
            ) != 1:
                errors.append(
                    "linux-premerge must declare its unnarrowed build scope through the registry"
                )
            if "RUE_TEST_TARGETS_FILE: ${{ steps.narrow.outputs.test_file }}" not in executable_text:
                errors.append(
                    "linux-premerge tests must consume the registry-intersected target file"
                )
            if "RUE_TEST_TARGETS_STATUS: ${{ steps.narrow.outputs.test_status }}" not in executable_text:
                errors.append(
                    "linux-premerge tests must consume the final scope-verification status"
                )
            if stripped_lines.count(
                'scripts/ci-timed "linux-x64 build" -- ./buck2 build //crates/...'
            ) != 2:
                errors.append("linux-premerge must retain its full-scope degraded fallback")
        elif job == "native-platforms":
            # native-targets is the compatibility spelling for the graph
            # allowlist; its implementation is registry-backed by the script.
            if stripped_lines.count(
                'native_targets="$(scripts/affected-targets native-targets)" || exit 1'
            ) != 1:
                errors.append(
                    "native-platforms must declare its graph-owned scope through native-targets"
                )
            if stripped_lines.count('narrowed="$native_targets"') != 1:
                errors.append("native-platforms must retain its full-scope degraded fallback")
        if "GITHUB_STEP_SUMMARY" not in executable_text or "saved share" not in executable_text:
            errors.append(f"{job} must publish final per-scope saved-share visibility")
    unused = set(registry) - used_scopes
    for consumer in sorted(unused):
        errors.append(f"registered scope {consumer!r} has no workflow narrow-scope consumer")
    return errors


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


PERFORMANCE_PINS_STEP = "Check the performance pins still match the tree"
VISIBLE_RUE_BENCH_BUILD = (
    'scripts/ci-timed "rue-bench build" -- ./buck2 build '
    "//crates/rue-bench:rue-bench"
)
WARM_RUE_BENCH_CAPTURE = (
    'BENCH="$(./buck2 build //crates/rue-bench:rue-bench --show-simple-output '
    '2>/dev/null | tail -1)"'
)
PERFORMANCE_PINS_COMMANDS = (
    "set -euo pipefail",
    'RUE="$(scripts/rue-bin)"',
    VISIBLE_RUE_BENCH_BUILD,
    WARM_RUE_BENCH_CAPTURE,
    '"$BENCH" check-pins \\',
    "  --manifest performance/manifest.toml \\",
    "  --repo-root . \\",
    '  --compiler "$RUE"',
)
PERFORMANCE_FAILURE_UPLOADER = (
    "        if: failure()",
    "        uses: actions/upload-artifact@v6",
    "        with:",
    "          name: premerge-linux-x64-failure-logs",
    "          path: ${{ runner.temp }}/rue-ci-failed-logs",
    "          if-no-files-found: ignore",
)


def performance_pin_step_errors(workflow: str) -> list[str]:
    """Keep the first rue-bench build visible and timed in the pin step.

    The warm path lookup intentionally remains a separate command: it names
    the binary after the visible build has completed. Restricting the check to
    the named step prevents a copy in another linux-premerge step (or a
    comment) from satisfying this failure-artifact and timing contract.
    """
    linux = job_blocks(workflow).get("linux-premerge", "")
    step_matches = list(
        re.finditer(
            rf"^      - name: {re.escape(PERFORMANCE_PINS_STEP)}\n"
            rf"(?P<body>.*?)(?=^      - |\Z)",
            linux,
            re.MULTILINE | re.DOTALL,
        )
    )
    if not step_matches:
        return [f"linux-premerge is missing the {PERFORMANCE_PINS_STEP!r} step"]
    if len(step_matches) != 1:
        return [
            f"linux-premerge must contain exactly one {PERFORMANCE_PINS_STEP!r} step"
        ]
    step_match = step_matches[0]

    job_if_lines = [
        line.strip()
        for line in linux.splitlines()
        if not line.lstrip().startswith("#")
        and re.match(r"^    (?:['\"]?if['\"]?)\s*:", line)
    ]
    if job_if_lines != ["if: ${{ always() }}"]:
        return [
            "linux-premerge must contain exactly one direct job if: "
            "if: ${{ always() }}"
        ]

    linux_has_job_policy_override = any(
        not line.lstrip().startswith("#")
        and (
            re.match(r"^    (?:['\"]?continue-on-error['\"]?)\s*:", line)
            or re.match(r"^    (?:['\"]?defaults['\"]?|<<)\s*:", line)
        )
        for line in linux.splitlines()
    )
    if linux_has_job_policy_override:
        return [
            "linux-premerge must not use job-level continue-on-error or "
            "defaults overrides for the performance-pin gate"
        ]

    step_body = step_match.group("body")
    run_match = re.search(r"^        run: \|[+-]?\s*$", step_body, re.MULTILINE)
    if not run_match:
        return [
            "the performance-pin step must contain a block-scalar shell run "
            "for its straight-line prefix"
        ]
    run_line_count = sum(
        bool(re.match(r"^        run: \|[+-]?\s*$", line))
        for line in step_body.splitlines()
    )
    unexpected_metadata = [
        line.strip()
        for line in step_body.splitlines()
        if line.strip()
        and not line.lstrip().startswith("#")
        and not re.match(r"^        run: \|[+-]?\s*$", line)
        and not line.startswith("          ")
    ]
    if run_line_count != 1 or unexpected_metadata:
        details = unexpected_metadata
        if run_line_count != 1:
            details = [f"run block occurs {run_line_count} times"] + details
        return [
            "the performance-pin step must not set disabling or custom "
            "execution metadata; only comments or blanks may precede or "
            "follow its run block: "
            + ", ".join(details)
        ]

    # Keep only the block-scalar shell body. The step's YAML metadata and later
    # steps are not executable lines in this named step.
    shell_lines = []
    run_body = step_body[run_match.end() :]
    if run_body.startswith("\n"):
        run_body = run_body[1:]
    for line in run_body.splitlines():
        if line.startswith("          "):
            shell_lines.append(line[10:])
        elif not line.strip():
            shell_lines.append("")
        else:
            break
    while shell_lines and shell_lines[-1] == "":
        shell_lines.pop()

    # Keep the de-indented physical shell lines intact. In particular, a `#`
    # adjacent to a command is shell data rather than a source comment, and a
    # blank/comment line inside this backslash chain must not disappear.
    executable_lines = shell_lines
    significant_lines = [line for line in executable_lines if line.strip()]
    visible_builds = [
        index
        for index, line in enumerate(significant_lines)
        if line == VISIBLE_RUE_BENCH_BUILD
    ]
    warm_captures = [
        index
        for index, line in enumerate(significant_lines)
        if line == WARM_RUE_BENCH_CAPTURE
    ]
    if len(visible_builds) != 1:
        errors = [
            "the performance-pin step must visibly run exactly one ci-timed "
            "rue-bench build targeting //crates/rue-bench:rue-bench"
        ]
        if not visible_builds:
            errors.append(
                "the performance-pin step has no executable ci-timed rue-bench build"
            )
        return errors
    if len(warm_captures) != 1:
        return [
            "the performance-pin step must retain exactly one warm rue-bench "
            "--show-simple-output path-only capture"
        ]
    if visible_builds[0] + 1 != warm_captures[0]:
        return [
            "the visible ci-timed rue-bench build must precede the warm "
            "path-only capture in the performance-pin step without an "
            "intervening command or control line"
        ]
    if tuple(executable_lines) != PERFORMANCE_PINS_COMMANDS:
        return [
            "the performance-pin step must keep the exact straight-line "
            "command sequence through check-pins and all expected arguments"
        ]

    uploader_matches = list(
        re.finditer(
            r"^      - name: Upload failing-suite output\n"
            r"(?P<body>.*?)(?=^      - |\Z)",
            linux,
            re.MULTILINE | re.DOTALL,
        )
    )
    if len(uploader_matches) != 1:
        return [
            "linux-premerge must contain exactly one failure-artifact uploader "
            "('Upload failing-suite output') step"
        ]
    uploader = uploader_matches[0]
    if uploader.start() <= step_match.start():
        return [
            "linux-premerge must upload failing-suite output after the "
            "performance-pin step"
        ]
    uploader_lines = [
        line
        for line in uploader.group("body").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if tuple(uploader_lines) != PERFORMANCE_FAILURE_UPLOADER:
        return [
            "linux-premerge failure-artifact uploader must retain its exact "
            "raw metadata mapping: expected "
            + repr(PERFORMANCE_FAILURE_UPLOADER)
            + "; got "
            + repr(tuple(uploader_lines))
        ]
    return []


def lane_target_drift(workflow: str, script: Path = AFFECTED_TARGETS_SCRIPT) -> list[str]:
    """Reject native target lists in YAML; membership belongs to the graph.

    The normal command-line entry point always performs the live graph checks
    in ``native_lane_ownership``. This structural half remains deterministic
    for source-only validator tests and prevents a workflow author from
    reintroducing a second membership source.
    """
    jobs = job_blocks(workflow)
    native = jobs.get("native-platforms", "")
    errors = []
    # The unit step may be renamed or moved. Inspect every executable line in
    # the job instead of anchoring to a display name, while ignoring comments
    # so explanatory target labels cannot become false positives.
    unit_invocation = './buck2 test "${targets[@]}"'
    native_query = 'native_targets="$(scripts/affected-targets native-targets)" || exit 1'
    intersect_query = 'scripts/affected-targets narrow-scope native-platforms-units "$NARROW_FILE"'
    native_query_count = 0
    test_invocations = []
    direct_targets = set()
    executable_targets = set()
    for line in native.splitlines():
        code = line.split("#", 1)[0]
        stripped = code.strip()
        is_native_query = stripped == native_query
        is_intersect_query = (
            stripped == f'narrowed="$({intersect_query})"'
            or stripped == f'if ! narrowed="$({intersect_query})"; then'
        )
        if is_native_query:
            native_query_count += 1
        if "scripts/affected-targets" in code:
            if not (is_native_query or is_intersect_query):
                errors.append(
                    "native-platforms may use scripts/affected-targets only for "
                    "the exact native-targets assignment or narrowing intersect"
                )
        buck_commands = re.finditer(
            r"(?<![A-Za-z0-9_.-])(?:\./)?buck2(?P<args>[^;&|]*)", code
        )
        if any(
            re.search(r"(?:^|\s)(?:query|uquery|targets)(?=\s|$)", match.group("args"))
            for match in buck_commands
        ):
            errors.append(
                "native-platforms must not run direct Buck graph queries; use "
                "the canonical affected-targets command"
            )
        executable_targets.update(
            re.findall(
                r"(?<![A-Za-z0-9_.-])//[A-Za-z0-9_./-]+:[A-Za-z0-9_.-]+(?![A-Za-z0-9_.-])",
                code,
            )
        )
        if re.search(r"(?:^|\s)(?:\./)?buck2\s+test(?:\s|$)", code):
            test_invocations.append(code.strip())
            if unit_invocation not in code:
                direct_targets.update(
                    re.findall(
                        r"(?<![A-Za-z0-9_.-])//[A-Za-z0-9_./-]+:[A-Za-z0-9_.-]+(?![A-Za-z0-9_.-])",
                        code,
                    )
                )
    if native_query_count != 1:
        errors.append(
            "native-platforms must derive unit membership exactly once with "
            "scripts/affected-targets native-targets"
        )
    if len(test_invocations) != 1 or unit_invocation not in test_invocations[0]:
        errors.append(
            "native-platforms must have exactly one graph-derived unit invocation: "
            './buck2 test "${targets[@]}"'
        )
    if direct_targets:
        errors.append(
            "native-platforms must not name Buck targets; graph labels own native membership: "
            + ", ".join(sorted(direct_targets))
        )
    # The compiler build is deliberately explicit and is not unit membership.
    # Every other executable target literal would be a peer source that could
    # be appended to the graph-derived array without appearing in lane-targets.
    unexpected_literals = executable_targets - {"//crates/rue:rue"}
    if unexpected_literals:
        errors.append(
            "native-platforms unit membership must come only from the graph; "
            "unexpected executable target literals: "
            + ", ".join(sorted(unexpected_literals))
        )
    return errors


NATIVE_CORPUS_PROXIES = {"//:spec-tests", "//:cli-tests"}


def native_graph_targets(script: Path = AFFECTED_TARGETS_SCRIPT) -> tuple[set[str], list[str]]:
    """Read the canonical live graph selection and report query failures."""
    result = subprocess.run(
        ["bash", str(script), "native-targets"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip()
        return set(), [
            "rue_platform_native graph query failed"
            + (f": {detail}" if detail else "")
        ]
    targets = {target.strip() for target in result.stdout.split() if target.strip()}
    if not targets:
        return set(), ["rue_platform_native graph selection is empty"]
    return targets, []


def native_lane_ownership(
    workflow: str,
    script: Path = AFFECTED_TARGETS_SCRIPT,
) -> list[str]:
    """Require both native lanes to equal the graph-owned native selection."""
    native_targets, errors = native_graph_targets(script)
    if errors:
        return errors
    expected = native_targets | NATIVE_CORPUS_PROXIES
    for lane in ("native-linux-arm64", "native-macos-arm64"):
        selected = lane_targets(lane, script)
        if not selected:
            errors.append(f"{lane} graph selection is empty or unavailable")
            continue
        missing = sorted(expected - selected)
        extras = sorted(selected - expected)
        if missing:
            errors.append(f"{lane} is missing graph-owned targets: {', '.join(missing)}")
        if extras:
            errors.append(f"{lane} selected unlabelled or unexpected targets: {', '.join(extras)}")
    errors.extend(lane_target_drift(workflow, script))
    return errors


def valgrind_install_errors(workflow: str, script: str) -> list[str]:
    """Keep Valgrind installation bounded and on its canonical script path."""
    block = job_blocks(workflow).get("valgrind", "")
    errors: list[str] = []
    if "run: scripts/install-valgrind\n" not in block:
        errors.append("valgrind must invoke scripts/install-valgrind")
    if re.search(r"(?:sudo\s+)?apt-get\s+(?:update|install)", block):
        errors.append("valgrind must not contain an inline unbounded apt-get operation")

    # Keep this exact policy in the CI contract: changing a bound, retry, or
    # lock option requires changing this reviewable contract and its tests.
    required = {
        "APT_OPERATION_TIMEOUT_SECONDS=600": "10-minute apt operation bound",
        "APT_KILL_AFTER_SECONDS=30": "30-second timeout kill grace period",
        "APT_ACQUIRE_TIMEOUT_SECONDS=30": "30-second per-acquisition timeout",
        "APT_RETRIES=2": "two apt-native retries",
        "APT_LOCK_TIMEOUT_SECONDS=60": "60-second package-manager lock wait",
        "--kill-after=\"${APT_KILL_AFTER_SECONDS}s\"": "timeout descendant kill bound",
        "--signal=TERM": "timeout termination signal",
        '"${APT_OPERATION_TIMEOUT_SECONDS}s"': "timeout operation deadline",
        "sudo -n apt-get": "non-interactive apt invocation",
        "DPkg::Lock::Timeout=${APT_LOCK_TIMEOUT_SECONDS}": "explicit apt/dpkg lock wait",
        "Acquire::Retries=${APT_RETRIES}": "apt-native retry policy",
        "Acquire::http::Timeout=${APT_ACQUIRE_TIMEOUT_SECONDS}": "HTTP acquisition bound",
        "Acquire::https::Timeout=${APT_ACQUIRE_TIMEOUT_SECONDS}": "HTTPS acquisition bound",
        'kill -TERM -- "-$child_pid"': "cancellation process-group cleanup",
        'kill -TERM "$child_pid"': "timeout process cleanup",
        'kill -KILL -- "-$child_pid"': "forced process-group cleanup",
        'kill -KILL "$child_pid"': "forced timeout cleanup",
        "trap on_signal INT TERM": "cancellation signal handling",
    }
    for text, description in required.items():
        if text not in script:
            errors.append(f"install-valgrind lost its {description}")
    return errors


def validate(
    ci_path: Path,
    native_runner_path: Path = NATIVE_RUNNER_SCRIPT,
    test_runner_path: Path = TEST_RUNNER_SOURCE,
    buck_path: Path = ROOT_BUCK,
    valgrind_install_path: Path = VALGRIND_INSTALL_SCRIPT,
) -> list[str]:
    workflow = ci_path.read_text()
    native_runner = native_runner_path.read_text()
    errors: list[str] = []
    if any(
        not line.lstrip().startswith("#")
        and re.match(r"^(?:['\"]?defaults['\"]?|<<)\s*:", line)
        for line in workflow.splitlines()
    ):
        errors.append(
            "workflow must not define top-level defaults or merge overrides "
            "that can replace the runner shell"
        )
    try:
        valgrind_install = valgrind_install_path.read_text()
    except OSError as error:
        errors.append(f"Valgrind installer unreadable: {error}")
        valgrind_install = ""
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
        errors.append("ci-contract no longer runs the live graph validator")
    validator_invocation = "scripts/validate-ci-gate.py .github/workflows/ci.yml"
    validator_position = contract.find(validator_invocation)
    if "facebook/install-dotslash@v2" not in contract:
        errors.append("ci-contract must install dotslash before the live Buck validator")
    elif validator_position < 0 or contract.index("facebook/install-dotslash@v2") > validator_position:
        errors.append("ci-contract must install dotslash before the live Buck validator")
    if "--structural-only" in contract:
        errors.append("ci-contract must run live graph ownership validation, not structural-only mode")
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
    errors.extend(performance_pin_step_errors(workflow))
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
        # A retired epoch's baseline is outside the derived data by design, so
        # the resolution check is a separate responsibility of this job rather
        # than a rule inside the stall script (RUE-1543).
        "check-baselines",
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
        "scripts/affected-targets native-targets",
        "scripts/run-native-platform-corpus.sh",
    ) + tuple(
        "scripts/rue cli " + " ".join(invocation)
        for invocation in DUPLICATION.NATIVE_CLI_INVOCATIONS
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
        "linux-x64-oracle-diff-o2",
        "linux-x64-oracle-diff-o3",
        "linux-x64-oracle-diff-spec",
        "linux-x64-oracle-diff-spec-o2",
        "linux-x64-oracle-diff-spec-o3",
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
        owners = {
            "release": jobs.get("release", ""),
        }
        for target in uncovered_dedicated_lanes(buck_source, corpus, owners):
            errors.append(
                f"{target} is marked {DEDICATED_LANE_LABEL} (so the premerge "
                "suite skips it) but has no exactly-one dedicated owner"
            )

    for sanitizer in ("valgrind", "asan"):
        block = jobs.get(sanitizer, "")
        if "runs-on: ubuntu-latest" not in block:
            errors.append(f"{sanitizer} is no longer consolidated into CI")
    if "inputs.large_program" not in jobs.get("valgrind", ""):
        errors.append("manual Valgrind large_program selection was not preserved")
    errors.extend(valgrind_install_errors(workflow, valgrind_install))

    errors.extend(undeclared_need_outputs(workflow, jobs))
    errors.extend(lane_target_drift(workflow))
    errors.extend(narrowing_contract_errors(workflow))

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
    parser.add_argument(
        "--structural-only",
        action="store_true",
        help=(
            "skip live Buck graph ownership; reserved for the Buck sh_test, "
            "whose nested Buck query would deadlock"
        ),
    )
    args = parser.parse_args()
    errors = validate(
        args.workflow, NATIVE_RUNNER_SCRIPT, args.test_runner_source, args.buck
    )
    if not args.structural_only:
        errors.extend(native_lane_ownership(args.workflow.read_text()))
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    print(
        (
            "CI gate valid: stable aggregate covers the exact required inventory"
            + (
                " and platform responsibilities"
                if not args.structural_only
                else "; structural-only Buck check defers live graph ownership"
            )
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
