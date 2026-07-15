"""Durable benchmark-history storage and publication metadata.

Version 3 histories are directories. Their index points at immutable per-run
shards partitioned into calendar-month directories, so history grows without
rewriting or truncating old measurements.
The index is replaced last: readers see either the old complete snapshot or
the new complete snapshot.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Callable


HISTORY_VERSION = 3
FORMAT = "rue-benchmark-history"
UNKNOWN_REGIME = "unknown"


def run_regime(run: dict) -> str:
    publication = run.get("publication")
    if isinstance(publication, dict) and publication.get("comparable") is True:
        value = publication.get("regime_id")
        if isinstance(value, str) and value:
            return value
    return UNKNOWN_REGIME


def comparison_break(previous: dict | None, current: dict) -> bool:
    """Return whether consumers must not compare adjacent measurements."""
    current_regime = run_regime(current)
    if previous is None or current_regime == UNKNOWN_REGIME or run_regime(previous) != current_regime:
        return True
    publication = current.get("publication", {})
    coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
    return coverage.get("gap_unknown") is True or bool(coverage.get("skipped_commits"))


def latest_comparable_segment(runs: list[dict]) -> list[dict]:
    """Return the latest contiguous regime, never crossing a comparison break."""
    if not runs:
        return []
    start = len(runs) - 1
    while start > 0 and not comparison_break(runs[start - 1], runs[start]):
        start -= 1
    return runs[start:]


def _json_bytes(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def atomic_write_json(path: Path, value: object) -> None:
    """Durably replace one JSON file without exposing a partial write."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(_json_bytes(value))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    except BaseException:
        try:
            os.unlink(name)
        except FileNotFoundError:
            pass
        raise


def legacy_history(path: Path) -> list[dict]:
    if not path.exists():
        return []
    value = json.loads(path.read_text())
    runs = value if isinstance(value, list) else value.get("runs") if isinstance(value, dict) else None
    if not isinstance(runs, list) or any(not isinstance(run, dict) for run in runs):
        raise ValueError(f"malformed legacy benchmark history: {path}")
    return runs


def mark_legacy(run: dict) -> dict:
    migrated = dict(run)
    migrated["publication"] = {
        "schema_version": HISTORY_VERSION,
        "comparable": False,
        "regime_id": UNKNOWN_REGIME,
        "legacy_metadata": "unknown",
        "coverage": {
            "measured_commit": run.get("commit"),
            "represented_commits": [],
            "skipped_commits": [],
            "gap_unknown": True,
        },
    }
    return migrated


def load_history(path: Path) -> dict:
    """Read a v3 directory or either legacy single-file representation."""
    if path.is_file() or (not path.exists() and path.suffix == ".json"):
        return {"version": 1, "runs": legacy_history(path)}
    index_path = path / "index.json"
    if not index_path.exists():
        return {"version": HISTORY_VERSION, "format": FORMAT, "runs": []}
    index = json.loads(index_path.read_text())
    if not isinstance(index, dict) or index.get("version") != HISTORY_VERSION or index.get("format") != FORMAT:
        raise ValueError(f"malformed benchmark history index: {index_path}")
    shards = index.get("shards")
    if not isinstance(shards, list):
        raise ValueError(f"benchmark history index has no shards array: {index_path}")
    runs: list[dict] = []
    for entry in shards:
        relative = entry.get("path") if isinstance(entry, dict) else None
        if not isinstance(relative, str) or Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise ValueError(f"invalid benchmark shard path in {index_path}")
        shard = json.loads((path / relative).read_text())
        shard_runs = shard.get("runs") if isinstance(shard, dict) else None
        if not isinstance(shard, dict) or shard.get("version") != HISTORY_VERSION or shard.get("platform") != index.get("platform"):
            raise ValueError(f"benchmark history shard metadata mismatch: {relative}")
        if not isinstance(shard_runs, list) or any(not isinstance(run, dict) for run in shard_runs):
            raise ValueError(f"malformed benchmark history shard: {relative}")
        if entry.get("run_count") != 1 or len(shard_runs) != 1:
            raise ValueError(f"benchmark history shard count mismatch: {relative}")
        timestamp = shard_runs[0].get("timestamp")
        if entry.get("first_timestamp") != timestamp or entry.get("last_timestamp") != timestamp:
            raise ValueError(f"benchmark history shard timestamp mismatch: {relative}")
        expected_digest = hashlib.sha256(_json_bytes(shard)).hexdigest()
        if Path(relative).name != f"{expected_digest}.json":
            raise ValueError(f"benchmark history shard digest mismatch: {relative}")
        if str(Path(relative).parent) != _month(shard_runs[0]):
            raise ValueError(f"benchmark history shard partition mismatch: {relative}")
        runs.extend(shard_runs)
    if index.get("run_count") != len(runs):
        raise ValueError(f"benchmark history index run count mismatch: {index_path}")
    return {**index, "runs": runs}


