"""Versioned benchmark annotations and derived measurement-boundary events."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import tomllib
from pathlib import Path


SCHEMA_VERSION = 1
CATEGORIES = {
    "capability",
    "optimization",
    "benchmark_corpus",
    "measurement_infrastructure",
    "known_regression",
    "repair",
}
METRICS = {"latency", "throughput", "memory", "binary_size", "*"}
REPOSITORY = "https://github.com/rue-language/rue"
LINK_RE = re.compile(rf"{re.escape(REPOSITORY)}/(?:issues|pull)/[1-9][0-9]*$")
COMMIT_RE = re.compile(r"[0-9a-f]{7,40}$")
SCOPE_IDENTIFIER_RE = re.compile(r"[a-z0-9]+(?:[._-][a-z0-9]+)*$")
DEFAULT_ANNOTATIONS = Path(__file__).resolve().parent.parent / "benchmarks" / "annotations.toml"


class GitCommitResolver:
    """Resolve and order annotation commits against the authoritative repository."""

    def __init__(self, repository: Path):
        self.repository = repository

    def canonical(self, commit: str) -> str:
        process = subprocess.run(
            ["git", "rev-parse", "--verify", f"{commit}^{{commit}}"],
            cwd=self.repository,
            text=True,
            capture_output=True,
            check=False,
        )
        value = process.stdout.strip()
        if process.returncode != 0 or not re.fullmatch(r"[0-9a-f]{40}", value):
            raise ValueError(f"unknown annotation commit: {commit}")
        return value

    def is_ancestor(self, start: str, end: str) -> bool:
        return subprocess.run(
            ["git", "merge-base", "--is-ancestor", start, end],
            cwd=self.repository,
            capture_output=True,
            check=False,
        ).returncode == 0

    def timestamp(self, commit: str) -> int:
        process = subprocess.run(
            ["git", "show", "-s", "--format=%ct", commit],
            cwd=self.repository,
            text=True,
            capture_output=True,
            check=False,
        )
        if process.returncode != 0 or not process.stdout.strip().isdigit():
            raise ValueError(f"unknown annotation commit: {commit}")
        return int(process.stdout.strip())


def load_annotations(path: Path = DEFAULT_ANNOTATIONS) -> list[dict]:
    """Load authored annotations; a missing file is valid legacy input."""
    if not path.exists():
        return []
    value = tomllib.loads(path.read_text())
    if value.get("version") != SCHEMA_VERSION:
        raise ValueError(f"benchmark annotation schema version must be {SCHEMA_VERSION}")
    unknown = set(value) - {"version", "annotation"}
    if unknown:
        raise ValueError(f"unknown benchmark annotation document fields: {', '.join(sorted(unknown))}")
    annotations = value.get("annotation", [])
    if not isinstance(annotations, list) or any(not isinstance(item, dict) for item in annotations):
        raise ValueError("benchmark annotations must be an array of tables")
    return annotations


def _text(item: dict, field: str, maximum: int) -> str:
    value = item.get(field)
    if not isinstance(value, str) or not value.strip() or value != value.strip() or len(value) > maximum:
        raise ValueError(f"annotation {field} must be non-empty, trimmed, and at most {maximum} characters")
    return value


def _scope(value: object) -> dict:
    if not isinstance(value, dict) or set(value) != {"metrics", "workloads", "platforms"}:
        raise ValueError("annotation scope must contain only metrics, workloads, and platforms")
    result = {}
    for field in ("metrics", "workloads", "platforms"):
        entries = value.get(field)
        if (
            not isinstance(entries, list)
            or not entries
            or any(
                not isinstance(entry, str)
                or not entry
                or entry != entry.strip()
                or (entry != "*" and not SCOPE_IDENTIFIER_RE.fullmatch(entry))
                for entry in entries
            )
            or len(entries) != len(set(entries))
        ):
            raise ValueError(f"annotation scope {field} must be a non-empty unique string list")
        result[field] = sorted(entries)
        if "*" in result[field] and len(result[field]) != 1:
            raise ValueError(f"annotation scope {field} cannot combine * with named values")
    if any(metric not in METRICS for metric in result["metrics"]):
        raise ValueError("annotation scope contains an invalid metric family")
    return result


def _location(item: dict, resolver) -> dict:
    has_commit = "commit" in item
    has_range = "start_commit" in item or "end_commit" in item
    if has_commit == has_range:
        raise ValueError("annotation must specify exactly one commit or one commit range")
    fields = ["commit"] if has_commit else ["start_commit", "end_commit"]
    if any(not isinstance(item.get(field), str) or not COMMIT_RE.fullmatch(item[field]) for field in fields):
        raise ValueError("annotation commit keys must be 7-40 lowercase hexadecimal characters")
    commits = [resolver.canonical(item[field]) for field in fields]
    if has_commit:
        return {"kind": "commit", "commit": commits[0]}
    if not resolver.is_ancestor(commits[0], commits[1]):
        raise ValueError("annotation range start must be an ancestor of its end")
    return {"kind": "range", "start_commit": commits[0], "end_commit": commits[1]}


def _event_id(event: dict) -> str:
    encoded = json.dumps(event, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def validate_authored_annotations(annotations: list[dict], resolver) -> list[dict]:
    """Validate and normalize checked-in annotations or raise ValueError."""
    normalized = []
    identities = {}
    allowed = {
        "commit", "start_commit", "end_commit", "category", "title",
        "explanation", "link", "scope", "comparability_note",
    }
    for index, item in enumerate(annotations, 1):
        unknown = set(item) - allowed
        if unknown:
            raise ValueError(f"annotation #{index} has unknown fields: {', '.join(sorted(unknown))}")
        category = item.get("category")
        if category not in CATEGORIES:
            raise ValueError(f"annotation #{index} has invalid category: {category}")
        title = _text(item, "title", 100)
        explanation = _text(item, "explanation", 500)
        link = item.get("link")
        if not isinstance(link, str) or not LINK_RE.fullmatch(link):
            raise ValueError(f"annotation #{index} must link to a Rue issue or pull request")
        event = {
            "source": "authored",
            "location": _location(item, resolver),
            "category": category,
            "title": title,
            "explanation": explanation,
            "link": link,
            "scope": _scope(item.get("scope")),
        }
        if "comparability_note" in item:
            event["comparability_note"] = _text(item, "comparability_note", 300)
        identity = (
            json.dumps(event["location"], sort_keys=True),
            event["category"],
            event["title"],
        )
        serialized = json.dumps(event, sort_keys=True, separators=(",", ":"))
        if identity in identities:
            kind = "duplicate" if identities[identity] == serialized else "conflicting"
            raise ValueError(f"{kind} benchmark annotation event: {title}")
        identities[identity] = serialized
        event["id"] = _event_id(event)
        normalized.append(event)
    return normalized


def _regime(run: dict) -> dict | None:
    publication = run.get("publication")
    regime = publication.get("regime") if isinstance(publication, dict) else None
    return regime if isinstance(regime, dict) else None


def derived_identity_annotations(runs: list[dict], resolver) -> list[dict]:
    """Derive an event for every durable measurement-identity transition."""
    events = []
    for previous, current in zip(runs, runs[1:]):
        before, after = _regime(previous), _regime(current)
        if before is None and after is None:
            continue
        if before == after:
            continue
        before_dimensions = before or {}
        after_dimensions = after or {}
        changed = sorted(set(before_dimensions) | set(after_dimensions), key=str)
        changed = [
            field
            for field in changed
            if before_dimensions.get(field) != after_dimensions.get(field)
        ]
        raw_commit = current.get("commit")
        if not isinstance(raw_commit, str) or not raw_commit:
            raise ValueError("measurement identity change has no commit")
        commit = resolver.canonical(raw_commit)
        platform = after_dimensions.get("platform", before_dimensions.get("platform", "*"))
        before_state = "explicit" if before is not None else "unknown"
        after_state = "explicit" if after is not None else "unknown"
        explanation = (
            f"Durable measurement identity changed from {before_state} to {after_state}: "
            + ", ".join(changed)
            + "."
        )
        event = {
            "source": "derived",
            "location": {"kind": "commit", "commit": commit},
            "category": "measurement_infrastructure",
            "title": "Benchmark measurement identity changed",
            "explanation": explanation,
            "link": f"{REPOSITORY}/commit/{commit}",
            "scope": _scope({
                "metrics": ["*"],
                "workloads": ["*"],
                "platforms": [platform],
            }),
            "comparability_note": "A comparability boundary occurs at this measurement.",
            "changed_dimensions": changed,
            "identity_change": {
                "before": {"status": before_state, "dimensions": before},
                "after": {"status": after_state, "dimensions": after},
            },
            "dimension_changes": {
                field: {
                    "before": before_dimensions.get(field),
                    "after": after_dimensions.get(field),
                }
                for field in changed
            },
        }
        event["id"] = _event_id(event)
        events.append(event)
    return events


def normalized_annotation_stream(
    histories: list[list[dict]], authored: list[dict], resolver
) -> list[dict]:
    """Return one deterministic stream shared by every benchmark data product."""
    events = validate_authored_annotations(authored, resolver)
    for runs in histories:
        events.extend(derived_identity_annotations(runs, resolver))

    # The same automatic transition can appear in multiple products. Event IDs
    # make their union stable without hiding distinct platform-scoped changes.
    events = list({event["id"]: event for event in events}.values())

    def sort_key(event: dict) -> tuple:
        location = event["location"]
        commit = location.get("commit", location.get("start_commit"))
        return (
            resolver.timestamp(commit),
            commit,
            event["category"],
            event["title"],
            event["source"],
            event["id"],
        )

    return sorted(events, key=sort_key)


def normalized_annotations(runs: list[dict], authored: list[dict], resolver) -> list[dict]:
    return normalized_annotation_stream([runs], authored, resolver)
