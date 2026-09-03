#!/usr/bin/env python3
"""Validate the stable CI aggregate, job inventory, and platform ownership.

Every check here has an incident behind it, and each compares two facts that
must agree — the workflow against the graph, the harness, BUCK, or its own
aggregate — rather than pinning the text of a shell step:

* the `ci-success` aggregate needs every job, and `remote-execution` stays
  merge-group-only (RUE-1006, RUE-1507);
* every `needs.<job>.outputs.<name>` reference is declared, because GitHub
  resolves an undeclared one to the empty string, which a lane gate reads as
  "nothing selected" (RUE-1130);
* every gate step names a lane the determinator can select, and reads the
  output that carries that kind of selection (RUE-1130);
* every corpus BUCK marks `rue_ci_dedicated_lane` has exactly one owning job
  (RUE-1163);
* the harness's platform responsibility matrix names exactly the platforms
  required CI executes on (RUE-1161);
* native unit membership and clippy ownership come from the live graph and
  agree with what the lanes select (RUE-1266, RUE-1855).
"""

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
AFFECTED_TARGETS_SCRIPT = Path(__file__).with_name("affected-targets")

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
DERIVED_MATRIX = "fromJSON(needs.affected-targets.outputs.corpus_matrix)"
GATE_STEP_RE = re.compile(r'^\s+run: scripts/ci-corpus-selected (?P<unit>.+?)\s*$', re.MULTILINE)
CORPUS_UNIT = '"${{ matrix.target }}"'

# RUE-1265: the duplication gate models the native `scripts/rue cli` steps as
# scheduled work, so the filters have to be one fact. Importing them here means
# a filter added to the workflow without teaching the gate about it fails this
# validator, rather than quietly running a corpus slice the duplication
# comparison cannot see.
DUPLICATION = load_script("validate-test-duplication.py", __file__)

ACTION_ID = r"[A-Za-z_][A-Za-z0-9_-]*"


def affected_targets(args: tuple[str, ...], script: Path) -> list[str] | None:
    """One `scripts/affected-targets` query's tokens, or None when it failed."""
    result = subprocess.run(
        ["bash", str(script), *args], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        return None
    return [token for token in result.stdout.split() if token]


def dedicated_lane_targets(buck_source: str) -> list[str]:
    """Root-package corpora BUCK marks as owning their own required-CI job."""
    return [f"//:{match.group('name')}" for match in DEDICATED_LANE_RE.finditer(buck_source)]


def _workflow_mentions_target(block: str, target: str) -> bool:
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
    inventory=None,
) -> list[str]:
    """Labeled corpora that do not have exactly one dedicated owner.

    A corpus carrying the label is skipped by the linux-premerge suite, so if
    the platform-corpus matrix or an explicitly supplied owner job does not run
    it — directly or through its shards — it stops being covered at all.
    Conversely, two owners would reintroduce the same work under two lane
    contracts. Both are the RUE-924 false-green shape, reached by way of a
    label instead of a log line, so they fail the CI contract.

    The matrix is derived from the graph (RUE-1267), so its membership is the
    `inventory` the live run supplies from `scripts/affected-targets
    corpus-targets`. Without it — the structural Buck sh_test, which cannot
    query the graph — a derived matrix is taken to own what the live run will
    prove it owns, and only a literal matrix is judged from its text.
    """
    matrix_targets = set(re.findall(r"target: (\S+)", corpus_block)) | set(inventory or ())
    derived_without_inventory = DERIVED_MATRIX in corpus_block and inventory is None
    uncovered = []
    for target in dedicated_lane_targets(buck_source):
        sharded = any(candidate.startswith(f"{target}-shard-") for candidate in matrix_targets)
        owners = []
        if target in matrix_targets or sharded:
            owners.append("platform-corpus")
        if owner_blocks:
            owners.extend(
                owner
                for owner, block in owner_blocks.items()
                if _workflow_mentions_target(block, target)
            )
        if not owners and derived_without_inventory:
            continue
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


