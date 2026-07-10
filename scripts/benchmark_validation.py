"""Shared schema and corpus validation for benchmark result publication."""

from collections import Counter
from pathlib import Path
import tomllib


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = REPO_ROOT / "benchmarks" / "manifest.toml"


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

    benchmarks = data.get("benchmarks")
    if not isinstance(benchmarks, list):
        return ["benchmarks must be an array"]
    if not benchmarks:
        return ["no benchmark results collected; all benchmarks failed"]

    errors = []
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

        mean = bench.get("mean_ms")
        if not isinstance(mean, (int, float)) or isinstance(mean, bool):
            display_name = name if isinstance(name, str) and name else f"#{index + 1}"
            errors.append(f"benchmark '{display_name}' has no numeric mean_ms")

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