def _month(run: dict) -> str:
    timestamp = run.get("timestamp")
    if not isinstance(timestamp, str) or len(timestamp) < 8 or timestamp[4] != "-" or timestamp[7] != "-":
        raise ValueError("benchmark timestamp must begin with YYYY-MM")
    year, month = timestamp[:7].split("-")
    if not (year.isdigit() and month.isdigit() and 1 <= int(month) <= 12):
        raise ValueError("benchmark timestamp must begin with YYYY-MM")
    return f"shards/{year}/{month}"


def save_history(path: Path, runs: list[dict], platform: str) -> None:
    """Write all shards, then atomically publish their index."""
    entries = []
    for run in runs:
        partition = _month(run)
        shard_runs = [run]
        payload = {"version": HISTORY_VERSION, "platform": platform, "runs": shard_runs}
        # Shards are content-addressed and immutable. Replacing the index is
        # therefore the only visibility boundary for a new snapshot.
        shard_hash = hashlib.sha256(_json_bytes(payload)).hexdigest()
        relative = f"{partition}/{shard_hash}.json"
        if not (path / relative).exists():
            atomic_write_json(path / relative, payload)
        entries.append({
            "path": relative,
            "run_count": len(shard_runs),
            "first_timestamp": shard_runs[0].get("timestamp"),
            "last_timestamp": shard_runs[-1].get("timestamp"),
        })
    atomic_write_json(path / "index.json", {
        "version": HISTORY_VERSION,
        "format": FORMAT,
        "platform": platform,
        "run_count": len(runs),
        "shards": entries,
    })


def corpus_metadata(manifest: Path) -> dict:
    """Hash the manifest and every declared source file in manifest order."""
    import tomllib
    raw = manifest.read_bytes()
    parsed = tomllib.loads(raw.decode())
    digest = hashlib.sha256()
    digest.update(b"manifest\0" + raw)
    files = []
    for entry in parsed.get("benchmark", []):
        relative = entry.get("path")
        if not isinstance(relative, str):
            raise ValueError("benchmark manifest entry has no path")
        source = manifest.parent / relative
        contents = source.read_bytes()
        digest.update(relative.encode() + b"\0" + contents)
        files.append(relative)
    return {"schema_version": 1, "sha256": digest.hexdigest(), "files": files}


def regime_metadata(result: dict, platform: str, runner_image: str, corpus: dict) -> dict:
    timing_versions = sorted({bench.get("timing_schema_version", 1) for bench in result["benchmarks"]})
    regime = {
        "platform": platform,
        "corpus_schema_version": corpus["schema_version"],
        "corpus_sha256": corpus["sha256"],
        "timing_schema_versions": timing_versions,
        "build_mode": result.get("build_mode"),
        "iteration_policy": {"kind": "fixed", "iterations": result.get("iterations")},
        "runner_image": runner_image,
        "measurement_family": result.get("measurement_family", "compiler"),
        "scenario_family": result.get("scenario_family", "cold_compilation"),
    }
    encoded = json.dumps(regime, sort_keys=True, separators=(",", ":")).encode()
    return {"id": hashlib.sha256(encoded).hexdigest(), "dimensions": regime}


def git_coverage(previous: str | None, current: str, repo: Path, runner: Callable[..., subprocess.CompletedProcess] = subprocess.run) -> dict:
    represented = [current]
    skipped: list[str] = []
    gap_unknown = previous != current
    if previous and previous != current:
        ancestor = runner(
            ["git", "merge-base", "--is-ancestor", previous, current],
            cwd=repo, text=True, capture_output=True, check=False,
        )
        if ancestor.returncode == 0:
            process = runner(
                ["git", "rev-list", "--reverse", f"{previous}..{current}"],
                cwd=repo, text=True, capture_output=True, check=False,
            )
            if process.returncode == 0:
                commits = [line for line in process.stdout.splitlines() if line]
                if commits and commits[-1] == current:
                    skipped = commits[:-1]
                    gap_unknown = False
    return {
        "measured_commit": current,
        "represented_commits": represented,
        "skipped_commits": skipped,
        "gap_unknown": gap_unknown,
    }


