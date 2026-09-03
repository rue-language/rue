#!/usr/bin/env python3
"""Derive and validate the required-CI CLI shard topology (RUE-1267).

The checked-in input is measured wall time.  The planner minimizes runner
count while keeping the conservative, skew-adjusted CLI projection below the
slowest measured lane that cannot be split.  It deliberately refuses to
produce a count when an indivisible item already exceeds that floor.

The workflow matrix is emitted from the live ``rue_cli_shard`` Buck label set.
That makes the planned-lane union equal to the graph set by construction; a
gap, duplicate, stale BUCK count, or unsupported target fails closed here.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime
from pathlib import Path
from typing import Optional

SHARD_RE = re.compile(r"^//:cli-tests-shard-(0|[1-9][0-9]*)$")


def positive_int(value: object, where: str) -> int:
    if type(value) is not int or value <= 0:
        raise ValueError(f"{where} must be a positive integer")
    return value


def nonempty_string(value: object, where: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{where} must be a non-empty string")
    return value


def source_run_ids(value: object, where: str) -> list[int]:
    if (
        not isinstance(value, list)
        or not value
        or any(type(run_id) is not int or run_id <= 0 for run_id in value)
        or len(value) != len(set(value))
    ):
        raise ValueError(f"{where} must be a non-empty list of unique run IDs")
    return value


def ceil_div(numerator: int, denominator: int) -> int:
    return (numerator + denominator - 1) // denominator


def utc_timestamp(value: object, where: str) -> datetime:
    nonempty_string(value, where)
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError as error:
        raise ValueError(f"{where} must be a UTC Actions timestamp") from error


def load_measurements(path: Path) -> dict:
    data = json.loads(path.read_text())
    if data.get("version") != 1:
        raise ValueError(f"{path}: unsupported planning-data version")
    if re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", str(data.get("measured_at"))) is None:
        raise ValueError(f"{path}: measured_at must be an ISO date")
    provenance = data.get("provenance")
    if not isinstance(provenance, dict):
        raise ValueError(f"{path}: provenance must be an object")
    for field in ("source", "acquisition", "refresh"):
        nonempty_string(provenance.get(field), f"{path}: provenance.{field}")
    floor = data.get("floor")
    cli = data.get("cli")
    if not isinstance(floor, dict) or not isinstance(cli, dict):
        raise ValueError(f"{path}: floor and cli must be objects")
    nonempty_string(floor.get("name"), f"{path}: floor.name")
    floor_range = floor.get("observed_wall_range_ms")
    cli_range = cli.get("unsharded_wall_range_ms")
    if not isinstance(floor_range, list) or len(floor_range) != 2:
        raise ValueError(f"{path}: floor.observed_wall_range_ms must have two entries")
    if not isinstance(cli_range, list) or len(cli_range) != 2:
        raise ValueError(f"{path}: cli.unsharded_wall_range_ms must have two entries")
    for index, value in enumerate(floor_range):
        positive_int(value, f"{path}: floor.observed_wall_range_ms[{index}]")
    for index, value in enumerate(cli_range):
        positive_int(value, f"{path}: cli.unsharded_wall_range_ms[{index}]")
    if floor_range[0] > floor_range[1] or cli_range[0] > cli_range[1]:
        raise ValueError(f"{path}: observed wall ranges must be ordered low to high")
    floor_ms = positive_int(
        floor.get("planning_floor_ms"), f"{path}: floor.planning_floor_ms"
    )
    total_ms = positive_int(
        cli.get("planning_total_ms"), f"{path}: cli.planning_total_ms"
    )
    if floor_ms != max(floor_range):
        raise ValueError(f"{path}: planning floor must equal the observed range maximum")
    if total_ms != max(cli_range):
        raise ValueError(f"{path}: planning total must equal the observed range maximum")
    source_run_ids(floor.get("source_run_ids"), f"{path}: floor.source_run_ids")
    cli_run_ids = source_run_ids(
        cli.get("source_run_ids"), f"{path}: cli.source_run_ids"
    )
    positive_int(
        cli.get("runner_count_skew_allowance_basis_points"),
        f"{path}: cli.runner_count_skew_allowance_basis_points",
    )
    positive_int(
        cli.get("observed_balance_budget_basis_points"),
        f"{path}: cli.observed_balance_budget_basis_points",
    )
    items = cli.get("indivisible_items")
    if not isinstance(items, list) or not items:
        raise ValueError(f"{path}: cli.indivisible_items must be a non-empty list")
    for index, item in enumerate(items):
        if not isinstance(item, dict):
            raise ValueError(f"{path}: cli.indivisible_items[{index}] must be an object")
        if not isinstance(item.get("name"), str) or not item["name"].strip():
            raise ValueError(f"{path}: cli.indivisible_items[{index}] needs a name")
        positive_int(
            item.get("observed_wall_ms"),
            f"{path}: cli.indivisible_items[{index}].observed_wall_ms",
        )
        nonempty_string(
            item.get("source_metric"),
            f"{path}: cli.indivisible_items[{index}].source_metric",
        )
        item_run_ids = source_run_ids(
            item.get("source_run_ids"),
            f"{path}: cli.indivisible_items[{index}].source_run_ids",
        )
        if not set(item_run_ids).issubset(cli_run_ids):
            raise ValueError(
                f"{path}: cli.indivisible_items[{index}] cites a run outside "
                "cli.source_run_ids"
            )
    inventory = cli.get("indivisible_inventory")
    if not isinstance(inventory, dict):
        raise ValueError(f"{path}: cli.indivisible_inventory must be an object")
    for field in ("scope", "acquisition", "completeness"):
        nonempty_string(
            inventory.get(field), f"{path}: cli.indivisible_inventory.{field}"
        )
    if inventory.get("completeness") != "manual-reviewed; not graph-derived":
        raise ValueError(
            f"{path}: cli.indivisible_inventory.completeness must explicitly "
            "record the manual, non-graph-derived boundary"
        )
    remeasurement = data.get("phase_6_remeasurement")
    if not isinstance(remeasurement, dict):
        raise ValueError(f"{path}: phase_6_remeasurement must be an object")
    pre_change = remeasurement.get("pre_change")
    if not isinstance(pre_change, dict):
        raise ValueError(f"{path}: phase_6_remeasurement.pre_change must be an object")
    if pre_change.get("event") not in {"pull_request", "merge_group"}:
        raise ValueError(f"{path}: Phase 6 pre-change event is invalid")
    positive_int(pre_change.get("run_id"), f"{path}: Phase 6 pre-change run_id")
    if re.fullmatch(r"[0-9a-f]{40}", str(pre_change.get("head_sha"))) is None:
        raise ValueError(f"{path}: Phase 6 pre-change head_sha must be a full SHA")
    created_at = utc_timestamp(
        pre_change.get("created_at"), f"{path}: Phase 6 pre-change created_at"
    )
    completed_at = utc_timestamp(
        pre_change.get("completed_at"), f"{path}: Phase 6 pre-change completed_at"
    )
    url = nonempty_string(
        pre_change.get("url"), f"{path}: Phase 6 pre-change url"
    )
    run_id = pre_change["run_id"]
    if not url.endswith(f"/actions/runs/{run_id}"):
        raise ValueError(f"{path}: Phase 6 pre-change URL does not name run_id")
    workflow_wall_ms = positive_int(
        pre_change.get("workflow_wall_ms"),
        f"{path}: Phase 6 pre-change workflow_wall_ms",
    )
    if int((completed_at - created_at).total_seconds() * 1000) != workflow_wall_ms:
        raise ValueError(f"{path}: Phase 6 pre-change workflow wall is inconsistent")
    binding_job = pre_change.get("binding_job")
    if not isinstance(binding_job, dict):
        raise ValueError(f"{path}: Phase 6 pre-change binding_job must be an object")
    nonempty_string(
        binding_job.get("name"), f"{path}: Phase 6 pre-change binding job name"
    )
    positive_int(
        binding_job.get("job_id"), f"{path}: Phase 6 pre-change binding job_id"
    )
    job_started_at = utc_timestamp(
        binding_job.get("started_at"), f"{path}: Phase 6 binding job started_at"
    )
    job_completed_at = utc_timestamp(
        binding_job.get("completed_at"), f"{path}: Phase 6 binding job completed_at"
    )
    job_wall_ms = positive_int(
        binding_job.get("wall_ms"), f"{path}: Phase 6 binding job wall_ms"
    )
    if int((job_completed_at - job_started_at).total_seconds() * 1000) != job_wall_ms:
        raise ValueError(f"{path}: Phase 6 binding job wall is inconsistent")
    post_change = remeasurement.get("post_change")
    if not isinstance(post_change, dict):
        raise ValueError(f"{path}: phase_6_remeasurement.post_change must be an object")
    status = post_change.get("status")
    if status not in {"pending_pr_ci", "recorded"}:
        raise ValueError(f"{path}: phase_6_remeasurement.post_change.status is invalid")
    run_ids = post_change.get("pull_request_run_ids")
    walls = post_change.get("observed_critical_path_ms")
    if not isinstance(run_ids, list) or any(
        type(run_id) is not int or run_id <= 0 for run_id in run_ids
    ) or len(run_ids) != len(set(run_ids)):
        raise ValueError(
            f"{path}: Phase 6 post-change pull_request_run_ids must contain run IDs"
        )
    if not isinstance(walls, list) or any(
        type(wall) is not int or wall <= 0 for wall in walls
    ):
        raise ValueError(
            f"{path}: Phase 6 post-change observed_critical_path_ms must contain positive walls"
        )
    if len(run_ids) != len(walls):
        raise ValueError(
            f"{path}: phase_6_remeasurement run IDs and walls must have equal length"
        )
    if status == "pending_pr_ci" and (run_ids or walls):
        raise ValueError(f"{path}: pending Phase 6 remeasurement must be empty")
    if status == "recorded" and not run_ids:
        raise ValueError(f"{path}: recorded Phase 6 remeasurement must name runs")
    nonempty_string(
        remeasurement.get("comparison"),
        f"{path}: phase_6_remeasurement.comparison",
    )
    return data


def derive_runner_count(data: dict) -> tuple[int, int, int, int]:
    floor_ms = data["floor"]["planning_floor_ms"]
    total_ms = data["cli"]["planning_total_ms"]
    allowance = data["cli"]["runner_count_skew_allowance_basis_points"]
    overweight = [
        item
        for item in data["cli"]["indivisible_items"]
        if item["observed_wall_ms"] > floor_ms
    ]
    if overweight:
        details = ", ".join(
            f"{item['name']} ({item['observed_wall_ms']}ms)" for item in overweight
        )
        raise ValueError(
            f"indivisible item exceeds the {floor_ms}ms floor: {details}; "
            "runner count is undefined"
        )
    for count in range(1, 65):
        # Apply one ceiling to the complete rational projection. Rounding the
        # even lane before applying skew can overstate a boundary by 1ms and
        # select a non-minimal runner count.
        projected_ms = ceil_div(
            total_ms * (10000 + allowance), count * 10000
        )
        if projected_ms <= floor_ms:
            return count, floor_ms, total_ms, projected_ms
    raise ValueError("more than 64 CLI runners would be required")


def normalize_target(line: str) -> str:
    target = re.sub(r" \([^()]*\)$", "", line.strip())
    return target.removeprefix("root")


def load_graph_shards(path: Path) -> list[str]:
    targets = [normalize_target(line) for line in path.read_text().splitlines() if line.strip()]
    if not targets:
        raise ValueError(f"{path}: live rue_cli_shard graph selection is empty")
    if len(targets) != len(set(targets)):
        raise ValueError(f"{path}: live rue_cli_shard graph selection contains duplicates")
    invalid = sorted(target for target in targets if SHARD_RE.fullmatch(target) is None)
    if invalid:
        raise ValueError("unsupported rue_cli_shard target(s): " + ", ".join(invalid))
    return sorted(targets, key=lambda target: int(SHARD_RE.fullmatch(target).group(1)))


def load_corpus_targets(path: Path) -> list[str]:
    targets = [normalize_target(line) for line in path.read_text().splitlines() if line.strip()]
    if not targets:
        raise ValueError(f"{path}: canonical platform-corpus inventory is empty")
    if len(targets) != len(set(targets)):
        raise ValueError(f"{path}: canonical platform-corpus inventory contains duplicates")
    return targets


def validate_graph_union(targets: list[str], count: int) -> None:
    expected = [f"//:cli-tests-shard-{index}" for index in range(count)]
    if targets != expected:
        missing = sorted(set(expected) - set(targets))
        extra = sorted(set(targets) - set(expected))
        details = []
        if missing:
            details.append("missing from graph: " + ", ".join(missing))
        if extra:
            details.append("not in derived plan: " + ", ".join(extra))
        raise ValueError(
            "planned CLI lane union does not equal the live Buck graph ("
            + "; ".join(details)
            + ")"
        )


def validate_corpus_union(corpus_targets: list[str], graph_shards: list[str]) -> None:
    planned_shards = [target for target in corpus_targets if SHARD_RE.fullmatch(target)]
    if set(planned_shards) != set(graph_shards):
        missing = sorted(set(graph_shards) - set(planned_shards))
        extra = sorted(set(planned_shards) - set(graph_shards))
        detail = []
        if missing:
            detail.append("graph shards missing from corpus inventory: " + ", ".join(missing))
        if extra:
            detail.append("corpus shards absent from graph: " + ", ".join(extra))
        raise ValueError("platform-corpus inventory does not equal live graph union (" + "; ".join(detail) + ")")


def corpus_name(target: str) -> str:
    if SHARD_RE.fullmatch(target):
        return "cli-" + target.removeprefix("//:cli-tests-")
    if target == "//:spec-tests":
        return "spec"
    prefix = "//crates/rue-oracle-diff:oracle-diff-"
    if target.startswith(prefix):
        suffix = target.removeprefix(prefix).replace("test-", "").replace("test", "").strip("-")
        return "oracle-diff" + ("-" + suffix if suffix else "")
    raise ValueError(f"no platform-corpus naming rule for {target}")


def matrix(targets: list[str]) -> dict:
    rows = []
    for target in targets:
        name = corpus_name(target)
        rows.append(
            {
                "os": "ubuntu-latest",
                "cache_name": "linux-x64",
                "target": target,
                "name": name,
                "check_name": "linux-x64-" + name,
            }
        )
    return {"include": rows}


def observed_skew_basis_points(walls: list[int]) -> int:
    if not walls or any(type(wall) is not int or wall <= 0 for wall in walls):
        raise ValueError("observed lane walls must be positive integers")
    total = sum(walls)
    # ceil((max / mean - 1) * 10000), without floating point.
    numerator = max(walls) * len(walls) - total
    return max(0, ceil_div(numerator * 10000, total))


def format_basis_points(value: int) -> str:
    return f"{value // 100}.{value % 100:02d}%"


def check_observed(data: dict, observations: dict) -> list[str]:
    budget = data["cli"]["observed_balance_budget_basis_points"]
    expected_count = derive_runner_count(data)[0]
    errors = []
    samples = observations.get("samples")
    if not isinstance(samples, list) or not samples:
        raise ValueError("observations.samples must be a non-empty list")
    for sample in samples:
        if not isinstance(sample, dict) or not isinstance(sample.get("name"), str):
            raise ValueError("every observed sample needs a name")
        walls = sample.get("lane_wall_ms")
        if not isinstance(walls, list):
            raise ValueError(f"{sample['name']}: lane_wall_ms must be a list")
        if len(walls) != expected_count:
            raise ValueError(
                f"{sample['name']}: expected {expected_count} observed lane walls, "
                f"got {len(walls)}"
            )
        skew = observed_skew_basis_points(walls)
        if skew > budget:
            errors.append(
                f"{sample['name']}: observed lane-wall skew "
                f"{format_basis_points(skew)} exceeds "
                f"{format_basis_points(budget)} budget; walls={walls}"
            )
    return errors


def load_repetition_observations(root: Path, count: int) -> dict:
    by_iteration: dict[int, dict[int, int]] = {}
    files = sorted(root.rglob("results.tsv"))
    if not files:
        raise ValueError(f"{root}: no correctness-repetition results.tsv files")
    for path in files:
        for line_number, line in enumerate(path.read_text().splitlines(), 1):
            fields = line.split("\t")
            if len(fields) != 5:
                raise ValueError(f"{path}:{line_number}: expected five tab-separated fields")
            target, iteration_text, result, elapsed_text, _log = fields
            match = SHARD_RE.fullmatch(target)
            if match is None:
                raise ValueError(f"{path}:{line_number}: invalid shard target {target!r}")
            try:
                iteration = int(iteration_text)
                elapsed_ms = int(elapsed_text) * 1000
            except ValueError as error:
                raise ValueError(f"{path}:{line_number}: invalid numeric field") from error
            if iteration <= 0 or elapsed_ms <= 0:
                raise ValueError(f"{path}:{line_number}: durations and iterations must be positive")
            if result != "PASS":
                continue
            shard = int(match.group(1))
            lanes = by_iteration.setdefault(iteration, {})
            if shard in lanes:
                raise ValueError(f"iteration {iteration}: duplicate observation for shard {shard}")
            lanes[shard] = elapsed_ms
    samples = []
    for iteration, lanes in sorted(by_iteration.items()):
        missing = sorted(set(range(count)) - set(lanes))
        extra = sorted(set(lanes) - set(range(count)))
        if missing or extra:
            detail = []
            if missing:
                detail.append("missing shards " + ", ".join(map(str, missing)))
            if extra:
                detail.append("unexpected shards " + ", ".join(map(str, extra)))
            raise ValueError(f"iteration {iteration}: " + "; ".join(detail))
        samples.append(
            {
                "name": f"correctness repetition {iteration}",
                "lane_wall_ms": [lanes[index] for index in range(count)],
            }
        )
    if not samples:
        raise ValueError(f"{root}: no complete passing repetition samples")
    return {"samples": samples}


def emit_outputs(plan: Optional[dict], count: int) -> None:
    encoded = json.dumps(plan, separators=(",", ":")) if plan is not None else None
    shards = json.dumps(list(range(count)), separators=(",", ":"))
    if encoded is not None:
        print(encoded)
    output_path = __import__("os").environ.get("GITHUB_OUTPUT")
    if output_path:
        with Path(output_path).open("a") as handle:
            if encoded is not None:
                handle.write(f"corpus_matrix={encoded}\n")
            handle.write(f"cli_shards={shards}\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--measurements", type=Path, required=True)
    parser.add_argument("--graph-shards", type=Path)
    parser.add_argument("--corpus-targets", type=Path)
    parser.add_argument("--shards-only", action="store_true")
    parser.add_argument("--observations", type=Path)
    parser.add_argument("--observations-root", type=Path)
    parser.add_argument("--derive-only", action="store_true")
    args = parser.parse_args()
    try:
        data = load_measurements(args.measurements)
        count, floor_ms, total_ms, projected_ms = derive_runner_count(data)
        if args.observations is not None and args.observations_root is not None:
            raise ValueError("choose only one observations input")
        if args.observations is not None or args.observations_root is not None:
            observations = (
                json.loads(args.observations.read_text())
                if args.observations is not None
                else load_repetition_observations(args.observations_root, count)
            )
            errors = check_observed(data, observations)
            if errors:
                for error in errors:
                    print(f"error: {error}", file=sys.stderr)
                return 1
            print("observed CLI lane-wall balance is within budget")
            return 0
        print(
            f"derived {count} CLI runners: max measured total {total_ms}ms, "
            f"skew-adjusted lane {projected_ms}ms <= {floor_ms}ms floor",
            file=sys.stderr,
        )
        if args.derive_only:
            print(count)
            return 0
        if args.graph_shards is None:
            raise ValueError("--graph-shards is required unless --derive-only is used")
        targets = load_graph_shards(args.graph_shards)
        validate_graph_union(targets, count)
        if args.shards_only:
            emit_outputs(None, count)
            return 0
        if args.corpus_targets is None:
            raise ValueError("--corpus-targets is required when emitting the corpus matrix")
        corpus_targets = load_corpus_targets(args.corpus_targets)
        validate_corpus_union(corpus_targets, targets)
        emit_outputs(matrix(corpus_targets), count)
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
