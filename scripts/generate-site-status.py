#!/usr/bin/env python3
"""Derive the homepage Field Report from the repository (RUE-1261).

The board reports three things about the project — where trunk is, how much of
the specification is traced to tests, and whether the compiler is getting
slower — and every one of them is computed here at site build time. None of it
is authored.

That property is the point. The board's claim is that this project can be
checked rather than taken on trust, so a hand-written number on it is not a
placeholder, it is a counterexample. It stayed hand-written for twelve days
after the legacy benchmark system was removed, reporting `commit 1234` and a
green `640 / 640`, while the launch post said the opposite.

Two rules follow from that, and both are load-bearing:

  Nothing is computed twice. Spec coverage arrives from
  `rue-spec --traceability --json`, the same report the traceability gate
  fails on; performance arrives from the derived dashboard data that
  `rue-bench derive` already produced. This script selects and formats. The
  previous generator counted `rule(cat=…)` markers itself, which is how the
  page came to claim a coverage figure the gate would not have agreed with.

  A figure that cannot be computed is absent, never guessed. A shallow clone
  has no commit count; a suite that has not collected yet has no index. The
  template renders what is present and says plainly what is missing.
"""

from __future__ import annotations

import argparse
import json
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

# The sparkline's viewBox, matching the markup in `index.html`.
SPARK_WIDTH, SPARK_HEIGHT = 300, 52
SPARK_TOP, SPARK_BOTTOM = 8, 46


