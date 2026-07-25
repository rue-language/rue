#!/usr/bin/env python3
"""Run Rue's reproducible incrementality value audit.

This is deliberately a black-box orchestration layer.  It does not inspect
semantic/provider implementation details or interpret work-counter fields.
The existing ``perf-baseline.py``, ``rue-compiler-session-bench``, and
``rue-scaling-bench`` binaries remain the measurement owners; this script
only supplies a common revision matrix, paired sampling policy, provenance,
and a fail-closed report.

The historical baseline is optional in practice: old binaries may not accept
the current benchmark JSON protocol or may not have a session benchmark.  In
that case the cold driver falls back to a timing-only black-box compile and
warm scenarios are recorded as unsupported.  That keeps the old revision
useful as context without backporting the modern incrementality harness.
"""

from __future__ import annotations

import argparse
from functools import lru_cache
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from typing import Any


ROLES = ("historical_baseline", "current_production", "candidate")
ROLE_LABELS = {
    "historical_baseline": "historical baseline",
    "current_production": "current production",
    "candidate": "candidate",
}
PAIR_NAMES = (
    ("historical_baseline", "current_production"),
    ("current_production", "candidate"),
)
PAIR_ORDER = "historical,current,candidate then candidate,current,historical"
SCENARIOS = (
    "cold",
    "warm_no_op",
    "warm_unrelated_declaration",
    "warm_leaf_body",
    "warm_signature_fanout",
    "repeated_edit_memory",
)

WORKLOADS = (
    {
        "name": "synthetic",
        "family": "synthetic",
        "root": "benchmarks/stress/many_functions.rue",
        "session_mode": "modules",
        "session_modules": 128,
        # `f000` is edited and `main` calls it: the exact reverse closure is two
        # bodies. Production currently reanalyzes one and imports `main`
        # durably, so this is an upper bound that a correct implementation stays
        # under rather than a prediction of the observed value.
        "warm_leaf_body_reverse_closure": 2,
        "warm_runner": "rue-compiler-session-bench --modules 128",
        "scaling_runner": "rue-scaling-bench --bodies 1000 --decls 100",
        "description": "Generated declaration-volume and completion locality corpus.",
    },
    {
        "name": "representative_multi_module",
        "family": "representative",
        "root": "benchmarks/scenarios/representative/main.rue",
        "session_mode": "representative",
        # `labels.adjustment` is edited, `report.adjust` calls it, and `main`
        # calls `report.adjust`: an exact reverse closure of three bodies. The
        # other three bodies in the fixture are outside it.
        "warm_leaf_body_reverse_closure": 3,
        "warm_runner": "rue-compiler-session-bench --representative",
        "scaling_runner": None,
        "description": "Tracked multi-module application-shaped fixture from benchmarks/manifest.toml.",
    },
    {
        "name": "medium_ordinary_rue",
        "family": "ordinary",
        "root": "examples/quicksort.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "description": "Ordinary medium Rue example; only fresh-driver scenarios are supported.",
    },
    {
        "name": "caldera",
        "family": "caldera",
        "root": "examples/caldera/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 300.0,
        "cold_timeout_seconds": 330.0,
        "description": "Large maintained application graph with a 300-second cold budget.",
    },
    # The five programs the RUE-1026/RUE-1027 query cutover regressed.  Their
    # cold gates come from the checked-in `[historical_reference]` table; see the
    # manifest for how `cold_wall_seconds` is derived and why the process timeout
    # is far looser than the gate.
    {
        "name": "rill",
        "family": "regressed_example",
        "root": "examples/rill/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 1.92,
        "cold_timeout_seconds": 120.0,
        "description": "Bytecode language and VM; 0.32s pre-cutover reference.",
    },
    {
        "name": "mosaic",
        "family": "regressed_example",
        "root": "examples/mosaic/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 2.94,
        "cold_timeout_seconds": 120.0,
        "description": "Medium multi-module application; 0.49s pre-cutover reference.",
    },
    {
        "name": "harbor",
        "family": "regressed_example",
        "root": "examples/harbor/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 5.34,
        "cold_timeout_seconds": 300.0,
        "description": "52-module application graph; 0.89s pre-cutover reference.",
    },
    {
        "name": "lattice",
        "family": "regressed_example",
        "root": "examples/lattice/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 6.9,
        "cold_timeout_seconds": 300.0,
        "description": "131-module application graph; 1.15s pre-cutover reference.",
    },
    {
        "name": "meridian",
        "family": "regressed_example",
        "root": "examples/meridian/main.rue",
        "warm_runner": "unsupported: no persistent-session black-box protocol",
        "scaling_runner": None,
        "cold_wall_seconds": 34.2,
        "cold_timeout_seconds": 900.0,
        "description": "265-module application graph; 5.7s pre-cutover reference.",
    },
)

# Workload families whose root names a directory-shaped program, so the recorded
# source digest must cover the whole tracked module graph rather than the root
# file alone.
DIRECTORY_ROOTED_FAMILIES = {"representative", "caldera", "regressed_example"}

PRECOMMITTED_THRESHOLDS = {
    "warm_unrelated_declaration_improvement": 0.50,
    "warm_leaf_body_improvement": 0.25,
    "claimed_win_mad_multiplier": 3.0,
    "cold_regression_fraction": 0.02,
    "repeated_edit_rss_band": 0.05,
    "caldera_wall_seconds": 300.0,
}

LOCALITY_COUNTERS = (
    "modules",
    "rir",
    "declarations",
    "durable_source",
    "semantic_bodies",
    "cfgs",
    "semantic_queries",
    "cold_body_preparations",
    "declarations_inspected",
    "modules_registered",
    "rir_indexes_constructed",
    "rir_instructions_visited",
    "durable_source_records_inspected",
    "durable_source_records_copied",
    "shells_prepared",
    "projections_performed",
    "semantics_installed",
    "endpoints_installed",
)
COUNTER_DIMENSIONS = ("computed", "reused", "invalidated", "required")
COLD_SEMANTIC_WORK_REQUIRED_PATHS = (
    ("schema_version",),
    ("bodies_attempted",),
    ("bodies_succeeded",),
    ("bodies_failed",),
    ("body_analyses_computed",),
    ("body_analyses_reused",),
    ("body_analyses_invalidated",),
    ("per_body_declaration_context", "cold_body_preparations"),
    ("per_body_declaration_context", "declarations_inspected"),
    ("per_body_declaration_context", "modules_registered"),
    ("per_body_declaration_context", "rir_indexes_constructed"),
    ("per_body_declaration_context", "rir_instructions_visited"),
    ("per_body_declaration_context", "shells_prepared"),
    ("per_body_declaration_context", "projections_performed"),
    ("per_body_declaration_context", "semantics_installed"),
    ("per_body_declaration_context", "endpoints_installed"),
    ("per_body_declaration_context", "durable_source_records_inspected"),
    ("per_body_declaration_context", "durable_source_records_copied"),
    ("durable_bodies", "candidate_comparisons"),
    ("durable_bodies", "reused_bodies"),
    ("cfg", "builds_attempted"),
    ("cfg", "builds_succeeded"),
    ("cfg", "builds_failed"),
)
PARITY_EXACT_FIELDS = (
    "public_semantic_cfg_artifacts",
    "public_type_universe",
    "durable_specialized_body_payloads",
    "durable_ordinary_body_manifest_artifacts",
    "diagnostics",
    "warnings",
    "stable_identities",
    "dependency_records",
    "emitted_output",
)
PARITY_EXACT_VALUES = {
    "public_type_universe": {"exact", "exact_ordered_entries"},
    "emitted_output": {"exact", "byte_exact"},
}


