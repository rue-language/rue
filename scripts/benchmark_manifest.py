#!/usr/bin/env python3
"""Canonical parser and validator for Rue's synthetic benchmark manifest."""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import sys
import tomllib

from scaling_workloads import GENERATORS


BENCHMARK_KIND = "synthetic_phase_probe"
VERIFIED_DIMENSIONS = {"source_lines", "function_declarations", "struct_declarations"}
REQUIRED_PROBE_FIELDS = (
    "name",
    "path",
    "subsystem",
    "workload_family",
    "interpretation",
    "non_representative",
)
REQUIRED_SCALING_FIELDS = (
    "name",
    "generator",
    "subsystem",
    "workload_family",
    "input_axis",
    "sizes",
    "focus_pass",
    "interpretation",
    "non_representative",
    "latency_per_unit_growth_budget",
    "memory_per_unit_growth_budget",
    "timeout_seconds",
)
REQUIRED_SCENARIO_FAMILY_FIELDS = (
    "name", "runner", "fixture", "measurement_family", "interpretation", "not_runtime", "target"
)


def _unique(entries: list[dict], label: str) -> None:
    names = [entry.get("name") for entry in entries]
    duplicates = sorted(name for name, count in Counter(names).items() if count > 1)
    if duplicates:
        raise ValueError(f"duplicate {label} name(s): " + ", ".join(duplicates))


def load_manifest(path: Path) -> dict:
    """Load the single source of benchmark workload semantics."""
    with path.open("rb") as stream:
        manifest = tomllib.load(stream)
    if manifest.get("schema_version") != 3:
        raise ValueError("benchmark manifest schema_version must be 3")
    probes = manifest.get("benchmark")
    families = manifest.get("scaling_family")
    if not isinstance(probes, list) or not probes:
        raise ValueError("manifest must contain at least one [[benchmark]] entry")
    if not isinstance(families, list) or len(families) < 3:
        raise ValueError("manifest must contain at least three [[scaling_family]] entries")
    for index, probe in enumerate(probes):
        if not isinstance(probe, dict):
            raise ValueError(f"manifest benchmark #{index + 1} must be a table")
        for field in REQUIRED_PROBE_FIELDS:
            if not isinstance(probe.get(field), str) or not probe[field]:
                raise ValueError(f"manifest benchmark #{index + 1} has no valid {field}")
        if probe.get("kind") != BENCHMARK_KIND:
            raise ValueError(f"manifest benchmark '{probe['name']}' must be {BENCHMARK_KIND}")
        dimensions = probe.get("dimensions")
        if not isinstance(dimensions, dict) or not dimensions:
            raise ValueError(f"manifest benchmark '{probe['name']}' has no dimensions")
        if any(
            not isinstance(value, int) or isinstance(value, bool) or value <= 0
            for value in dimensions.values()
        ):
            raise ValueError(
                f"manifest benchmark '{probe['name']}' dimensions must be positive integers"
            )
        unknown_dimensions = sorted(set(dimensions) - VERIFIED_DIMENSIONS)
        if unknown_dimensions:
            raise ValueError(
                f"manifest benchmark '{probe['name']}' has unverifiable dimensions: "
                + ", ".join(unknown_dimensions)
            )
    _unique(probes, "benchmark")
    generators = set()
    for index, family in enumerate(families):
        if not isinstance(family, dict):
            raise ValueError(f"scaling family #{index + 1} must be a table")
        for field in REQUIRED_SCALING_FIELDS:
            value = family.get(field)
            if field == "sizes":
                if not isinstance(value, list) or len(value) < 3 or any(
                    not isinstance(size, int) or isinstance(size, bool) or size <= 0
                    for size in value
                ) or value != sorted(set(value)):
                    raise ValueError(
                        f"scaling family '{family.get('name', index + 1)}' sizes "
                        "must be three or more increasing positive integers"
                    )
            elif field.endswith("_budget"):
                if not isinstance(value, (int, float)) or isinstance(value, bool) or value <= 1:
                    raise ValueError(
                        f"scaling family '{family.get('name', index + 1)}' "
                        f"{field} must be greater than 1"
                    )
            elif field == "timeout_seconds":
                if not isinstance(value, (int, float)) or isinstance(value, bool) or value <= 0:
                    raise ValueError(
                        f"scaling family '{family.get('name', index + 1)}' "
                        "timeout_seconds must be positive"
                    )
            elif not isinstance(value, str) or not value:
                raise ValueError(f"scaling family #{index + 1} has no valid {field}")
        generators.add(family["generator"])
    _unique(families, "scaling family")
    if len(generators) != len(families):
        raise ValueError("scaling families must use distinct generators")
    unknown = sorted(generators - set(GENERATORS))
    if unknown:
        raise ValueError("unknown scaling generator(s): " + ", ".join(unknown))
    scenario_families = manifest.get("scenario_family")
    if not isinstance(scenario_families, list) or len(scenario_families) != 2:
        raise ValueError("manifest must contain exactly two [[scenario_family]] entries")
    _unique(scenario_families, "scenario family")
    if [family.get("runner") for family in scenario_families] != [
        "cold_batch_root_compiler", "reused_compiler_session"
    ]:
        raise ValueError("scenario families must declare the canonical cold and reused runners")
    for index, family in enumerate(scenario_families):
        for field in REQUIRED_SCENARIO_FAMILY_FIELDS:
            if not isinstance(family.get(field), str) or not family[field]:
                raise ValueError(f"scenario family #{index + 1} has no valid {field}")
        if family["measurement_family"] != "compiler_build_and_query_performance":
            raise ValueError(f"scenario family '{family['name']}' has invalid measurement family")
        if family["target"] != "host-default":
            raise ValueError(f"scenario family '{family['name']}' has invalid target")
    scenarios = scenario_families[1].get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise ValueError("reused scenario family must contain scenarios")
    _unique(scenarios, "representative scenario")
    for scenario in scenarios:
        for artifact in ("modules", "semantic_bodies", "cfgs", "semantic_queries"):
            counts = scenario.get(artifact)
            if (not isinstance(counts, list) or len(counts) != 2 or any(
                not isinstance(value, int) or isinstance(value, bool) or value < 0
                for value in counts
            )):
                raise ValueError(f"representative scenario '{scenario.get('name')}' has invalid {artifact}")
    corpus = manifest.get("corpus")
    paths = corpus.get("definition_paths") if isinstance(corpus, dict) else None
    if (
        not isinstance(paths, list)
        or not paths
        or any(not isinstance(value, str) or not value for value in paths)
    ):
        raise ValueError("manifest corpus.definition_paths must be a non-empty string array")
    return manifest


