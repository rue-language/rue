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
import hashlib
import importlib.util
import json
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
        "description": "Generated declaration-volume and completion locality corpus.",
    },
    {
        "name": "representative_multi_module",
        "family": "representative",
        "root": "benchmarks/scenarios/representative/main.rue",
        "session_mode": "representative",
        "description": "Tracked multi-module application-shaped fixture from benchmarks/manifest.toml.",
    },
    {
        "name": "medium_ordinary_rue",
        "family": "ordinary",
        "root": "examples/quicksort.rue",
        "description": "Ordinary medium Rue example; only fresh-driver scenarios are supported.",
    },
    {
        "name": "caldera",
        "family": "caldera",
        "root": "examples/caldera/main.rue",
        "description": "Large maintained application graph with a 300-second cold budget.",
    },
)

THRESHOLDS = {
    "warm_unrelated_declaration_improvement": 0.50,
    "warm_leaf_body_improvement": 0.25,
    "claimed_win_mad_multiplier": 3.0,
    "cold_regression_fraction": 0.02,
    "repeated_edit_rss_band": 0.05,
    "caldera_wall_seconds": 300.0,
}


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


def source_provenance(source_dir: Path | None, root: Path, workload_root: Path) -> dict[str, Any]:
    source_dir = (source_dir or root).resolve()
    commit = git_value(source_dir, "rev-parse", "HEAD")
    tree = git_value(source_dir, "rev-parse", "HEAD^{tree}")
    if commit is not None:
        tracked = git_value(source_dir, "ls-files", "-z")
        if tracked is not None:
            paths = [source_dir / item for item in tracked.split("\0") if item]
        else:
            paths = all_files(source_dir)
    else:
        paths = all_files(source_dir)
    # A workload hash is kept separate from the complete source-tree identity;
    # it makes an audit row reproducible even when unrelated repository files
    # change between two operators' runs.
    workload_paths = all_files(workload_root) if workload_root.is_dir() else [workload_root]
    return {
        "directory": str(source_dir),
        "commit": commit,
        "source_tree": tree,
        "source_files_sha256": sha256_paths(paths, source_dir),
        "workload_files_sha256": sha256_paths(workload_paths, root),
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
    if manifest.get("schema_version") != 1:
        raise ValueError("value-audit manifest schema_version must be 1")
    protocol = manifest.get("protocol")
    if not isinstance(protocol, dict) or protocol.get("paired_samples") != 7 or protocol.get("warmup") != 1:
        raise ValueError("value-audit manifest must pin one warmup and seven paired samples")
    if protocol.get("measurement_profile") != "release":
        raise ValueError("value-audit manifest must pin the release measurement profile")
    source_manifest = path.parent / str(manifest.get("source_manifest", ""))
    if source_manifest.resolve() != (root / "benchmarks" / "manifest.toml").resolve():
        raise ValueError("value-audit manifest must reuse benchmarks/manifest.toml")
    workloads = manifest.get("workload")
    expected = {workload["name"] for workload in WORKLOADS}
    if not isinstance(workloads, list) or {item.get("name") for item in workloads} != expected:
        raise ValueError("value-audit manifest workload matrix drifted from the runner")
    return manifest


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
        match = re.search(r"maximum resident set size:\s*(\d+)", stderr)
        return int(match.group(1)) if match else None
    if style == "gnu":
        match = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr)
        return int(match.group(1)) * 1024 if match else None
    return None


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
) -> dict[str, Any]:
    output = workdir / "audit-output"
    output.unlink(missing_ok=True)
    env = os.environ.copy()
    env.pop("RUST_LOG", None)
    env["RUE_STD_PATH"] = str(root / "std")
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
        perf_baseline = load_perf_baseline(root)
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


def locality_check(scenario: str, row: dict[str, Any]) -> dict[str, Any]:
    work = row.get("required_vs_reused_work")
    if not isinstance(work, dict):
        return {"status": "unsupported", "reason": "session runner omitted locality evidence"}

    def count(artifact: str, side: str) -> Any:
        return work.get(artifact, {}).get(side)

    if scenario == "warm_no_op":
        passed = count("semantic_queries", "required") == 0 and count("semantic_queries", "reused") >= 1
    elif scenario == "warm_unrelated_declaration":
        passed = (
            count("semantic_bodies", "required") == 0
            and count("cfgs", "required") == 0
            and count("semantic_bodies", "reused") >= 1
            and count("cfgs", "reused") >= 1
        )
    elif scenario == "warm_leaf_body":
        passed = count("semantic_bodies", "required") >= 1 and count("cfgs", "reused") >= 1
    else:
        passed = count("modules", "required") >= 1 or count("semantic_bodies", "required") >= 1
    return {"status": "pass" if passed else "fail", "work": work}


