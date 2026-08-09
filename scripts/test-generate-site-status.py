#!/usr/bin/env python3
"""Tests for the homepage Field Report generator (RUE-1261)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "generate_site_status",
    Path(__file__).resolve().parent / "generate-site-status.py",
)
gen = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gen)


def traceability(covered: int = 798, total: int = 801, **overrides) -> dict:
    report = {
        "normative_total": total,
        "normative_covered": covered,
        "normative_uncovered": total - covered,
        "known_uncovered": total - covered,
        "paragraphs_total": 1207,
        "paragraphs_covered": 875,
        "cases": 2277,
        "platform_unreachable_cases": 1,
    }
    report.update(overrides)
    return report


def point(when: str, index: float | None, epoch_points: list | None = None) -> dict:
    entry = {"finished_at": when, "commit": "a" * 40}
    if index is not None:
        entry["index"] = {"latency": index}
    return entry


def perf(*points: dict, epoch: int = 2, platform: str = "x86_64-linux") -> dict:
    return {
        "platforms": [
            {"platform": platform, "epochs": [{"epoch": epoch, "points": list(points)}]}
        ]
    }


# ---- spec figures ----------------------------------------------------------

def test_both_sides_of_the_ratio_are_published() -> None:
    # The defect this generator exists to prevent: the board received one
    # number, rendered it as N / N, and reported 100% while three rules were
    # uncovered.
    spec = gen.spec_status(traceability())
    assert spec["normative_traced"] == 798
    assert spec["normative_total"] == 801
    assert spec["normative_traced"] != spec["normative_total"]
    assert spec["normative_percent"] == 99.6
    assert spec["complete"] is False


def test_complete_coverage_is_marked_complete() -> None:
    spec = gen.spec_status(traceability(covered=801, total=801))
    assert spec["complete"] is True
    assert spec["normative_percent"] == 100.0


def test_the_diluted_total_is_never_the_headline() -> None:
    # paragraphs_covered/paragraphs_total counts informative prose that is not
    # required to have tests. It reads ~72% while normative coverage is 99.6%,
    # so it is carried but must not be what `normative_percent` reports.
    spec = gen.spec_status(traceability())
    diluted = round((875 / 1207) * 100, 1)
    assert diluted == 72.5
    assert spec["normative_percent"] != diluted
    assert spec["paragraphs_covered"] == 875


def test_missing_or_malformed_traceability_omits_the_spec_rows() -> None:
    for bad in [None, {}, {"normative_total": 0, "normative_covered": 0},
                {"normative_total": "801", "normative_covered": 798}]:
        assert gen.spec_status(bad) is None, bad


# ---- commit ----------------------------------------------------------------

def test_a_shallow_clone_publishes_no_commit_count() -> None:
    # Publishing the fetch depth as though it were the project's history is
    # exactly the class of invented number this board must not carry.
    status = gen.build_status(
        {"commit": "abc1234", "history_complete": False}, None, None, []
    )
    assert status["commit"] == "abc1234"
    assert "commit_count" not in status


def test_a_full_clone_publishes_the_count() -> None:
    status = gen.build_status(
        {"commit": "abc1234", "history_complete": True, "commit_count": 2107},
        None, None, [],
    )
    assert status["commit_count"] == 2107


# ---- performance -----------------------------------------------------------

def test_no_published_index_yields_no_performance_row() -> None:
    # The honest first state: collection has run nothing, so there is no index.
    assert gen.performance_status(None) is None
    assert gen.performance_status({"platforms": []}) is None
    assert gen.performance_status(perf(point("2026-08-01T00:00:00Z", None))) is None


def test_the_headline_names_one_platform() -> None:
    data = {
        "platforms": [
            {"platform": "x86_64-linux", "epochs": [{"epoch": 2, "points": [
                point("2026-08-01T00:00:00Z", 1.0), point("2026-08-08T00:00:00Z", 0.97)]}]},
            {"platform": "aarch64-macos", "epochs": [{"epoch": 2, "points": [
                point("2026-08-08T00:00:00Z", 1.4)]}]},
        ]
    }
    status = gen.performance_status(data)
    # One platform, named — never two indexes side by side. They are normalized
    # against independent baselines, so comparing them is meaningless.
    assert status["platform"] == "x86_64-linux"
    assert status["index"] == 0.97
    assert "1.4" not in str(status)


def test_the_week_delta_uses_the_newest_point_at_least_a_week_old() -> None:
    status = gen.performance_status(perf(
        point("2026-08-01T00:00:00Z", 1.00),
        point("2026-08-05T00:00:00Z", 0.99),  # too recent to be "a week ago"
        point("2026-08-08T00:00:00Z", 0.95),
    ))
    # 0.95 against 1.00, not against 0.99.
    assert status["delta_percent"] == -5.0


def test_no_delta_when_nothing_is_a_week_old() -> None:
    status = gen.performance_status(perf(
        point("2026-08-07T00:00:00Z", 1.00), point("2026-08-08T00:00:00Z", 0.95)))
    assert status["delta_percent"] is None


def test_only_the_current_epoch_is_drawn() -> None:
    # Each epoch normalizes against its own baseline. A sparkline through a
    # boundary renders a rebaselining as a change in the compiler, and a delta
    # across one reports it as movement. Neither may happen.
    data = {"platforms": [{"platform": "x86_64-linux", "epochs": [
        {"epoch": 1, "points": [point("2026-07-01T00:00:00Z", 1.20),
                                point("2026-07-02T00:00:00Z", 1.30)]},
        # Both inside one week, so the only week-old point anywhere is epoch 1's.
        {"epoch": 2, "points": [point("2026-08-08T00:00:00Z", 1.00),
                                point("2026-08-09T00:00:00Z", 0.90)]},
    ]}]}
    status = gen.performance_status(data)
    assert status["epoch"] == 2
    assert status["points_count"] == 2, "epoch 1's points must not be in the line"
    assert status["spark"]["count"] == 2
    # Epoch 1's older point must not become the comparand.
    assert status["delta_percent"] is None


def test_a_delta_within_the_current_epoch_is_reported() -> None:
    data = {"platforms": [{"platform": "x86_64-linux", "epochs": [
        {"epoch": 1, "points": [point("2026-07-01T00:00:00Z", 5.00)]},
        {"epoch": 2, "points": [point("2026-08-01T00:00:00Z", 1.00),
                                point("2026-08-09T00:00:00Z", 0.90)]},
    ]}]}
    status = gen.performance_status(data)
    assert status["delta_percent"] == -10.0, "against epoch 2's own week-old point"


def test_a_flat_series_still_draws_a_line() -> None:
    spark = gen._sparkline([1.0, 1.0, 1.0])
    ys = {pair.split(",")[1] for pair in spark["points"].split()}
    assert len(ys) == 1, "a flat series is one horizontal line"
    assert ys != {"46.0"} and ys != {"8.0"}, "and sits on the midline, not an edge"


def test_lower_index_draws_higher() -> None:
    # Lower is faster, so an improving series must trend upward on the chart.
    spark = gen._sparkline([1.0, 0.9])
    first, last = spark["points"].split()
    assert float(last.split(",")[1]) < float(first.split(",")[1])


def test_the_sparkline_spans_the_viewbox() -> None:
    spark = gen._sparkline([1.0, 0.98, 0.95, 0.9])
    xs = [float(pair.split(",")[0]) for pair in spark["points"].split()]
    assert xs[0] == 0.0 and xs[-1] == float(gen.SPARK_WIDTH)
    assert spark["count"] == 4
    assert (spark["last_x"], spark["last_y"]) == (xs[-1],
        float(spark["points"].split()[-1].split(",")[1]))


def test_a_single_point_is_not_a_trend() -> None:
    status = gen.performance_status(perf(point("2026-08-08T00:00:00Z", 1.0)))
    assert status["points_count"] == 1
    # The template only draws the polyline when count > 1.
    assert status["spark"]["count"] == 1


# ---- assembly --------------------------------------------------------------

def test_the_board_never_invents_a_value() -> None:
    # Everything absent, nothing fabricated: no zeroes, no placeholders.
    status = gen.build_status({}, None, None, ["x86-64"])
    assert status["spec"] is None
    assert status["performance"] is None
    assert "commit" not in status
    assert status["platforms"] == ["x86-64"]
    assert status["generated"] is True


def test_the_shipped_shape_is_json_serializable() -> None:
    import json
    status = gen.build_status(
        {"commit": "abc1234", "history_complete": True, "commit_count": 2107,
         "commit_date": "2026-08-09"},
        traceability(),
        perf(point("2026-08-01T00:00:00Z", 1.0), point("2026-08-09T00:00:00Z", 0.97)),
        ["x86-64", "arm64", "macOS"],
    )
    round_tripped = json.loads(json.dumps(status))
    assert round_tripped["spec"]["normative_traced"] == 798
    assert round_tripped["performance"]["platform"] == "x86_64-linux"


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
    print(f"ok: {len(tests)} test(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