def validate_publication(run: dict) -> list[str]:
    errors = []
    publication = run.get("publication")
    if not isinstance(publication, dict):
        return ["publication metadata must be an object"]
    required = ("schema_version", "comparable", "regime_id", "regime", "corpus", "timing_schema_versions", "build_mode", "compiler", "iteration_policy", "platform", "runner", "trigger_reason", "coverage", "metric_semantics")
    for name in required:
        if name not in publication:
            errors.append(f"publication metadata missing {name}")
    if publication.get("schema_version") != HISTORY_VERSION:
        errors.append("publication schema_version must be 3")
    if publication.get("comparable") is not True:
        errors.append("new benchmark run must be comparable")
    coverage = publication.get("coverage")
    if isinstance(coverage, dict):
        commit = run.get("commit")
        if coverage.get("measured_commit") != commit or coverage.get("represented_commits") != [commit]:
            errors.append("coverage must identify only the measured commit as represented")
        if not isinstance(coverage.get("skipped_commits"), list) or not isinstance(coverage.get("gap_unknown"), bool):
            errors.append("coverage skipped_commits/gap_unknown are malformed")
    else:
        errors.append("publication coverage must be an object")
    if publication.get("regime_id") == UNKNOWN_REGIME:
        errors.append("new benchmark run cannot use the unknown regime")
    regime = publication.get("regime")
    if isinstance(regime, dict):
        encoded = json.dumps(regime, sort_keys=True, separators=(",", ":")).encode()
        if publication.get("regime_id") != hashlib.sha256(encoded).hexdigest():
            errors.append("regime_id does not match regime dimensions")
    else:
        errors.append("publication regime must be an object")
    corpus = publication.get("corpus")
    if not isinstance(corpus, dict) or corpus.get("schema_version") != 1 or not isinstance(corpus.get("sha256"), str) or len(corpus.get("sha256", "")) != 64:
        errors.append("publication corpus metadata is malformed")
    compiler = publication.get("compiler")
    if not isinstance(compiler, dict) or compiler.get("commit") != run.get("commit") or not isinstance(compiler.get("identity"), str):
        errors.append("publication compiler identity is malformed")
    iterations = publication.get("iteration_policy")
    if not isinstance(iterations, dict) or iterations.get("kind") != "fixed" or iterations.get("iterations") != run.get("iterations"):
        errors.append("publication iteration policy is malformed")
    for field in ("platform", "build_mode", "trigger_reason"):
        if not isinstance(publication.get(field), str) or not publication[field]:
            errors.append(f"publication {field} is malformed")
    runner = publication.get("runner")
    if not isinstance(runner, dict) or not isinstance(runner.get("image"), str) or not runner.get("image"):
        errors.append("publication runner metadata is malformed")
    timing_versions = publication.get("timing_schema_versions")
    if not isinstance(timing_versions, list) or not timing_versions or any(not isinstance(value, int) or isinstance(value, bool) or value <= 0 for value in timing_versions):
        errors.append("publication timing schema metadata is malformed")
    semantics = publication.get("metric_semantics")
    if not isinstance(semantics, dict) or semantics != {
        "measurement_family": run.get("measurement_family", "compiler"),
        "scenario_family": run.get("scenario_family", "cold_compilation"),
    } or any(not isinstance(value, str) or not value for value in semantics.values()):
        errors.append("publication metric semantics are malformed")
    if isinstance(regime, dict):
        expected_dimensions = {
            "platform": publication.get("platform"),
            "corpus_schema_version": corpus.get("schema_version") if isinstance(corpus, dict) else None,
            "corpus_sha256": corpus.get("sha256") if isinstance(corpus, dict) else None,
            "timing_schema_versions": timing_versions,
            "build_mode": publication.get("build_mode"),
            "iteration_policy": publication.get("iteration_policy"),
            "runner_image": runner.get("image") if isinstance(runner, dict) else None,
            "measurement_family": semantics.get("measurement_family") if isinstance(semantics, dict) else None,
            "scenario_family": semantics.get("scenario_family") if isinstance(semantics, dict) else None,
        }
        if regime != expected_dimensions:
            errors.append("publication fields do not match regime dimensions")
    return errors