def session_sample(
    session_binary: Path,
    workload: dict[str, Any],
    scenario: str,
    timeout: float,
    root: Path,
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
    result = run_command(command, timeout, {**os.environ, "RUE_STD_PATH": str(root / "std")})
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
    row = next((candidate for candidate in rows if candidate.get("name") == wanted), None)
    if row is None:
        raise RuntimeError(f"session benchmark did not report {wanted}")
    timing_ns = row.get("wall_time_ns")
    if not isinstance(timing_ns, int) or timing_ns <= 0:
        raise RuntimeError(f"session benchmark row {wanted} has no positive wall_time_ns")
    parity = row.get("differential_parity") or row.get("diagnostic_parity")
    exact = parity is not None
    locality = locality_check(scenario, row)
    return {
        "status": "pass" if exact and locality["status"] == "pass" else "fail",
        "wall_ms": timing_ns / 1_000_000,
        "peak_rss_bytes": result["peak_rss_bytes"],
        "runner_wall_ms": result["wall_ms"],
        "scenario_name": wanted,
        "exact_parity": parity,
        "locality": locality,
        "row": row,
    }


def scaling_sample(
    scaling_binary: Path,
    workload: dict[str, Any],
    warm: bool,
    timeout: float,
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
    result = run_command(command, timeout, os.environ.copy())
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


def pair_verdict(
    left: dict[str, Any],
    right: dict[str, Any],
    kind: str,
    threshold: float | None = None,
) -> dict[str, Any]:
    left_median = left.get("median")
    right_median = right.get("median")
    if left_median is None or right_median is None:
        return {"status": "unsupported", "reason": "one side has no samples"}
    left_mad = left.get("mad") or 0.0
    right_mad = right.get("mad") or 0.0
    noise = THRESHOLDS["claimed_win_mad_multiplier"] * max(left_mad, right_mad)
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
    allowed = max(THRESHOLDS["cold_regression_fraction"] * left_median, noise)
    return {
        "status": "pass" if delta <= allowed else "fail",
        "reason": "within cold wall/RSS regression budget" if delta <= allowed else "cold regression exceeds budget",
        "delta": delta,
        "allowed_regression": allowed,
        "noise_budget": noise,
        "threshold_fraction": THRESHOLDS["cold_regression_fraction"],
    }


def scenario_verdict(
    scenario: str,
    role_rows: dict[str, dict[str, Any]],
    workload: dict[str, Any],
) -> dict[str, Any]:
    pairs: dict[str, Any] = {}
    if scenario == "cold":
        kind = "regression"
        threshold = None
    elif scenario == "warm_unrelated_declaration":
        kind = "improvement"
        threshold = THRESHOLDS["warm_unrelated_declaration_improvement"]
    elif scenario == "warm_leaf_body":
        kind = "improvement"
        threshold = THRESHOLDS["warm_leaf_body_improvement"]
    else:
        kind = "observation"
        threshold = None
    for left_role, right_role in PAIR_NAMES:
        left = role_rows.get(left_role, {})
        right = role_rows.get(right_role, {})
        if left.get("status") == "unsupported" or right.get("status") == "unsupported":
            pairs[f"{left_role}_vs_{right_role}"] = {"status": "unsupported", "reason": "role lacks this protocol"}
            continue
        if left.get("status") == "fail" or right.get("status") == "fail":
            pairs[f"{left_role}_vs_{right_role}"] = {"status": "fail", "reason": "exact correctness or locality evidence failed"}
            continue
        if scenario == "repeated_edit_memory":
            pairs[f"{left_role}_vs_{right_role}"] = {"status": "unsupported", "reason": "no repeated-edit session protocol is exposed by the reused benchmark"}
            continue
        if kind == "observation":
            pair = {"status": "pass", "reason": "exact/locality observation recorded", "wall": {
                "left": left["wall"], "right": right["wall"]
            }}
        else:
            pair = pair_verdict(left["wall"], right["wall"], kind, threshold)
        if scenario == "cold":
            rss_left = left.get("rss", {})
            rss_right = right.get("rss", {})
            pair["rss"] = pair_verdict(rss_left, rss_right, "regression")
        pairs[f"{left_role}_vs_{right_role}"] = pair
    for role, row in role_rows.items():
        if row.get("status") == "fail":
            pairs.setdefault(role, {"status": "fail", "reason": "role evidence failed"})
        if (
            scenario == "cold"
            and workload["family"] == "caldera"
            and row.get("wall", {}).get("median") is not None
            and row["wall"]["median"] > THRESHOLDS["caldera_wall_seconds"] * 1000
        ):
            pairs.setdefault(role, {
                "status": "fail",
                "reason": "Caldera cold wall time exceeds the 300-second budget",
            })
    statuses = [value.get("status") for value in pairs.values()]
    if "fail" in statuses:
        status = "fail"
    elif "indeterminate" in statuses:
        status = "indeterminate"
    elif any(value == "pass" for value in statuses):
        status = "pass"
    else:
        status = "unsupported"
    return {"status": status, "pairs": pairs}


def role_metadata(role: str, binary: Path, source_dir: Path | None, root: Path) -> dict[str, Any]:
    source = source_provenance(source_dir, root, root)
    return {
        "role": role,
        "label": ROLE_LABELS[role],
        "binary": str(binary),
        "binary_sha256": sha256_file(binary) if binary.is_file() else None,
        **source,
    }


def run_audit(args: argparse.Namespace) -> dict[str, Any]:
    root = Path(__file__).resolve().parent.parent
    audit_manifest = load_audit_manifest(root)
    perf_baseline = load_perf_baseline(root)
    del perf_baseline  # Loading validates that this audit uses the canonical module.
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
        "schema_version": 1,
        "protocol": {
            "name": "rue_incrementality_value_audit",
            "measurement_profile": "release",
            "warmup": args.warmup,
            "paired_samples": args.iterations,
            "pair_order": "historical,current,candidate then candidate,current,historical alternating",
            "source_of_cold_timing": "scripts/perf-baseline.py-compatible benchmark JSON, black-box fallback for old revisions",
            "source_of_warm_timing": "rue-compiler-session-bench opaque scenario JSON",
            "thresholds": THRESHOLDS,
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
            role: role_metadata(role, binaries[role], args.source_dirs.get(role), root)
            for role in ROLES
        },
        "workloads": [],
        "baseline_selection": {
            "recommendation": "current_production",
            "reason": "candidate value is measured against current production; historical baseline is context and may require the revision-compatible black-box fallback",
        },
        "fixture_protocol": audit_manifest["protocol"],
        "verdict": "pass",
    }
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
            "root": str(root_source),
            "source_sha256": sha256_paths(
                all_files(root_source.parent) if workload["family"] in {"representative", "caldera"} else [root_source],
                root,
            ),
            "scenarios": {},
            "scaling_evidence": {},
        }
        with tempfile.TemporaryDirectory(prefix="rue-value-audit-") as temp:
            workdir = Path(temp)
            for scenario in SCENARIOS:
                role_rows: dict[str, dict[str, Any]] = {}
                for pair_index in range(args.iterations):
                    order = list(ROLES) if pair_index % 2 == 0 else list(reversed(ROLES))
                    for role in order:
                        if role in role_rows and len(role_rows[role].get("wall_samples", [])) > pair_index:
                            continue
                        role_rows.setdefault(role, {"status": "pass", "wall_samples": [], "rss_samples": [], "details": []})
                        if scenario == "cold":
                            sample = cold_sample(
                                binaries[role], root_source, root, args.timeout, args.jobs, workdir
                            )
                        elif scenario == "repeated_edit_memory":
                            sample = {"status": "unsupported", "reason": "existing reused benchmark has no bounded repeated-edit memory protocol"}
                        elif role not in session_bins:
                            sample = {"status": "unsupported", "reason": "no compatible session benchmark binary supplied for this revision"}
                        elif workload["family"] not in SESSION_ROW_NAMES or scenario not in SESSION_ROW_NAMES[workload["family"]]:
                            sample = {"status": "unsupported", "reason": "scenario is not implemented by the canonical runner for this workload"}
                        else:
                            if pair_index == 0:
                                # Exactly one unrecorded warmup per role and
                                # scenario, as required by the protocol.
                                session_sample(session_bins[role], workload, scenario, args.timeout, root)
                            sample = session_sample(session_bins[role], workload, scenario, args.timeout, root)
                        if (
                            workload["family"] == "synthetic"
                            and scenario == "warm_leaf_body"
                            and role in scaling_bins
                        ):
                            if pair_index == 0:
                                scaling_sample(scaling_bins[role], workload, True, args.timeout)
                            scaling = scaling_sample(scaling_bins[role], workload, True, args.timeout)
                            row["scaling_evidence"].setdefault(role, []).append(scaling)
                        if sample.get("status") == "unsupported":
                            role_rows[role] = sample
                            continue
                        if scenario == "cold":
                            sample["status"] = "pass"
                            role_rows[role]["wall_samples"].append(float(sample["wall_ms"]))
                            role_rows[role]["rss_samples"].append(sample.get("peak_rss_bytes"))
                        else:
                            role_rows[role]["wall_samples"].append(float(sample["wall_ms"]))
                            role_rows[role]["rss_samples"].append(sample.get("peak_rss_bytes"))
                            if sample.get("status") == "fail":
                                role_rows[role]["status"] = "fail"
                        role_rows[role]["details"].append(sample)
                for role, values in list(role_rows.items()):
                    if values.get("status") == "unsupported":
                        continue
                    # Null RSS is deliberately omitted rather than converted to
                    # zero; platform memory availability is an explicit fact.
                    values["wall"] = median_mad(values.pop("wall_samples"))
                    values["rss"] = median_mad([
                        float(value) for value in values.pop("rss_samples") if value is not None
                    ])
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
                verdict = scenario_verdict(scenario, role_rows, workload)
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
    if "fail" in statuses:
        report["verdict"] = "fail"
    elif "indeterminate" in statuses:
        report["verdict"] = "indeterminate"
    elif not statuses or all(status == "unsupported" for status in statuses):
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