def gated_lane_errors(jobs: dict[str, str], script: Path = AFFECTED_TARGETS_SCRIPT) -> list[str]:
    """Every gate step names a selectable unit and reads its kind of selection.

    `scripts/ci-corpus-selected` writes run=false whenever the decision is
    selective and the unit is absent from the selected list. A lane name the
    determinator never emits, or a lane reading the corpus list instead of
    the lane list, is therefore deselected on EVERY selective run — the
    RUE-1130 shape, reached through the gate's argument rather than an
    output name.
    """
    lanes = affected_targets(("lanes",), script)
    if not lanes:
        return ["scripts/affected-targets lanes is unavailable or empty"]
    errors: list[str] = []
    gated: dict[str, list[str]] = {}
    for job, block in jobs.items():
        # A matrix job gates once per `name:` entry of its matrix.
        matrix_names = re.findall(r"^\s+(?:- )?name: ([A-Za-z0-9-]+)\s*$", block, re.MULTILINE)
        for match in GATE_STEP_RE.finditer(block):
            spelled = match.group("unit").strip('"')
            step = block[: match.start()].rsplit("      - name:", 1)[-1]
            if spelled == CORPUS_UNIT.strip('"'):
                units = [spelled]
            elif "${{ matrix.name }}" in spelled:
                units = [spelled.replace("${{ matrix.name }}", name) for name in matrix_names]
            else:
                units = [spelled]
            for unit in units:
                if unit == CORPUS_UNIT.strip('"'):
                    selection = "RUE_AFFECTED_TARGETS: ${{ needs.affected-targets.outputs.selected }}"
                elif unit in lanes:
                    selection = "RUE_AFFECTED_LANES: ${{ needs.affected-targets.outputs.selected_lanes }}"
                    gated.setdefault(unit, []).append(job)
                else:
                    errors.append(
                        f"{job} gates on {unit!r}, which scripts/affected-targets never selects; "
                        "it would be deselected on every selective run"
                    )
                    continue
                for required in (
                    "id: sel",
                    "RUE_AFFECTED_FULL: ${{ needs.affected-targets.outputs.full }}",
                    selection,
                ):
                    if required not in step:
                        errors.append(f"{job} gate step for {unit!r} lacks {required!r}")
    for lane in lanes:
        owners = gated.get(lane, [])
        if len(owners) != 1:
            errors.append(
                f"lane {lane!r} must be gated by exactly one job's ci-corpus-selected step; "
                f"found {', '.join(owners) or 'none'}"
            )
    return errors


def lane_target_drift(workflow: str) -> list[str]:
    """Reject native target lists in YAML; membership belongs to the graph.

    The live half is `native_lane_ownership`; this structural half keeps a
    workflow author from reintroducing a second membership source next to the
    graph-derived one (RUE-1266).
    """
    native = job_blocks(workflow).get("native-platforms", "")
    errors = []
    native_queries = 0
    literals = set()
    for line in native.splitlines():
        code = line.split("#", 1)[0]
        if "scripts/affected-targets native-targets" in code:
            native_queries += 1
        if re.search(r"(?<![A-Za-z0-9_.-])(?:\./)?buck2\s+(?:query|uquery|cquery|targets)(?=\s|$)", code):
            errors.append(
                "native-platforms must not run direct Buck graph queries; use "
                "the canonical affected-targets command"
            )
        literals.update(
            re.findall(r"(?<![A-Za-z0-9_.-])//[A-Za-z0-9_./-]+:[A-Za-z0-9_.-]+(?![A-Za-z0-9_.-])", code)
        )
    if native_queries != 1:
        errors.append(
            "native-platforms must derive unit membership exactly once with "
            "scripts/affected-targets native-targets"
        )
    # The compiler build is deliberately explicit and is not unit membership.
    unexpected = sorted(literals - {"//crates/rue:rue"})
    if unexpected:
        errors.append(
            "native-platforms must not name Buck targets; graph labels own native membership: "
            + ", ".join(unexpected)
        )
    return errors