@lru_cache(maxsize=1)
def load_perf_baseline(root: Path):
    path = root / "scripts" / "perf-baseline.py"
    spec = importlib.util.spec_from_file_location("rue_perf_baseline", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_paths(paths: list[Path], base: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted({path.resolve() for path in paths}):
        if not path.is_file():
            continue
        try:
            label = path.relative_to(base.resolve()).as_posix()
        except ValueError:
            label = str(path)
        digest.update(label.encode())
        digest.update(b"\0")
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        digest.update(b"\0")
    return digest.hexdigest()


def git_value(directory: Path, *args: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(directory), *args],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    value = result.stdout.strip()
    return value or None


def all_files(directory: Path) -> list[Path]:
    return [
        path
        for path in directory.rglob("*")
        if path.is_file() and ".git" not in path.parts
    ]


def tracked_files(directory: Path) -> list[Path]:
    names = git_value(directory, "ls-files", "-z")
    if names is not None:
        return [directory / item for item in names.split("\0") if item]
    return all_files(directory)


def source_provenance(source_dir: Path) -> dict[str, Any]:
    source_dir = source_dir.resolve()
    commit = git_value(source_dir, "rev-parse", "HEAD")
    tree = git_value(source_dir, "rev-parse", "HEAD^{tree}")
    paths = tracked_files(source_dir)
    return {
        "directory": str(source_dir),
        "commit": commit,
        "source_tree": tree,
        "source_files_sha256": sha256_paths(paths, source_dir),
    }


def host_provenance() -> dict[str, Any]:
    uname = platform.uname()
    return {
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "python": platform.python_version(),
        "logical_cpus": os.cpu_count(),
        "uname": " ".join(part for part in uname if part),
    }


def load_audit_manifest(root: Path) -> dict[str, Any]:
    path = root / "benchmarks" / "value-audit" / "manifest.toml"
    with path.open("rb") as stream:
        manifest = tomllib.load(stream)
    if manifest.get("schema_version") != 2:
        raise ValueError("value-audit manifest schema_version must be 2")
    protocol = manifest.get("protocol")
    if not isinstance(protocol, dict) or protocol.get("paired_samples") != 7 or protocol.get("warmup") != 1:
        raise ValueError("value-audit manifest must pin one warmup and seven paired samples")
    if protocol.get("pair_order") != PAIR_ORDER:
        raise ValueError("value-audit manifest pair order drifted from the runner")
    if protocol.get("measurement_profile") != "release":
        raise ValueError("value-audit manifest must pin the release measurement profile")
    if not isinstance(protocol.get("caldera_timeout_headroom_seconds"), (int, float)) or protocol["caldera_timeout_headroom_seconds"] <= 0:
        raise ValueError("value-audit manifest must provide positive Caldera timeout headroom")
    source_manifest = path.parent / str(manifest.get("source_manifest", ""))
    if source_manifest.resolve() != (root / "benchmarks" / "manifest.toml").resolve():
        raise ValueError("value-audit manifest must reuse benchmarks/manifest.toml")
    thresholds = manifest.get("thresholds")
    if not isinstance(thresholds, dict):
        raise ValueError("value-audit manifest must define thresholds")
    for name, expected in PRECOMMITTED_THRESHOLDS.items():
        actual = thresholds.get(name)
        if not isinstance(actual, (int, float)) or actual != expected:
            raise ValueError(
                f"value-audit manifest threshold {name} drifted from the precommitted value {expected}"
            )
    policy = manifest.get("scenario_policy")
    if not isinstance(policy, dict) or set(policy) != set(SCENARIOS):
        raise ValueError("value-audit manifest scenario policy drifted from the runner")
    for scenario in SCENARIOS:
        entry = policy[scenario]
        if not isinstance(entry, dict) or not isinstance(entry.get("required"), bool):
            raise ValueError(f"scenario policy for {scenario} must declare required=true/false")
    locality = manifest.get("locality")
    required_locality = set(SCENARIOS) - {"cold", "repeated_edit_memory"}
    if not isinstance(locality, dict) or not required_locality.issubset(locality):
        raise ValueError("value-audit manifest locality policy is incomplete")
    for scenario in required_locality:
        entry = locality[scenario]
        if not isinstance(entry, dict):
            raise ValueError(f"locality policy for {scenario} must be a table")
        for counter in LOCALITY_COUNTERS:
            key = f"max_{counter}_computed"
            value = entry.get(key)
            if not isinstance(value, int) or value < 0:
                raise ValueError(f"locality policy for {scenario} must define non-negative {key}")
    workloads = manifest.get("workload")
    expected = {workload["name"] for workload in WORKLOADS}
    if not isinstance(workloads, list) or {item.get("name") for item in workloads} != expected:
        raise ValueError("value-audit manifest workload matrix drifted from the runner")
    if len(workloads) != len(WORKLOADS):
        raise ValueError("value-audit manifest workload matrix contains duplicate entries")
    expected_workloads = {workload["name"]: workload for workload in WORKLOADS}
    for item in workloads:
        expected_workload = expected_workloads.get(item.get("name"))
        if expected_workload is None:
            raise ValueError(f"value-audit manifest has an unknown workload {item.get('name')}")
        expected_manifest_root = os.path.normpath(
            os.path.relpath(expected_workload["root"], "benchmarks/value-audit")
        )
        if item.get("root") != expected_manifest_root:
            raise ValueError(f"workload {item.get('name')} root drifted from the runner")
        if item.get("warm_runner") != expected_workload["warm_runner"]:
            raise ValueError(f"workload {item.get('name')} warm runner drifted from the runner")
        if item.get("scaling_runner") != expected_workload["scaling_runner"]:
            raise ValueError(f"workload {item.get('name')} scaling runner drifted from the runner")
        supported = item.get("supported_scenarios")
        if not isinstance(supported, list) or not set(supported).issubset(SCENARIOS):
            raise ValueError(f"workload {item.get('name')} has invalid supported_scenarios")
        for key in ("cold_wall_seconds", "cold_timeout_seconds"):
            expected_budget = expected_workload.get(key)
            actual_budget = item.get(key)
            if expected_budget is None:
                if actual_budget is not None:
                    raise ValueError(f"workload {item.get('name')} declares an unexpected {key}")
                continue
            if not isinstance(actual_budget, (int, float)) or float(actual_budget) != expected_budget:
                raise ValueError(f"workload {item.get('name')} {key} drifted from the runner")
        expected_closure = expected_workload.get("warm_leaf_body_reverse_closure")
        actual_closure = item.get("warm_leaf_body_reverse_closure")
        if expected_closure is None:
            if actual_closure is not None:
                raise ValueError(
                    f"workload {item.get('name')} declares a leaf closure for a scenario it does not support"
                )
        elif actual_closure != expected_closure:
            raise ValueError(
                f"workload {item.get('name')} leaf reverse closure drifted from the runner"
            )
        if ("warm_leaf_body" in supported) != (actual_closure is not None):
            raise ValueError(
                f"workload {item.get('name')} must declare a leaf reverse closure exactly when it supports warm_leaf_body"
            )
        gate = item.get("cold_wall_seconds")
        bound = item.get("cold_timeout_seconds")
        if gate is not None and (bound is None or bound <= gate):
            raise ValueError(
                f"workload {item.get('name')} must bound its process timeout above its cold gate "
                "so an over-budget compile fails the gate instead of crashing the run"
            )
    caldera = next(item for item in workloads if item.get("name") == "caldera")
    if caldera.get("cold_wall_seconds") != thresholds["caldera_wall_seconds"]:
        raise ValueError("the Caldera cold gate must restate thresholds.caldera_wall_seconds exactly")
    if caldera.get("cold_timeout_seconds") != (
        thresholds["caldera_wall_seconds"] + protocol["caldera_timeout_headroom_seconds"]
    ):
        raise ValueError(
            "the Caldera process timeout must be its gate plus the manifest-pinned headroom"
        )
    _validate_historical_reference(manifest, expected_workloads)
    return manifest


def _validate_historical_reference(
    manifest: dict[str, Any], expected_workloads: dict[str, dict[str, Any]]
) -> None:
    """Cross-check the checked-in pre-cutover reference against the cold gates.

    The reference is prose evidence transcribed from
    `docs/notes/rue-1083-closure-evidence.md`, not a run of this protocol. It is
    validated here for exactly two properties: that it cannot be mistaken for
    role evidence, and that every `regressed_example` cold gate is the declared
    multiple of its reference rather than a hand-tuned number that drifted away
    from the regression it is supposed to measure.
    """
    reference = manifest.get("historical_reference")
    if not isinstance(reference, dict):
        raise ValueError("value-audit manifest must carry the historical reference table")
    if reference.get("produced_by_this_protocol") is not False:
        raise ValueError("the historical reference must declare that this protocol did not produce it")
    if reference.get("usable_as_role_evidence") is not False:
        raise ValueError("the historical reference must declare that it is not role evidence")
    multiplier = reference.get("cold_gate_multiplier")
    if not isinstance(multiplier, (int, float)) or multiplier <= 1:
        raise ValueError("the historical reference must declare a cold gate multiplier above one")
    programs = reference.get("programs")
    if not isinstance(programs, dict):
        raise ValueError("the historical reference must name its per-program measurements")
    regressed = {
        name for name, workload in expected_workloads.items()
        if workload["family"] == "regressed_example"
    }
    if set(programs) != regressed:
        raise ValueError("the historical reference programs drifted from the regressed_example workloads")
    for name, entry in programs.items():
        pre = entry.get("pre_cutover_seconds") if isinstance(entry, dict) else None
        post = entry.get("post_cutover_seconds") if isinstance(entry, dict) else None
        if not isinstance(pre, (int, float)) or pre <= 0:
            raise ValueError(f"historical reference for {name} needs a positive pre-cutover value")
        if not isinstance(post, (int, float)) or post <= pre:
            raise ValueError(
                f"historical reference for {name} must record a post-cutover regression above its reference"
            )
        gate = expected_workloads[name]["cold_wall_seconds"]
        if abs(gate - pre * multiplier) > 0.005:
            raise ValueError(
                f"cold gate for {name} is not {multiplier}x its pre-cutover reference"
            )
        if post <= gate:
            raise ValueError(
                f"cold gate for {name} does not separate the reference from the recorded regression"
            )


def median_mad(values: list[float]) -> dict[str, Any]:
    if not values:
        return {"available": False, "samples": [], "median": None, "mad": None}
    median = statistics.median(values)
    mad = statistics.median(abs(value - median) for value in values)
    return {
        "available": True,
        "samples": values,
        "median": median,
        "mad": mad,
    }


def parse_role_mapping(values: list[str], label: str) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for value in values:
        role, separator, path = value.partition("=")
        if not separator or role not in ROLES or not path:
            raise argparse.ArgumentTypeError(
                f"{label} must be ROLE=PATH for {', '.join(ROLES)}"
            )
        if role in result:
            raise argparse.ArgumentTypeError(f"duplicate {label} entry for {role}")
        result[role] = Path(path).expanduser().resolve()
    return result


def time_prefix() -> tuple[list[str], str]:
    if Path("/usr/bin/time").exists():
        if platform.system() == "Darwin":
            return ["/usr/bin/time", "-l"], "darwin"
        return ["/usr/bin/time", "-v"], "gnu"
    return [], "none"


def parse_rss(stderr: str, style: str) -> int | None:
    if style == "darwin":
        # macOS has emitted both `maximum resident set size: 123` and
        # `123 maximum resident set size` forms across supported hosts.
        match = re.search(r"maximum resident set size:\s*(\d+)", stderr)
        if match:
            return int(match.group(1))
        match = re.search(r"(\d+)\s+maximum resident set size", stderr)
        return int(match.group(1)) if match else None
    if style == "gnu":
        match = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
        return int(match.group(1)) * 1024 if match else None
    return None


def measurement_env(root: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.pop("RUST_LOG", None)
    env["RUE_STD_PATH"] = str(root / "std")
    return env


def run_command(command: list[str], timeout: float, env: dict[str, str]) -> dict[str, Any]:
    prefix, style = time_prefix()
    started = time.perf_counter_ns()
    try:
        result = subprocess.run(
            prefix + command,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f"command exceeded {timeout:g}s: {' '.join(command)}") from error
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return {
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "wall_ms": elapsed_ms,
        "peak_rss_bytes": parse_rss(result.stderr, style),
        "command": command,
    }


def json_object(stdout: str) -> dict[str, Any] | None:
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def cold_sample(
    binary: Path,
    source: Path,
    root: Path,
    timeout: float,
    jobs: int,
    workdir: Path,
    perf_baseline: Any,
) -> dict[str, Any]:
    output = workdir / "audit-output"
    output.unlink(missing_ok=True)
    env = measurement_env(root)
    modern = [
        str(binary),
        "--benchmark-json",
        "--jobs",
        str(jobs),
        "-O0",
        str(source),
        "-o",
        str(output),
    ]
    result = run_command(modern, timeout, env)
    payload = json_object(result["stdout"])
    protocol = "benchmark_json"
    if result["returncode"] != 0 or payload is None:
        # Old pre-query revisions are allowed to use only the executable
        # protocol.  Do not infer pass-level work from their human timing text.
        output.unlink(missing_ok=True)
        legacy = [str(binary), str(source), "-o", str(output)]
        result = run_command(legacy, timeout, env)
        payload = None
        protocol = "black_box_compile"
    if result["returncode"] != 0:
        output.unlink(missing_ok=True)
        detail = result["stderr"].strip() or result["stdout"].strip()
        raise RuntimeError(f"compile failed ({result['returncode']}): {detail[-1200:]}")
    if not output.is_file():
        raise RuntimeError("compiler succeeded without producing an output artifact")
    output_hash = sha256_file(output)
    output_size = output.stat().st_size
    output.unlink(missing_ok=True)
    sample: dict[str, Any] = {
        "protocol": protocol,
        "wall_ms": result["wall_ms"],
        "peak_rss_bytes": result["peak_rss_bytes"],
        "output_sha256": output_hash,
        "output_size_bytes": output_size,
        "correctness": "exit_zero_and_output_present",
    }
    if payload is not None:
        # Reuse perf-baseline's schema/aggregation validator for modern JSON.
        # The audit keeps the raw payload as evidence, but does not duplicate
        # its pass-nesting interpretation here.
        payload["_process_ms"] = result["wall_ms"]
        sample["perf_baseline_aggregate"] = perf_baseline.aggregate([payload])
        sample["compiler_ms"] = payload.get("total_ms")
        sample["benchmark_json"] = payload
    return sample


SESSION_ROW_NAMES = {
    "synthetic": {
        "warm_no_op": "completion_exact_noop",
        "warm_unrelated_declaration": "completion_unrelated_edit",
        "warm_leaf_body": "completion_changed_reachable_body",
        "warm_signature_fanout": "completion_reverse_closure",
    },
    "representative": {
        "warm_no_op": "no_change_query",
        "warm_leaf_body": "leaf_edit",
        "warm_signature_fanout": "import_change",
    },
}


def parity_check(row: dict[str, Any]) -> dict[str, Any]:
    parity = row.get("differential_parity")
    if not isinstance(parity, dict):
        return {"status": "unsupported", "reason": "session runner omitted differential parity evidence"}
    if parity.get("schema_version") != 1:
        return {"status": "unsupported", "reason": "differential parity schema version is missing or unknown"}
    if parity.get("status") != "pass" or parity.get("comparison") != "cold_vs_reused_in_process":
        return {"status": "unsupported", "reason": "differential parity did not report a completed in-process comparison"}
    if parity.get("comparison_completed") is not True:
        return {"status": "unsupported", "reason": "differential parity comparison was not marked complete"}
    for field in PARITY_EXACT_FIELDS:
        if parity.get(field) not in PARITY_EXACT_VALUES.get(field, {"exact"}):
            return {"status": "unsupported", "reason": f"differential parity field {field} is not exact"}
    digest = parity.get("emitted_output_sha256")
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        return {"status": "unsupported", "reason": "differential parity has no valid emitted-output digest"}
    if not isinstance(parity.get("emitted_output_size_bytes"), int) or parity["emitted_output_size_bytes"] <= 0:
        return {"status": "unsupported", "reason": "differential parity has no positive emitted-output size"}
    cold_work = parity.get("cold_semantic_work")
    if not isinstance(cold_work, dict) or not cold_work:
        return {"status": "unsupported", "reason": "differential parity has empty cold work evidence"}
    for path in COLD_SEMANTIC_WORK_REQUIRED_PATHS:
        value: Any = cold_work
        for key in path:
            if not isinstance(value, dict) or key not in value:
                return {
                    "status": "unsupported",
                    "reason": f"differential parity cold work omitted {'.'.join(path)}",
                }
            value = value[key]
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            return {
                "status": "unsupported",
                "reason": f"differential parity cold work has malformed {'.'.join(path)}",
            }
    if cold_work["schema_version"] != 1:
        return {"status": "unsupported", "reason": "differential parity cold work schema version is missing or unknown"}
    return {"status": "pass", "evidence": parity}


def counter_value(values: dict[str, Any], field: str) -> int | None:
    value = values.get(field)
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value
    return None


def unavailable_counter(value: Any) -> bool:
    return (
        isinstance(value, dict)
        and value.get("available") is False
        and isinstance(value.get("reason"), str)
        and bool(value["reason"])
    )


def warm_unit_scale(workload: dict[str, Any], scenario: str) -> int:
    """How many units of production work the scenario's edit legitimately costs.

    The manifest's locality bounds are stated *per recomputed unit*, not per
    request. A leaf edit does not cost one unit: exact reverse invalidation
    recomputes the edited body and its direct reverse callers, and that count is
    a property of the fixture's call graph rather than of the gate. Multiplying
    the per-unit bounds by the workload's declared closure size keeps the gate
    tight without failing an implementation that is behaving correctly.

    Every other scenario is a single unit: a no-op and an unrelated-declaration
    edit are bounded at zero, and the fanout scenario already carries its own
    explicit allowance in the manifest.
    """
    if scenario != "warm_leaf_body":
        return 1
    return workload.get("warm_leaf_body_reverse_closure", 1)


def locality_check(
    scenario: str,
    row: dict[str, Any],
    policy: dict[str, Any],
    unit_scale: int = 1,
) -> dict[str, Any]:
    evidence_schema = row.get("evidence_schema")
    if evidence_schema != {
        "version": 2,
        "differential_parity_version": 1,
        "locality_work_version": 2,
    }:
        return {"status": "unsupported", "reason": "session row omitted the explicit evidence schema attachment"}
    work = row.get("required_vs_reused_work")
    if not isinstance(work, dict) or work.get("schema_version") != 2:
        return {"status": "unsupported", "reason": "session runner omitted schema-versioned locality evidence"}
    if work.get("counter_source") != "production_metrics":
        return {"status": "unsupported", "reason": "locality evidence is not sourced from production metrics"}
    identity_sets = work.get("exact_identity_sets")
    if not isinstance(identity_sets, dict) or not isinstance(identity_sets.get("available"), bool):
        return {"status": "unsupported", "reason": "locality evidence omitted exact-identity-set availability"}
    if identity_sets["available"]:
        for key in ("recomputed", "reused"):
            if not isinstance(identity_sets.get(key), list):
                return {"status": "unsupported", "reason": "available identity sets are malformed"}
    elif not isinstance(identity_sets.get("reason"), str) or not identity_sets["reason"]:
        return {"status": "unsupported", "reason": "unavailable identity sets were not explained"}

    for artifact in LOCALITY_COUNTERS:
        values = work.get(artifact)
        if not isinstance(values, dict):
            return {"status": "unsupported", "reason": f"locality evidence omitted {artifact} counters"}
        for field in COUNTER_DIMENSIONS:
            value = values.get(field)
            if counter_value(values, field) is None and not unavailable_counter(value):
                return {"status": "unsupported", "reason": f"locality evidence has malformed {artifact}.{field}"}

    # Every warm locality gate is an upper-bounded production-work gate. A
    # lower bound such as "some CFG was reused" cannot detect a full-program
    # recompute. Unavailable dimensions are shape-checked above but never
    # treated as zero or as independent evidence here.
    maxima = policy
    if any(
        not isinstance(maxima.get(f"max_{artifact}_computed"), int)
        or isinstance(maxima.get(f"max_{artifact}_computed"), bool)
        or maxima[f"max_{artifact}_computed"] < 0
        for artifact in LOCALITY_COUNTERS
    ):
        return {"status": "unsupported", "reason": "manifest locality bounds are malformed"}
    if not isinstance(unit_scale, int) or isinstance(unit_scale, bool) or unit_scale < 1:
        return {"status": "unsupported", "reason": "locality unit scale is not a positive integer"}
    violations = []
    for artifact in LOCALITY_COUNTERS:
        # Per-unit bound times the units this edit legitimately recomputes. A
        # zero per-unit bound stays zero at any scale, so the no-op and
        # unrelated-edit gates cannot be loosened by this multiplication.
        maximum = maxima[f"max_{artifact}_computed"] * unit_scale
        values = work[artifact]
        if counter_value(values, "computed") is None:
            return {"status": "unsupported", "reason": f"locality evidence has no computed {artifact} counter"}
        for dimension in ("computed", "invalidated"):
            observed = counter_value(values, dimension)
            if observed is not None and observed > maximum:
                violations.append({
                    "artifact": artifact,
                    "dimension": dimension,
                    "maximum": maximum,
                    "observed": observed,
                })
    if violations:
        return {"status": "fail", "reason": "production work exceeded the manifest locality bounds", "violations": violations, "work": work}

    # Semantic work is sliced to the current request by the session benchmark.
    # A non-zero body/context counter with no current semantic execution is a
    # historical cached-result field, not current-request recomputation. Do not
    # let an exact-cycle cache hit satisfy a locality claim.
    semantic_execution_count = counter_value(work["semantic_queries"], "computed")
    current_request_work = (
        "semantic_bodies",
        "durable_source",
        "cfgs",
        "shells_prepared",
        "projections_performed",
        "semantics_installed",
        "endpoints_installed",
    )
    if semantic_execution_count == 0 and any(
        (counter_value(work[artifact], "computed") or 0) > 0
        for artifact in current_request_work
    ):
        return {
            "status": "unsupported",
            "reason": "current-request semantic work is unavailable; counters may be historical cached-result fields",
            "work": work,
        }

    # A successful edit must show that the corresponding production query did
    # run.  This is deliberately narrow and does not turn reuse into a pass.
    if scenario == "warm_no_op" and (counter_value(work["semantic_queries"], "reused") or 0) < 1:
        return {"status": "unsupported", "reason": "no-op row has no reused semantic-query evidence", "work": work}
    if scenario == "warm_unrelated_declaration" and not any(
        (counter_value(work[artifact], "reused") or 0) > 0 for artifact in LOCALITY_COUNTERS
    ):
        return {"status": "unsupported", "reason": "unrelated-edit row has no reused production work evidence", "work": work}
    if scenario in {"warm_leaf_body", "warm_signature_fanout"} and (counter_value(work["semantic_bodies"], "computed") or 0) < 1:
        return {"status": "unsupported", "reason": "edit row has no computed body evidence", "work": work}
    if scenario == "warm_leaf_body" and (counter_value(work["cfgs"], "computed") or 0) < 1:
        return {"status": "unsupported", "reason": "leaf edit has no computed CFG evidence", "work": work}
    if scenario == "warm_signature_fanout" and (counter_value(work["modules"], "computed") or 0) < 1 and (counter_value(work["semantic_bodies"], "computed") or 0) < 1:
        return {"status": "unsupported", "reason": "fanout edit has no computed locality evidence", "work": work}
    return {"status": "pass", "work": work}


def session_sample(
    session_binary: Path,
    workload: dict[str, Any],
    scenario: str,
    timeout: float,
    root: Path,
    locality_policy: dict[str, Any],
) -> dict[str, Any]:
    mode = workload["session_mode"]
    if scenario not in SESSION_ROW_NAMES.get(workload["family"], {}):
        return {"status": "unsupported", "reason": "existing session benchmark has no matching scenario"}
    command = [str(session_binary)]
    if mode == "representative":
        command += ["--representative"]
    else:
        command += ["--modules", str(workload["session_modules"])]
    command += ["--warmup", "0", "--iterations", "1"]
    result = run_command(command, timeout, measurement_env(root))
    if result["returncode"] != 0:
        detail = result["stderr"].strip() or result["stdout"].strip()
        raise RuntimeError(f"session benchmark failed: {detail[-1200:]}")
    payload = json_object(result["stdout"])
    if payload is None or not isinstance(payload.get("iterations"), list) or len(payload["iterations"]) != 1:
        raise RuntimeError("session benchmark returned an invalid JSON envelope")
    rows = payload["iterations"][0]
    if not isinstance(rows, list):
        raise RuntimeError("session benchmark iteration is not an array")
    wanted = SESSION_ROW_NAMES[workload["family"]][scenario]
    row = next((candidate for candidate in rows if isinstance(candidate, dict) and candidate.get("name") == wanted), None)
    if row is None:
        raise RuntimeError(f"session benchmark did not report {wanted}")
    timing_ns = row.get("wall_time_ns")
    if not isinstance(timing_ns, int) or timing_ns <= 0:
        raise RuntimeError(f"session benchmark row {wanted} has no positive wall_time_ns")
    parity_result = parity_check(row)
    locality = locality_check(
        scenario, row, locality_policy, warm_unit_scale(workload, scenario)
    )
    evidence_status = "pass"
    if parity_result["status"] != "pass" or locality["status"] == "unsupported":
        evidence_status = "indeterminate"
    elif locality["status"] == "fail":
        evidence_status = "fail"
    return {
        "status": evidence_status,
        "protocol": "session_benchmark_json",
        "wall_ms": timing_ns / 1_000_000,
        "peak_rss_bytes": result["peak_rss_bytes"],
        "runner_wall_ms": result["wall_ms"],
        "scenario_name": wanted,
        "exact_parity": parity_result,
        "locality": locality,
        "row": row,
    }


def scaling_sample(
    scaling_binary: Path,
    warm: bool,
    timeout: float,
    root: Path,
) -> dict[str, Any]:
    command = [
        str(scaling_binary),
        "--mode",
        "timing",
        "--bodies",
        "1000",
        "--decls",
        "100",
        "--iterations",
        "1",
        "--json",
    ]
    if warm:
        command.append("--warm")
    result = run_command(command, timeout, measurement_env(root))
    if result["returncode"] != 0:
        raise RuntimeError(f"scaling benchmark failed: {result['stderr'][-1200:]}")
    payload = json_object(result["stdout"])
    if payload is None:
        raise RuntimeError("scaling benchmark returned invalid JSON")
    values = payload.get("pre_link", {}).get("samples_ns")
    if not isinstance(values, list) or len(values) != 1 or not isinstance(values[0], str):
        raise RuntimeError("scaling benchmark returned no pre_link sample")
    return {
        "wall_ms": int(values[0]) / 1_000_000,
        "peak_rss_bytes": result["peak_rss_bytes"],
        "protocol": "rue-scaling-bench",
        "payload": payload,
    }


def merge_role_sample(role_row: dict[str, Any], sample: dict[str, Any], scenario: str) -> None:
    """Merge one measurement without allowing unsupported evidence to erase prior samples."""
    if sample.get("status") == "unsupported":
        if role_row.get("wall_samples"):
            role_row["status"] = "indeterminate"
            role_row.setdefault("details", []).append(sample)
        else:
            role_row["status"] = "unsupported"
            role_row["reason"] = sample.get("reason")
        return
    if scenario == "cold":
        sample["status"] = "pass"
    role_row.setdefault("wall_samples", []).append(float(sample["wall_ms"]))
    role_row.setdefault("rss_samples", []).append(sample.get("peak_rss_bytes"))
    if sample.get("status") == "fail":
        role_row["status"] = "fail"
    elif sample.get("status") == "indeterminate":
        role_row["status"] = "indeterminate"
    role_row.setdefault("details", []).append(sample)


def pair_verdict(
    left: dict[str, Any],
    right: dict[str, Any],
    kind: str,
    threshold: float | None = None,
    thresholds: dict[str, Any] | None = None,
) -> dict[str, Any]:
    if thresholds is None:
        raise ValueError("pair_verdict requires thresholds loaded from the audit manifest")
    if not isinstance(left, dict) or not isinstance(right, dict):
        return {"status": "indeterminate", "reason": "malformed timing evidence"}
    left_median = left.get("median")
    right_median = right.get("median")
    if (
        not isinstance(left_median, (int, float))
        or isinstance(left_median, bool)
        or not math.isfinite(left_median)
        or not isinstance(right_median, (int, float))
        or isinstance(right_median, bool)
        or not math.isfinite(right_median)
    ):
        return {"status": "unsupported", "reason": "one side has no samples"}
    left_mad = left.get("mad")
    right_mad = right.get("mad")
    if left_mad is None:
        left_mad = 0.0
    if right_mad is None:
        right_mad = 0.0
    if (
        not isinstance(left_mad, (int, float))
        or isinstance(left_mad, bool)
        or not math.isfinite(left_mad)
        or not isinstance(right_mad, (int, float))
        or isinstance(right_mad, bool)
        or not math.isfinite(right_mad)
    ):
        return {"status": "indeterminate", "reason": "malformed MAD evidence"}
    noise = thresholds["claimed_win_mad_multiplier"] * max(left_mad, right_mad)
    delta = right_median - left_median
    if kind == "improvement":
        improvement = (left_median - right_median) / left_median if left_median else None
        if improvement is None:
            return {"status": "indeterminate", "reason": "zero baseline median"}
        if improvement < threshold:
            status = "fail"
            reason = "improvement below precommitted threshold"
        elif left_median - right_median <= noise:
            status = "indeterminate"
            reason = "claimed win does not exceed three times the larger MAD"
        else:
            status = "pass"
            reason = "improvement and MAD gates pass"
        return {
            "status": status,
            "reason": reason,
            "improvement_fraction": improvement,
            "delta": delta,
            "noise_budget": noise,
            "threshold": threshold,
        }
    allowed = max(thresholds["cold_regression_fraction"] * left_median, noise)
    return {
        "status": "pass" if delta <= allowed else "fail",
        "reason": "within cold wall/RSS regression budget" if delta <= allowed else "cold regression exceeds budget",
        "delta": delta,
        "allowed_regression": allowed,
        "noise_budget": noise,
        "threshold_fraction": thresholds["cold_regression_fraction"],
    }


def scenario_verdict(
    scenario: str,
    role_rows: dict[str, dict[str, Any]],
    workload: dict[str, Any],
    scenario_policy: dict[str, Any],
    thresholds: dict[str, Any],
    baseline_available: bool = False,
) -> dict[str, Any]:
    """Decide one scenario.

    `baseline_available` says whether this run supplied a genuinely distinct
    historical baseline binary. It decides how an absolute `cold_wall_seconds`
    breach is reported — see the gate below for why.
    """
    pairs: dict[str, Any] = {}
    required = scenario_policy["required"]
    if scenario not in workload["supported_scenarios"]:
        return {
            "status": "not_applicable",
            "required": False,
            "reason": "scenario is not declared by the workload in the audit manifest",
            "pairs": {},
        }
    if scenario == "cold":
        kind = "regression"
        threshold = None
    elif scenario == "warm_unrelated_declaration":
        kind = "improvement"
        threshold = thresholds["warm_unrelated_declaration_improvement"]
    elif scenario == "warm_leaf_body":
        kind = "improvement"
        threshold = thresholds["warm_leaf_body_improvement"]
    else:
        kind = "observation"
        threshold = None
    for left_role, right_role in PAIR_NAMES:
        left = role_rows.get(left_role, {})
        right = role_rows.get(right_role, {})
        if left.get("status") in {"unsupported", "indeterminate"} or right.get("status") in {"unsupported", "indeterminate"}:
            pairs[f"{left_role}_vs_{right_role}"] = {
                "status": "indeterminate" if required else "unsupported",
                "reason": "role lacks complete required protocol evidence",
            }
            continue
        if left.get("status") == "fail" or right.get("status") == "fail":
            pairs[f"{left_role}_vs_{right_role}"] = {"status": "fail", "reason": "exact correctness or locality evidence failed"}
            continue
        left_binary = left.get("binary_sha256")
        right_binary = right.get("binary_sha256")
        if isinstance(left_binary, str) and isinstance(right_binary, str) and left_binary == right_binary:
            pairs[f"{left_role}_vs_{right_role}"] = {
                "status": "unsupported",
                "reason": "identical binary hashes cannot support a value or improvement claim",
                "same_binary": True,
                "binary_sha256": left_binary,
            }
            continue
        if left.get("protocol") != right.get("protocol"):
            pairs[f"{left_role}_vs_{right_role}"] = {
                "status": "indeterminate",
                "reason": "timing protocols differ; benchmark_json and black_box_compile are not comparable",
                "left_protocol": left.get("protocol"),
                "right_protocol": right.get("protocol"),
            }
            continue
        if not isinstance(left.get("wall"), dict) or not isinstance(right.get("wall"), dict):
            pairs[f"{left_role}_vs_{right_role}"] = {
                "status": "indeterminate",
                "reason": "role row is partially shaped and has no timing evidence",
            }
            continue
        if scenario == "repeated_edit_memory":
            pairs[f"{left_role}_vs_{right_role}"] = {"status": "unsupported", "reason": "no repeated-edit session protocol is exposed by the reused benchmark"}
            continue
        if kind == "observation":
            pair = {"status": "pass", "reason": "exact/locality observation recorded", "wall": {
                "left": left["wall"], "right": right["wall"]
            }}
        else:
            pair = pair_verdict(left["wall"], right["wall"], kind, threshold, thresholds)
        if scenario == "cold":
            rss_left = left.get("rss", {})
            rss_right = right.get("rss", {})
            rss = pair_verdict(rss_left, rss_right, "regression", thresholds=thresholds)
            pair["rss"] = rss
            if rss["status"] == "fail":
                pair["status"] = "fail"
                pair["reason"] = "cold RSS regression exceeds budget"
            elif rss["status"] in {"unsupported", "indeterminate"}:
                pair["status"] = "indeterminate"
                pair["reason"] = "cold RSS evidence is unavailable or indeterminate"
        pairs[f"{left_role}_vs_{right_role}"] = pair
    for role, row in role_rows.items():
        if row.get("status") == "fail":
            pairs.setdefault(role, {"status": "fail", "reason": "role evidence failed"})
        # The absolute cold budget is per-role and needs no pairing, which is
        # what makes it useful with no baseline binary: it is the only thing
        # that reports at all when every pair is unsupported.
        #
        # It is also intrinsically host-sensitive. It compares a wall time on
        # whatever machine is running against a constant calibrated on a
        # different one, so no multiplier is simultaneously tight enough to mean
        # something and loose enough to be safe everywhere. A measured host ran
        # 2.4x the reference figures and left as little as 1.2x headroom over
        # its OWN pre-cutover cost; see
        # docs/notes/pre-cutover-baseline-binary.md.
        #
        # When a distinct historical baseline IS supplied, the role-vs-role pair
        # comparison above is available and is host-independent by construction
        # — both roles run on the same machine in the same run. The absolute
        # budget then adds no evidence the pairs do not already carry, and can
        # only contribute a false failure on a slow host. So it is reported as
        # `advisory` and does not decide the verdict. Without a baseline it
        # remains the hard gate it was designed to be.
        budget = workload.get("cold_wall_seconds")
        if (
            scenario == "cold"
            and budget is not None
            and row.get("wall", {}).get("median") is not None
            and row["wall"]["median"] > budget * 1000
        ):
            over_budget = {
                "reason": f"cold wall time exceeds the {budget:g}-second budget for {workload['name']}",
                "budget_seconds": budget,
                "observed_seconds": row["wall"]["median"] / 1000,
            }
            if baseline_available:
                over_budget["status"] = "advisory"
                over_budget["advisory_because"] = (
                    "a distinct historical baseline is present, so the host-independent "
                    "pair comparison decides this scenario"
                )
            else:
                over_budget["status"] = "fail"
            pairs.setdefault(role, over_budget)
    statuses = [value.get("status") for value in pairs.values()]
    if "fail" in statuses:
        status = "fail"
    elif "indeterminate" in statuses or (required and "unsupported" in statuses):
        status = "indeterminate"
    # `advisory` is an observation, never a verdict: it neither fails a scenario
    # nor manufactures a pass for one that has no passing evidence of its own.
    elif any(value == "pass" for value in statuses):
        status = "pass"
    else:
        status = "unsupported"
    return {"status": status, "required": required, "pairs": pairs}


def role_metadata(role: str, binary: Path, source_dir: Path | None) -> dict[str, Any]:
    if source_dir is None:
        raise ValueError(
            f"{role} requires explicit --source-dir provenance; the runner never defaults a role to the candidate checkout"
        )
    if not source_dir.is_dir():
        raise ValueError(f"{role} source provenance directory does not exist: {source_dir}")
    source = source_provenance(source_dir)
    if source["commit"] is None or source["source_tree"] is None:
        raise ValueError(f"{role} source provenance is not a verifiable Git checkout: {source_dir}")
    binary_hash = sha256_file(binary) if binary.is_file() else None
    return {
        "role": role,
        "label": ROLE_LABELS[role],
        "binary": str(binary),
        "binary_sha256": binary_hash,
        "build_provenance": {
            "source_directory": str(source_dir),
            "source_commit": source["commit"],
            "source_tree": source["source_tree"],
            "binary_sha256": binary_hash,
        },
        **source,
    }


def run_audit(args: argparse.Namespace) -> dict[str, Any]:
    root = Path(__file__).resolve().parent.parent
    audit_manifest = load_audit_manifest(root)
    thresholds = audit_manifest["thresholds"]
    scenario_policy = audit_manifest["scenario_policy"]
    manifest_workloads = {item["name"]: item for item in audit_manifest["workload"]}
    if args.warmup != audit_manifest["protocol"]["warmup"] or args.iterations != audit_manifest["protocol"]["paired_samples"]:
        raise ValueError("CLI sampling counts drifted from the validated audit manifest")
    perf_baseline = load_perf_baseline(root)
    binaries: dict[str, Path] = args.binaries
    for role, binary in binaries.items():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise RuntimeError(f"{role} binary is not executable: {binary}")
    session_bins = args.session_bins
    scaling_bins = args.scaling_bins
    for role, binary in {**session_bins, **scaling_bins}.items():
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise RuntimeError(f"{role} benchmark binary is not executable: {binary}")
    report: dict[str, Any] = {
        "schema_version": 2,
        "protocol": {
            "name": "rue_incrementality_value_audit",
            "measurement_profile": "release",
            "warmup": args.warmup,
            "paired_samples": args.iterations,
            "pair_order": audit_manifest["protocol"]["pair_order"],
            "source_of_cold_timing": "scripts/perf-baseline.py-compatible benchmark JSON, black-box fallback for old revisions",
            "source_of_warm_timing": "rue-compiler-session-bench opaque scenario JSON",
            "thresholds": thresholds,
            "scenario_policy": scenario_policy,
            "caldera_timeout_headroom_seconds": audit_manifest["protocol"]["caldera_timeout_headroom_seconds"],
        },
        "provenance": {
            "created_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "host": host_provenance(),
            "repository": str(root),
            "manifest": str(root / "benchmarks/manifest.toml"),
            "audit_manifest": str(root / "benchmarks/value-audit/manifest.toml"),
            "audit_manifest_sha256": sha256_file(root / "benchmarks/value-audit/manifest.toml"),
        },
        "revisions": {
            role: role_metadata(role, binaries[role], args.source_dirs.get(role))
            for role in ROLES
        },
        "comparison_provenance": {},
        "workloads": [],
        "baseline_selection": {
            "recommendation": "current_production",
            "reason": "candidate value is measured against current production; historical baseline is context and may require the revision-compatible black-box fallback",
        },
        "fixture_protocol": audit_manifest["protocol"],
        "verdict": "pass",
    }
    binary_hashes = {
        role: report["revisions"][role]["binary_sha256"] for role in ROLES
    }
    # A distinct historical binary is exactly what makes the pair comparison
    # real, so the same test that classifies the run also decides whether the
    # absolute cold budget is a gate or an advisory.
    baseline_available = (
        binary_hashes["historical_baseline"] is not None
        and binary_hashes["historical_baseline"] != binary_hashes["current_production"]
    )
    if len(set(binary_hashes.values())) == 1:
        report["comparison_provenance"] = {
            "classification": "same_binary_protocol_smoke",
            "historical_comparison_valid": False,
            "claim_scope": "protocol and fail-closed gate smoke only; no historical/current/candidate value claim",
        }
    else:
        report["comparison_provenance"] = {
            "classification": "three_role_binary_comparison",
            "historical_comparison_valid": True,
            "claim_scope": "role comparisons are linked to explicit source/build provenance and binary hashes",
        }
    report["comparison_provenance"]["absolute_cold_budget"] = (
        "advisory; the host-independent pair comparison decides cold scenarios"
        if baseline_available
        else "enforced; no distinct historical baseline binary was supplied"
    )
    statuses: list[str] = []
    selected = set(args.workloads) if args.workloads else {workload["name"] for workload in WORKLOADS}
    for workload in WORKLOADS:
        if workload["name"] not in selected:
            continue
        root_source = root / workload["root"]
        if not root_source.is_file():
            raise RuntimeError(f"workload root is missing: {root_source}")
        row: dict[str, Any] = {
            **workload,
            "supported_scenarios": manifest_workloads[workload["name"]]["supported_scenarios"],
            "root": str(root_source),
            "source_sha256": sha256_paths(
                tracked_files(root_source.parent) if workload["family"] in DIRECTORY_ROOTED_FAMILIES else [root_source],
                root,
            ),
            "scenarios": {},
            "scaling_evidence": {},
        }
        with tempfile.TemporaryDirectory(prefix="rue-value-audit-") as temp:
            workdir = Path(temp)
            for scenario in SCENARIOS:
                timeout = args.timeout
                if scenario == "cold" and workload.get("cold_timeout_seconds") is not None:
                    timeout = max(timeout, workload["cold_timeout_seconds"])
                role_rows: dict[str, dict[str, Any]] = {}
                for pair_index in range(args.iterations):
                    order = list(ROLES) if pair_index % 2 == 0 else list(reversed(ROLES))
                    for role in order:
                        if role in role_rows and len(role_rows[role].get("wall_samples", [])) > pair_index:
                            continue
                        role_rows.setdefault(role, {
                            "status": "pass",
                            "wall_samples": [],
                            "rss_samples": [],
                            "details": [],
                            "binary_sha256": report["revisions"][role]["binary_sha256"],
                        })
                        if scenario == "cold":
                            if pair_index == 0:
                                for _ in range(audit_manifest["protocol"]["warmup"]):
                                    cold_sample(
                                        binaries[role], root_source, root, timeout, args.jobs,
                                        workdir, perf_baseline,
                                    )
                            sample = cold_sample(
                                binaries[role], root_source, root, timeout, args.jobs, workdir,
                                perf_baseline,
                            )
                        elif scenario == "repeated_edit_memory":
                            sample = {"status": "unsupported", "reason": "existing reused benchmark has no bounded repeated-edit memory protocol"}
                        elif role not in session_bins:
                            sample = {"status": "unsupported", "reason": "no compatible session benchmark binary supplied for this revision"}
                        elif scenario not in manifest_workloads[workload["name"]]["supported_scenarios"]:
                            sample = {"status": "unsupported", "reason": "scenario is not declared by the workload in the audit manifest"}
                        elif workload["family"] not in SESSION_ROW_NAMES or scenario not in SESSION_ROW_NAMES[workload["family"]]:
                            sample = {"status": "unsupported", "reason": "scenario is not implemented by the canonical runner for this workload"}
                        else:
                            if pair_index == 0:
                                for _ in range(audit_manifest["protocol"]["warmup"]):
                                    session_sample(
                                        session_bins[role], workload, scenario, timeout, root,
                                        audit_manifest["locality"][scenario],
                                    )
                            sample = session_sample(
                                session_bins[role], workload, scenario, timeout, root,
                                audit_manifest["locality"][scenario],
                            )
                        if (
                            workload["family"] == "synthetic"
                            and scenario == "warm_leaf_body"
                            and role in scaling_bins
                        ):
                            if pair_index == 0:
                                scaling_sample(scaling_bins[role], True, timeout, root)
                            scaling = scaling_sample(scaling_bins[role], True, timeout, root)
                            row["scaling_evidence"].setdefault(role, []).append(scaling)
                        merge_role_sample(role_rows[role], sample, scenario)
                for role, values in list(role_rows.items()):
                    if values.get("status") == "unsupported":
                        continue
                    # Null RSS is deliberately omitted rather than converted to
                    # zero; platform memory availability is an explicit fact.
                    values["wall"] = median_mad(values.pop("wall_samples"))
                    values["rss"] = median_mad([
                        float(value) for value in values.pop("rss_samples") if value is not None
                    ])
                    protocols = sorted({
                        detail.get("protocol")
                        for detail in values.get("details", [])
                        if detail.get("protocol") is not None
                    })
                    if len(protocols) != 1:
                        values["status"] = "indeterminate"
                        values["protocol"] = protocols
                    else:
                        values["protocol"] = protocols[0]
                for role, values in role_rows.items():
                    if values.get("status") == "unsupported":
                        continue
                    output_hashes = [
                        detail.get("output_sha256")
                        for detail in values.get("details", [])
                        if detail.get("output_sha256") is not None
                    ]
                    if output_hashes and len(set(output_hashes)) != 1:
                        values["status"] = "fail"
                        values["correctness"] = {"status": "fail", "reason": "output artifact changed between paired samples"}
                    else:
                        values["correctness"] = {"status": "pass", "criterion": "exit_zero_and_output_present"}
                verdict = scenario_verdict(
                    scenario,
                    role_rows,
                    row,
                    scenario_policy[scenario],
                    thresholds,
                    baseline_available,
                )
                row["scenarios"][scenario] = {"roles": role_rows, "verdict": verdict}
                statuses.append(verdict["status"])
        report["workloads"].append(row)
    for workload in report["workloads"]:
        for role, samples in workload["scaling_evidence"].items():
            workload["scaling_evidence"][role] = {
                "warm_leaf_body_pre_link_ms": median_mad(
                    [sample["wall_ms"] for sample in samples]
                ),
                "rss_bytes": median_mad(
                    [float(sample["peak_rss_bytes"]) for sample in samples if sample.get("peak_rss_bytes") is not None]
                ),
                "runner": "rue-scaling-bench",
            }
    required_statuses = [
        status
        for workload in report["workloads"]
        for scenario, value in workload["scenarios"].items()
        if value["verdict"].get("required")
        and value["verdict"].get("status") not in {"not_applicable"}
        for status in [value["verdict"]["status"]]
    ]
    if "fail" in statuses or "fail" in required_statuses:
        report["verdict"] = "fail"
    elif "indeterminate" in statuses or "indeterminate" in required_statuses:
        report["verdict"] = "indeterminate"
    elif not statuses or all(status in {"unsupported", "not_applicable"} for status in statuses):
        report["verdict"] = "unsupported"
    return report


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--historical-baseline", required=True, type=Path)
    parser.add_argument("--current", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--source-dir", action="append", default=[], metavar="ROLE=PATH")
    parser.add_argument("--session-bench", action="append", default=[], metavar="ROLE=PATH")
    parser.add_argument("--scaling-bench", action="append", default=[], metavar="ROLE=PATH")
    parser.add_argument("--output", type=Path, default=Path("value-audit.json"))
    parser.add_argument("--iterations", type=int, default=7)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--jobs", type=int, default=1)
    parser.add_argument("--workload", action="append", dest="workloads", choices=[workload["name"] for workload in WORKLOADS])
    args = parser.parse_args(argv)
    if args.iterations != 7:
        parser.error("--iterations is fixed at 7 by the precommitted audit protocol")
    if args.warmup != 1:
        parser.error("--warmup is fixed at 1 by the precommitted audit protocol")
    if args.timeout <= 0 or args.jobs < 0:
        parser.error("--timeout must be positive and --jobs must be non-negative")
    args.binaries = {
        "historical_baseline": args.historical_baseline.expanduser().resolve(),
        "current_production": args.current.expanduser().resolve(),
        "candidate": args.candidate.expanduser().resolve(),
    }
    args.source_dirs = parse_role_mapping(args.source_dir, "--source-dir")
    if set(args.source_dirs) != set(ROLES):
        parser.error("--source-dir is required once for every role; provenance is never inferred")
    args.session_bins = parse_role_mapping(args.session_bench, "--session-bench")
    args.scaling_bins = parse_role_mapping(args.scaling_bench, "--scaling-bench")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    try:
        report = run_audit(args)
    except (OSError, RuntimeError, ValueError) as error:
        print(f"rue-value-audit: {error}", file=sys.stderr)
        return 2
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"output": str(args.output.resolve()), "verdict": report["verdict"]}))
    return 0 if report["verdict"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