def static_probes(path: Path) -> list[dict]:
    return load_manifest(path)["benchmark"]


def scaling_families(path: Path) -> list[dict]:
    return load_manifest(path)["scaling_family"]


def scenario_families(path: Path) -> list[dict]:
    return load_manifest(path)["scenario_family"]


def corpus_paths(path: Path) -> list[tuple[str, Path]]:
    manifest = load_manifest(path)
    result = [(entry["path"], path.parent / entry["path"]) for entry in manifest["benchmark"]]
    for relative in manifest["corpus"]["definition_paths"]:
        result.append((relative, path.parent / relative))
    return result


def validate_static_dimensions(path: Path) -> list[str]:
    errors = []
    for probe in static_probes(path):
        source = (path.parent / probe["path"]).read_text()
        actual = {
            "source_lines": len(source.splitlines()),
            "function_declarations": sum(
                line.startswith("fn ") or line.startswith("pub fn ")
                for line in source.splitlines()
            ),
            "struct_declarations": sum(
                line.startswith("struct ") or line.startswith("pub struct ")
                for line in source.splitlines()
            ),
        }
        for name, expected in probe["dimensions"].items():
            if name in actual and actual[name] != expected:
                errors.append(f"{probe['name']} {name}: manifest={expected}, source={actual[name]}")
    return errors


def render_probe_inventory(path: Path) -> str:
    labels = {
        "source_lines": "lines",
        "function_declarations": "functions",
        "struct_declarations": "structs",
    }
    rows = [
        "| Probe | Primary subsystem | Workload family | Verified dimensions |",
        "|---|---|---|---|",
    ]
    for probe in static_probes(path):
        dimensions = "; ".join(
            f"{value:,} {labels[name]}" for name, value in probe["dimensions"].items()
        )
        rows.append(
            f"| `{probe['name']}` | {probe['subsystem']} | {probe['workload_family']} | {dimensions} |"
        )
    return "\n".join(rows)


def validate_readme_inventory(path: Path) -> list[str]:
    readme = (path.parent / "README.md").read_text()
    begin = "<!-- BEGIN GENERATED PROBE INVENTORY -->"
    end = "<!-- END GENERATED PROBE INVENTORY -->"
    if begin not in readme or end not in readme:
        return ["benchmark README lacks generated probe inventory markers"]
    actual = readme.split(begin, 1)[1].split(end, 1)[0].strip()
    expected = render_probe_inventory(path)
    return [] if actual == expected else ["benchmark README probe inventory is stale"]


def enrich_results(data: dict, path: Path) -> dict:
    """Attach canonical probe semantics without creating a second schema path."""
    declared = {probe["name"]: probe for probe in static_probes(path)}
    for result in data.get("benchmarks", []):
        probe = declared.get(result.get("name"))
        if probe is None:
            continue
        for field in (
            "kind",
            "subsystem",
            "workload_family",
            "interpretation",
            "non_representative",
            "dimensions",
        ):
            result[field] = probe[field]
    data["probe_suite"] = {
        "kind": BENCHMARK_KIND,
        "representative": False,
        "aggregate_interpretation": "synthetic compiler phase diagnostics only",
    }
    return data


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("command", choices=["list-static", "validate", "enrich-results"])
    parser.add_argument("--results", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "list-static":
            for probe in static_probes(args.manifest):
                print(f"{probe['name']}\t{probe['path']}")
        elif args.command == "validate":
            errors = validate_static_dimensions(
                args.manifest
            ) + validate_readme_inventory(args.manifest)
            if errors:
                raise ValueError("; ".join(errors))
        else:
            if args.results is None:
                raise ValueError("enrich-results requires --results")
            data = enrich_results(json.loads(args.results.read_text()), args.manifest)
            args.results.write_text(json.dumps(data, indent=2) + "\n")
    except (OSError, ValueError, tomllib.TOMLDecodeError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
