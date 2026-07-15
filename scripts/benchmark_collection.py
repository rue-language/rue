"""Strict parsing and aggregation for benchmark compiler iterations."""

import json
import math


def _non_negative_number(value: object, description: str) -> float:
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or value < 0
        or (isinstance(value, float) and not math.isfinite(value))
    ):
        raise ValueError(f"{description} must be a finite non-negative number")
    return float(value)


def parse_iteration_json(text: str) -> dict:
    """Parse one compiler timing payload, rejecting malformed timing/pass data."""
    data = json.loads(text)
    if not isinstance(data, dict):
        raise ValueError("benchmark timing JSON must be an object")

    schema_version = data.get("schema_version", 1)
    if (
        not isinstance(schema_version, int)
        or isinstance(schema_version, bool)
        or schema_version not in (1, 2)
    ):
        raise ValueError("schema_version must be integer 1 or 2")
    _non_negative_number(data.get("total_ms"), "total_ms")
    passes = data.get("passes")
    if not isinstance(passes, list):
        raise ValueError("passes must be an array")
    for index, row in enumerate(passes):
        if not isinstance(row, dict):
            raise ValueError(f"pass #{index + 1} must be an object")
        name = row.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError(f"pass #{index + 1} has no valid name")
        _non_negative_number(row.get("duration_ms"), f"pass '{name}' duration_ms")
        if schema_version == 2:
            for field in ("invocations", "root_invocations", "leaf_invocations"):
                value = row.get(field)
                if (
                    not isinstance(value, int)
                    or isinstance(value, bool)
                    or value < 0
                ):
                    raise ValueError(
                        f"pass '{name}' {field} must be a non-negative integer"
                    )
            if row["invocations"] <= 0:
                raise ValueError(f"pass '{name}' invocations must be greater than zero")
            for field in ("root_invocations", "leaf_invocations"):
                if row[field] > row["invocations"]:
                    raise ValueError(
                        f"pass '{name}' {field} exceeds invocations"
                    )
    names = [row["name"] for row in passes]
    if len(names) != len(set(names)):
        raise ValueError("passes must not contain duplicate names")
    if "source_metrics" in data:
        metrics = data["source_metrics"]
        if not isinstance(metrics, dict):
            raise ValueError("source_metrics must be an object")
        for field in ("bytes", "files", "lines", "tokens"):
            value = metrics.get(field)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise ValueError(f"source_metrics {field} must be a non-negative integer")
    return data


def aggregate_iterations(payloads: list[str]) -> dict:
    """Aggregate validated compiler timing payloads without dropping rows."""
    if not payloads:
        raise ValueError("no benchmark iterations supplied")

    pass_data: dict[str, list[float]] = {}
    source_metrics = None
    source_metrics_present = None
    peak_memory_samples = []
    timing_schema_version = None
    expected_included_passes = None
    pass_structure = None

    for text in payloads:
        data = parse_iteration_json(text)
        present = "source_metrics" in data
        if source_metrics_present is None:
            source_metrics_present = present
        elif present != source_metrics_present:
            raise ValueError("source_metrics presence changed between iterations")
        schema_version = data.get("schema_version", 1)
        if timing_schema_version is None:
            timing_schema_version = schema_version
        elif schema_version != timing_schema_version:
            raise ValueError("timing schema_version changed between iterations")

        included_passes = set()
        current_structure = {}
        for row in data["passes"]:
            if schema_version >= 2 and row.get("leaf_invocations") != row.get(
                "invocations"
            ):
                continue
            included_passes.add(row["name"])
            pass_data.setdefault(row["name"], []).append(float(row["duration_ms"]))
            if schema_version >= 2:
                current_structure[row["name"]] = {
                    "invocations": row["invocations"],
                    "root_invocations": row["root_invocations"],
                    "leaf_invocations": row["leaf_invocations"],
                }
        if expected_included_passes is None:
            expected_included_passes = included_passes
        elif included_passes != expected_included_passes:
            raise ValueError("aggregated pass set changed between iterations")
        if pass_structure is None:
            pass_structure = current_structure
        elif current_structure != pass_structure:
            raise ValueError("pass invocation structure changed between iterations")
        if "source_metrics" in data:
            if source_metrics is None:
                source_metrics = data["source_metrics"]
            elif source_metrics != data["source_metrics"]:
                raise ValueError("source_metrics changed between iterations")
        if "peak_memory_bytes" in data and data["peak_memory_bytes"]:
            peak_memory_samples.append(
                _non_negative_number(data["peak_memory_bytes"], "peak_memory_bytes")
            )

    passes = {}
    for name, durations in pass_data.items():
        passes[name] = {"mean_ms": round(sum(durations) / len(durations), 3)}
        if pass_structure and name in pass_structure:
            passes[name].update(pass_structure[name])
    result = {"passes": passes, "timing_schema_version": timing_schema_version}
    if source_metrics is not None:
        result["source_metrics"] = source_metrics
    if peak_memory_samples:
        result["peak_memory_bytes"] = int(
            sum(peak_memory_samples) / len(peak_memory_samples)
        )
    return result
