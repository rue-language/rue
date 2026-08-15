#!/usr/bin/env python3
"""Tests for the performance-staleness gate (RUE-1258)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import load_script

stall = load_script("validate-performance-stall.py", __file__)


def data(*points: tuple[str, str, str]) -> dict:
    """Build derived data from (platform, commit, finished_at) triples."""
    platforms: dict[str, list] = {}
    for platform, commit, finished_at in points:
        platforms.setdefault(platform, []).append(
            {"commit": commit, "finished_at": finished_at}
        )
    return {
        "platforms": [
            {"platform": name, "epochs": [{"epoch": 2, "points": pts}]}
            for name, pts in platforms.items()
        ]
    }


def fixed(count: int):
    return lambda _commit: count


def test_a_current_series_is_not_stalled() -> None:
    subject = data(("x86_64-linux", "a" * 40, "2026-08-08T00:00:00Z"))
    assert stall.stalled(subject, fixed(0)) == []
    assert stall.stalled(subject, fixed(5)) == [], "the threshold itself still passes"


def test_a_series_past_the_threshold_is_stalled() -> None:
    subject = data(("x86_64-linux", "a" * 40, "2026-08-08T00:00:00Z"))
    behind = stall.stalled(subject, fixed(6))
    assert len(behind) == 1
    assert behind[0][0] == "x86_64-linux"
    assert behind[0][2] == 6


def test_one_stalled_platform_is_enough() -> None:
    # A platform dropping out is a stall even while the others report, because
    # the series that lost its points is the one hiding a regression.
    subject = data(
        ("x86_64-linux", "a" * 40, "2026-08-08T00:00:00Z"),
        ("aarch64-macos", "b" * 40, "2026-07-29T00:00:00Z"),
    )
    counts = {"a" * 40: 0, "b" * 40: 40}
    behind = stall.stalled(subject, lambda commit: counts[commit])
    assert [entry[0] for entry in behind] == ["aarch64-macos"]


def test_an_empty_dashboard_is_not_a_stall() -> None:
    # The honest first state of a suite that has not collected yet. Failing
    # here would block the repository the day collection is introduced.
    assert stall.newest_plotted({"platforms": []}) == []
    assert stall.stalled({"platforms": []}, fixed(1000)) == []


def test_the_newest_point_wins_regardless_of_ordering() -> None:
    subject = {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {
                        "epoch": 2,
                        "points": [
                            {"commit": "b" * 40, "finished_at": "2026-08-08T05:00:00Z"},
                            {"commit": "a" * 40, "finished_at": "2026-08-08T01:00:00Z"},
                        ],
                    }
                ],
            }
        ]
    }
    assert stall.newest_plotted(subject)[0][1] == "b" * 40


def test_the_newest_point_spans_epochs() -> None:
    # A freshly declared epoch carries the newest points while the previous
    # epoch still holds most of them.
    subject = {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {
                        "epoch": 2,
                        "points": [
                            {"commit": "a" * 40, "finished_at": "2026-08-01T00:00:00Z"}
                        ],
                    },
                    {
                        "epoch": 3,
                        "points": [
                            {"commit": "c" * 40, "finished_at": "2026-08-08T00:00:00Z"}
                        ],
                    },
                ],
            }
        ]
    }
    assert stall.newest_plotted(subject)[0][1] == "c" * 40


def indexed_epoch(
    epoch: int,
    baseline: str | None,
    indexes: list[float | None],
    platform: str = "x86_64-linux",
) -> dict:
    """One platform holding one epoch whose points carry the given indexes."""
    return {
        "platforms": [
            {
                "platform": platform,
                "epochs": [
                    {
                        "epoch": epoch,
                        "baseline_commit": baseline,
                        "points": [
                            {
                                "commit": "a" * 40,
                                "finished_at": f"2026-08-0{i + 1}T00:00:00Z",
                                "index": None if value is None else {"latency": value},
                            }
                            for i, value in enumerate(indexes)
                        ],
                    }
                ],
            }
        ]
    }


def test_an_epoch_publishing_its_index_is_healthy() -> None:
    assert stall.unindexed(indexed_epoch(5, "b" * 40, [1.0, 0.98])) == []


def test_a_baseline_that_publishes_no_index_is_a_failure() -> None:
    # Points keep arriving, so the commit-count rule is satisfied while the
    # figure they exist to move is absent. This is the shape RUE-1475 took.
    missing = stall.unindexed(indexed_epoch(5, "b" * 40, [None, None, None]))
    assert missing == [("x86_64-linux", 5, 3)]


def test_an_epoch_with_no_baseline_yet_publishes_no_index_by_design() -> None:
    # The documented state of a freshly declared epoch: it accepts runs before
    # it can express them as an index. Failing here would make declaring the
    # next epoch — the remedy for a stall — itself a repository-wide failure.
    assert stall.unindexed(indexed_epoch(6, None, [None, None])) == []


def test_a_retired_epoch_is_not_held_to_publishing_an_index() -> None:
    # Only the epoch still receiving points is the live signal. An older epoch
    # keeps whatever it published and must not block the repository forever.
    subject = {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {
                        "epoch": 4,
                        "baseline_commit": "b" * 40,
                        "points": [
                            {
                                "commit": "a" * 40,
                                "finished_at": "2026-08-01T00:00:00Z",
                                "index": None,
                            }
                        ],
                    },
                    {
                        "epoch": 5,
                        "baseline_commit": "c" * 40,
                        "points": [
                            {
                                "commit": "d" * 40,
                                "finished_at": "2026-08-08T00:00:00Z",
                                "index": {"latency": 1.0},
                            }
                        ],
                    },
                ],
            }
        ]
    }
    assert stall.unindexed(subject) == []


def test_a_partial_newest_run_does_not_fail_an_indexed_epoch() -> None:
    # A partial run publishes no headline point by design. The epoch is still
    # publishing an index, so the rule is about the epoch, not the last point.
    assert stall.unindexed(indexed_epoch(5, "b" * 40, [1.0, None])) == []


def test_the_unindexed_report_names_the_pin_to_check() -> None:
    text = stall.report_unindexed([("x86_64-linux", 5, 25)])
    assert "not caused by" in text
    assert "epoch.baseline" in text
    assert "performance/manifest.toml" in text


def test_the_report_says_it_is_not_the_authors_fault_and_how_to_fix_it() -> None:
    # There is no bypass, so a pull request author blocked by an unrelated
    # stall must be able to act without reading the issue.
    text = stall.report([("x86_64-linux", "a" * 40, 9)], 5)
    assert "not caused by" in text
    assert "performance/manifest.toml" in text
    assert "collection = true" in text
    assert "needs no baseline" in text


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
    print(f"performance-stall gate valid: {len(tests)} checks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