def git(repo: Path, *args: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def commit_info(repo: Path) -> dict:
    """Where trunk is, as far as this checkout can honestly say.

    A shallow clone can still name the commit but cannot count history, so it
    reports the hash and omits the count rather than publishing the depth of
    the fetch as though it were the age of the project.
    """
    commit = git(repo, "rev-parse", "--short", "HEAD")
    if commit is None:
        return {}
    info = {
        "commit": commit,
        "commit_date": git(repo, "log", "-1", "--format=%cs"),
        "history_complete": git(repo, "rev-parse", "--is-shallow-repository") == "false",
    }
    if info["history_complete"]:
        count = git(repo, "rev-list", "--count", "HEAD")
        if count is not None and count.isdigit():
            info["commit_count"] = int(count)
    return info


def spec_status(traceability: dict | None) -> dict | None:
    """The spec figures the board publishes, from the gate's own report.

    Both sides of the ratio travel together. The board used to receive one
    number and render it as `N / N`, which could only ever say 100% — it said
    so while three normative rules were uncovered.

    `paragraphs_covered / paragraphs_total` is deliberately not published as a
    headline. It counts informative prose and examples that are not required to
    have tests, so it reads around 72% while normative coverage is 99.6%. It is
    carried through only for the disclosure line.
    """
    if not traceability:
        return None
    total = traceability.get("normative_total")
    covered = traceability.get("normative_covered")
    if not isinstance(total, int) or not isinstance(covered, int) or total <= 0:
        return None
    return {
        "normative_traced": covered,
        "normative_total": total,
        "normative_percent": round((covered / total) * 100, 1),
        "complete": covered == total,
        "uncovered": traceability.get("normative_uncovered", total - covered),
        # Uncovered rules that carry a written reason, as opposed to gaps
        # nobody has examined. The board says "3 documented exemptions" only
        # when every uncovered rule is one.
        "known_uncovered": traceability.get("known_uncovered", 0),
        "cases": traceability.get("cases"),
        "paragraphs_total": traceability.get("paragraphs_total"),
        "paragraphs_covered": traceability.get("paragraphs_covered"),
        "platform_unreachable_cases": traceability.get("platform_unreachable_cases", 0),
    }


def _parse_time(value: str | None) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _sparkline(values: list[float]) -> dict:
    """An SVG polyline over the index series, newest last.

    A flat series still draws a line: with no spread, every point sits on the
    midline rather than collapsing onto an edge and implying a trend.
    """
    if not values:
        return {"points": "", "count": 0}
    low, high = min(values), max(values)
    span = high - low
    steps = max(len(values) - 1, 1)

    def y(value: float) -> float:
        if span == 0:
            return (SPARK_TOP + SPARK_BOTTOM) / 2
        # Lower index is faster, so invert: better readings sit higher.
        return SPARK_BOTTOM - ((high - value) / span) * (SPARK_BOTTOM - SPARK_TOP)

    points = [
        (round((i / steps) * SPARK_WIDTH, 1), round(y(v), 1))
        for i, v in enumerate(values)
    ]
    return {
        "points": " ".join(f"{x},{y_}" for x, y_ in points),
        "count": len(points),
        "last_x": points[-1][0],
        "last_y": points[-1][1],
    }


def performance_status(data: dict | None) -> dict | None:
    """The headline index for one platform, from the derived dashboard data.

    One platform, named. Platform indexes are normalized against independent
    per-platform baselines, so putting two of them on one line — or averaging
    them — would compare ratios of different things. The dashboard refuses to
    do it and so does this.

    Nothing here recomputes a statistic. The index arrives already decided by
    `rue-bench derive`; a second implementation would drift from the
    calibrator's, and the drifting one would be the one on the homepage.
    """
    if not data:
        return None
    for platform in data.get("platforms", []):
        # The current epoch's stretch only. Each epoch normalizes against its
        # own baseline, so a line drawn through a boundary renders a
        # rebaselining as a change in the compiler — the same reason the
        # dashboard draws one polyline per epoch rather than one per series.
        indexed: list[dict] = []
        epoch: dict = {}
        for candidate in platform.get("epochs", []):
            points = [
                point
                for point in candidate.get("points", [])
                if isinstance(point.get("index"), dict)
                and isinstance(point["index"].get("latency"), (int, float))
            ]
            if points:
                epoch, indexed = candidate, points
        if not indexed:
            continue

        latest = indexed[-1]
        values = [point["index"]["latency"] for point in indexed]
        status = {
            "platform": platform.get("platform"),
            "epoch": epoch.get("epoch"),
            "index": round(latest["index"]["latency"], 3),
            "measured_at": latest.get("finished_at"),
            "points_count": len(values),
            "spark": _sparkline(values),
            "delta_percent": None,
        }

        # Against irregular collection, "a week" means the most recent point at
        # least seven days older — not a fixed offset into the series. The
        # search is confined to `indexed`, which is one epoch, so the
        # comparison can never cross a baseline change.
        newest = _parse_time(latest.get("finished_at"))
        if newest is not None:
            cutoff = newest - timedelta(days=7)
            for point in reversed(indexed):
                when = _parse_time(point.get("finished_at"))
                if when is not None and when <= cutoff:
                    earlier = point["index"]["latency"]
                    if earlier:
                        change = (latest["index"]["latency"] / earlier - 1) * 100
                        status["delta_percent"] = round(change, 2)
                    break
        return status
    return None


def build_status(
    commit: dict,
    traceability: dict | None,
    performance: dict | None,
    platforms: list[str],
) -> dict:
    """Assemble the board. Pure, so the shape is testable without a repository."""
    status = {
        "generated": True,
        "platforms": platforms,
        "spec": spec_status(traceability),
        "performance": performance_status(performance),
    }
    status.update(commit)
    return status


def read_json(path: Path | None) -> dict | None:
    if path is None or not path.is_file():
        return None
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument(
        "--traceability",
        type=Path,
        help="JSON from `rue-spec --traceability --json`. Omitted or unreadable "
        "leaves the spec rows off the board.",
    )
    parser.add_argument(
        "--performance-data",
        type=Path,
        help="The derived dashboard data. Omitted or unreadable leaves the "
        "performance row off the board.",
    )
    parser.add_argument(
        "--platforms",
        nargs="*",
        default=["x86-64", "arm64", "macOS"],
        help="Platforms the project builds for.",
    )
    args = parser.parse_args()

    status = build_status(
        commit_info(args.repo),
        read_json(args.traceability),
        read_json(args.performance_data),
        list(args.platforms),
    )
    args.out.write_text(json.dumps(status, indent=2, sort_keys=True) + "\n")

    spec = status["spec"]
    performance = status["performance"]
    print(f"  trunk: {status.get('commit', 'unavailable')}", end="")
    print(f" · commit {status['commit_count']}" if "commit_count" in status else " · count unavailable")
    if spec:
        print(
            f"  spec: {spec['normative_traced']}/{spec['normative_total']} normative "
            f"({spec['normative_percent']}%), {spec['cases']} cases"
        )
    else:
        print("  spec: unavailable, rows omitted")
    if performance:
        print(
            f"  performance: index {performance['index']} on {performance['platform']} "
            f"({performance['points_count']} point(s))"
        )
    else:
        print("  performance: no published index yet, row omitted")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