NATIVE_CORPUS_PROXIES = {"//:spec-tests", "//:cli-tests"}


def native_lane_ownership(workflow: str, script: Path = AFFECTED_TARGETS_SCRIPT) -> list[str]:
    """Require both native lanes to equal the graph-owned native selection."""
    native_targets = affected_targets(("native-targets",), script)
    if native_targets is None:
        return ["rue_platform_native graph query failed"]
    if not native_targets:
        return ["rue_platform_native graph selection is empty"]
    expected = set(native_targets) | NATIVE_CORPUS_PROXIES
    errors = []
    for lane in ("native-linux-arm64", "native-macos-arm64"):
        selected = set(affected_targets(("lane-targets", lane), script) or ())
        if not selected:
            errors.append(f"{lane} graph selection is empty or unavailable")
            continue
        missing = sorted(expected - selected)
        extras = sorted(selected - expected)
        if missing:
            errors.append(f"{lane} is missing graph-owned targets: {', '.join(missing)}")
        if extras:
            errors.append(f"{lane} selected unlabelled or unexpected targets: {', '.join(extras)}")
    return errors


def clippy_lane_ownership(script: Path = AFFECTED_TARGETS_SCRIPT) -> list[str]:
    """Require selection, execution, and CI-owner labels to be exactly equal."""
    outputs: dict[str, set[str]] = {}
    errors: list[str] = []
    for command, args in (
        ("lane proxy", ("lane-targets", "clippy")),
        ("registered scope", ("scope-targets", "clippy")),
        ("owner label", ("clippy-owned-targets",)),
    ):
        targets = affected_targets(args, script)
        if targets is None:
            errors.append(f"clippy {command} query failed")
            continue
        if not targets:
            errors.append(f"clippy {command} is empty")
            continue
        if len(targets) != len(set(targets)):
            errors.append(f"clippy {command} contains duplicate targets")
        invalid = sorted(
            target
            for target in set(targets)
            if not (
                (target.startswith("//crates/") or target.startswith("//crates:"))
                and target.endswith("-clippy")
            )
        )
        if invalid:
            errors.append(
                f"clippy {command} contains targets outside the canonical live set: "
                + ", ".join(invalid)
            )
        outputs[command] = set(targets)
    if {"lane proxy", "registered scope"} <= set(outputs) and outputs[
        "lane proxy"
    ] != outputs["registered scope"]:
        errors.append("clippy lane proxy and registered runnable scope disagree")
    if {"registered scope", "owner label"} <= set(outputs):
        missing_labels = sorted(outputs["registered scope"] - outputs["owner label"])
        extra_labels = sorted(outputs["owner label"] - outputs["registered scope"])
        if missing_labels:
            errors.append(
                "canonical live clippy targets missing rue_ci_clippy_lane: "
                + ", ".join(missing_labels)
            )
        if extra_labels:
            errors.append(
                "rue_ci_clippy_lane labels targets outside the canonical live inventory: "
                + ", ".join(extra_labels)
            )
    return errors


