"""Shared schema and corpus validation for benchmark result publication."""

from collections import Counter
import math
from pathlib import Path
import tomllib


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO_ROOT / "benchmarks" / "manifest.toml"


def _is_finite_non_negative_number(value: object) -> bool:
    return (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and value >= 0
        and (isinstance(value, int) or math.isfinite(value))
    )


def load_manifest_names(path: Path) -> list[str]:
    """Load and validate the benchmark names declared by the manifest."""
    with path.open("rb") as f:
        manifest = tomllib.load(f)

    entries = manifest.get("benchmark")
    if not isinstance(entries, list) or not entries:
        raise ValueError("manifest must contain at least one [[benchmark]] entry")

    names = []
    for index, entry in enumerate(entries):
        name = entry.get("name") if isinstance(entry, dict) else None
        if not isinstance(name, str) or not name:
            raise ValueError(f"manifest benchmark #{index + 1} has no valid name")
        names.append(name)

    duplicates = sorted(name for name, count in Counter(names).items() if count > 1)
    if duplicates:
        raise ValueError(
            "manifest contains duplicate benchmark name(s): " + ", ".join(duplicates)
        )

    return names


def validate_results(data: object, expected_names: list[str]) -> list[str]:
    """Return every schema or corpus-membership error in a benchmark result."""
    if not isinstance(data, dict):
        return ["benchmark result must be a JSON object"]

    iterations = data.get("iterations")
    if (
        not isinstance(iterations, int)
        or isinstance(iterations, bool)
        or iterations <= 0
    ):
        errors = ["iterations must be a positive integer"]
    else:
        errors = []

    benchmarks = data.get("benchmarks")
    if not isinstance(benchmarks, list):
        return errors + ["benchmarks must be an array"]
    if not benchmarks:
        return errors + ["no benchmark results collected; all benchmarks failed"]

    result_names = []
    for index, bench in enumerate(benchmarks):
        if not isinstance(bench, dict):
            errors.append(f"benchmark #{index + 1} must be an object")
            continue

        name = bench.get("name")
        if not isinstance(name, str) or not name:
            errors.append(f"benchmark #{index + 1} has no valid name")
        else:
            result_names.append(name)

        display_name = name if isinstance(name, str) and name else f"#{index + 1}"
        for field in ("mean_ms", "std_ms"):
            value = bench.get(field)
            if not _is_finite_non_negative_number(value):
                errors.append(
                    f"benchmark '{display_name}' has no finite non-negative {field}"
                )

        bench_iterations = bench.get("iterations")
        if (
            not isinstance(bench_iterations, int)
            or isinstance(bench_iterations, bool)
            or bench_iterations <= 0
        ):
            errors.append(
                f"benchmark '{display_name}' iterations must be a positive integer"
            )
        elif isinstance(iterations, int) and not isinstance(iterations, bool):
            if bench_iterations != iterations:
                errors.append(
                    f"benchmark '{display_name}' iterations ({bench_iterations}) "
                    f"does not match root iterations ({iterations})"
                )

        passes = bench.get("passes")
        if not isinstance(passes, dict):
            errors.append(f"benchmark '{display_name}' passes must be an object")
        else:
            for pass_name, pass_data in passes.items():
                if not isinstance(pass_name, str) or not pass_name:
                    errors.append(
                        f"benchmark '{display_name}' has an invalid pass name"
                    )
                    continue
                pass_mean = (
                    pass_data.get("mean_ms") if isinstance(pass_data, dict) else None
                )
                if not _is_finite_non_negative_number(pass_mean):
                    errors.append(
                        f"benchmark '{display_name}' pass '{pass_name}' has no "
                        "finite non-negative mean_ms"
                    )

    duplicates = sorted(
        name for name, count in Counter(result_names).items() if count > 1
    )
    if duplicates:
        errors.append("duplicate benchmark result name(s): " + ", ".join(duplicates))

    expected = set(expected_names)
    actual = set(result_names)
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    if missing:
        errors.append("missing benchmark result(s): " + ", ".join(missing))
    if unknown:
        errors.append("unknown benchmark result name(s): " + ", ".join(unknown))

    return errors