def validate(
    ci_path: Path,
    native_runner_path: Path = NATIVE_RUNNER_SCRIPT,
    test_runner_path: Path = TEST_RUNNER_SOURCE,
    buck_path: Path = ROOT_BUCK,
    affected_targets_path: Path = AFFECTED_TARGETS_SCRIPT,
    corpus_inventory=None,
) -> list[str]:
    workflow = ci_path.read_text()
    native_runner = native_runner_path.read_text()
    errors: list[str] = []
    try:
        jobs = job_blocks(workflow)
    except ValueError as error:
        return [f"{ci_path}: {error}"]

    # The aggregate is the only branch-protection context, so it must need
    # every job: a job outside it can fail without blocking a merge.
    gate = jobs.get("ci-success", "")
    if "    name: CI success\n" not in gate:
        errors.append("ci-success must expose the stable displayed name 'CI success'")
    if "    if: ${{ always() }}\n" not in gate:
        errors.append("ci-success must use if: always()")
    actual_needs = list_needs(gate)
    expected_needs = set(jobs) - {"ci-success"}
    if not actual_needs:
        errors.append("ci-success needs no job; the aggregate would gate nothing")
    elif actual_needs != expected_needs:
        missing = sorted(expected_needs - actual_needs)
        extra = sorted(actual_needs - expected_needs)
        if missing:
            errors.append(f"CI job inventory has unaggregated jobs: {', '.join(missing)}")
        if extra:
            errors.append(f"ci-success needs jobs the workflow does not define: {', '.join(extra)}")
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
    validator_invocation = "scripts/validate-ci-gate.py .github/workflows/ci.yml"
    validator_position = contract.find(validator_invocation)
    if validator_position < 0:
        errors.append("ci-contract no longer runs the live graph validator")
    # RUE-1825: every job bootstraps through the one composite action that
    # owns the install and its cache; the toolchain must precede the validator.
    bootstrap = "uses: ./.github/actions/bootstrap-dotslash"
    if bootstrap not in contract or contract.index(bootstrap) > validator_position:
        errors.append("ci-contract must install dotslash before the live Buck validator")
    if "--structural-only" in contract:
        errors.append("ci-contract must run live graph ownership validation, not structural-only mode")
    if "scripts/validate-tier-ci-selectors.py" not in contract:
        errors.append("ci-contract no longer proves every test tier is CI-selected")
    else:
        if "--affected-targets scripts/affected-targets" not in contract:
            errors.append("ci-contract tier selector must receive the canonical affected-targets input")
        if "--live-graph" not in contract:
            errors.append("ci-contract tier selector must prove the derived matrix from the live graph")
    # RUE-1507: the scheduled-workflow health check is the only thing that reads
    # unattended run history on a path a human actually looks at, and it needs
    # `actions: read` as an executable line, not a mention in a comment.
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
        # RUE-1258: the pin gate; losing it restores the silence that let the
        # published series freeze for ten days while every job stayed green.
        "check-pins",
        # RUE-1265: the only check that compares test contents rather than
        # target lists; losing it hid 223s of re-executed work for weeks.
        "scripts/validate-test-duplication.py",
    ):
        if required not in linux:
            errors.append(f"linux-premerge responsibility missing {required!r}")
    if "scripts/validate-performance-stall.py" in linux:
        errors.append(
            "the staleness gate belongs to performance-staleness (RUE-1504); "
            "running it inside linux-premerge puts ~1m50s back on the critical path"
        )

    # RUE-1504: the staleness gate is required work that stopped being
    # premerge's work, so the contract follows it rather than relaxing.
    staleness = jobs.get("performance-staleness", "")
    for required in (
        "runs-on: ubuntu-latest",
        "scripts/validate-performance-stall.py",
        "//crates/rue-bench:rue-bench",
        "fetch-depth: 0",  # RUE-1258: it counts trunk commits
        "staleness-inputs",  # RUE-1542: the live epoch, not the whole store
        "check-baselines",  # RUE-1543
    ):
        if required not in staleness:
            errors.append(f"performance-staleness responsibility missing {required!r}")
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
    if "continue-on-error" in staleness:
        errors.append(
            "performance-staleness must not use continue-on-error; the gate has "
            "no bypass, and a required check that reports success regardless of "
            "the gate's result is one"
        )

    # RUE-1163: the label replaced the environment protocol; two sources drift.
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
            errors.append(f"native platform corpus runner no longer guarantees {required!r}")

    # RUE-1267: the platform-corpus matrix is derived by the shard planner from
    # the live graph; the wiring is the contract, not the rows.
    corpus = jobs.get("platform-corpus", "")
    affected = jobs.get("affected-targets", "")
    if f"matrix: ${{{{ {DERIVED_MATRIX} }}}}" not in corpus:
        errors.append("platform-corpus must take its matrix from the derived corpus_matrix output")
    for required in (
        "corpus_matrix: ${{ steps.cli-plan.outputs.corpus_matrix }}",
        "scripts/plan-cli-shards.py",
        "ci/cli-shard-planning.json",
        "attrfilter(labels, 'rue_cli_shard', //...)",
        "scripts/affected-targets corpus-targets",
    ):
        if required not in affected:
            errors.append(f"derived platform-corpus matrix missing planner contract {required!r}")
    if re.search(
        r"- name: Bootstrap dotslash for shard planning\n"
        r"(?:\s*#.*\n)*\s*if: github\.event_name != 'pull_request'\n"
        r"\s*uses: \./\.github/actions/bootstrap-dotslash",
        affected,
    ) is None:
        errors.append(
            "derived platform-corpus planner lacks Buck bootstrap for every "
            "non-pull-request event, including workflow_dispatch"
        )
    if re.search(
        r"- name: Bootstrap dotslash with BTD\n"
        r"\s*if: github\.event_name == 'pull_request'\n"
        r"\s*uses: \./\.github/actions/bootstrap-dotslash\n"
        r"\s*with:\n\s*with-btd: 'true'",
        affected,
    ) is None or affected.count("with-btd: 'true'") != 1:
        errors.append("affected-targets must have exactly one PR-only BTD bootstrap")

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
    # by exactly one owner: the platform-corpus matrix or the release job.
    try:
        buck_source = buck_path.read_text()
    except OSError as error:
        errors.append(f"root BUCK file unreadable: {error}")
    else:
        if not dedicated_lane_targets(buck_source):
            errors.append(
                "no corpus carries rue_ci_dedicated_lane; linux-premerge would "
                "re-run the corpora that have their own jobs"
            )
        owners = {"release": jobs.get("release", "")}
        for target in uncovered_dedicated_lanes(buck_source, corpus, owners, corpus_inventory):
            errors.append(
                f"{target} is marked {DEDICATED_LANE_LABEL} (so the premerge "
                "suite skips it) but has no exactly-one dedicated owner"
            )

    for sanitizer in ("valgrind", "asan"):
        if "runs-on: ubuntu-latest" not in jobs.get(sanitizer, ""):
            errors.append(f"{sanitizer} is no longer consolidated into CI")
    valgrind = jobs.get("valgrind", "")
    if "inputs.large_program" not in valgrind:
        errors.append("manual Valgrind large_program selection was not preserved")
    # The installer bounds apt; an inline apt-get is the unbounded hang it exists to prevent.
    if "run: scripts/install-valgrind\n" not in valgrind:
        errors.append("valgrind must invoke scripts/install-valgrind")
    if re.search(r"(?:sudo\s+)?apt-get\s+(?:update|install)", valgrind):
        errors.append("valgrind must not contain an inline unbounded apt-get operation")

    errors.extend(undeclared_need_outputs(workflow, jobs))
    errors.extend(gated_lane_errors(jobs, affected_targets_path))
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
    parser.add_argument(
        "--structural-only",
        action="store_true",
        help=(
            "skip live Buck graph ownership; reserved for the Buck sh_test, "
            "whose nested Buck query would deadlock"
        ),
    )
    args = parser.parse_args()
    inventory = None
    errors: list[str] = []
    if not args.structural_only:
        inventory = affected_targets(("corpus-targets",), AFFECTED_TARGETS_SCRIPT)
        if not inventory:
            errors.append("scripts/affected-targets corpus-targets is unavailable or empty")
            inventory = []
    errors.extend(
        validate(
            args.workflow,
            NATIVE_RUNNER_SCRIPT,
            args.test_runner_source,
            args.buck,
            corpus_inventory=inventory,
        )
    )
    if not args.structural_only:
        errors.extend(native_lane_ownership(args.workflow.read_text()))
        errors.extend(clippy_lane_ownership())
    if errors:
        for error in errors:
            print(f"error: {error}")
        return 1
    print(
        "CI gate valid: stable aggregate covers the exact required inventory"
        + (
            " and platform responsibilities"
            if not args.structural_only
            else "; structural-only Buck check defers live graph ownership"
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
