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


def test_a_burst_with_a_recent_newest_commit_is_not_a_stall() -> None:
    # RUE-1258 recency guard: multi-commit queue merges can outrun the
    # per-push collector, but an actively collecting series always has a
    # recent newest plotted commit. Only a count past the threshold AND an
    # aged newest commit is a stall.
    subject = data(("x86_64-linux", "a" * 40, "2026-08-08T00:00:00Z"))
    young = lambda commit: 60
    old = lambda commit: 3 * 60 * 60
    assert stall.stalled(subject, fixed(40), commit_age=young) == []
    behind = stall.stalled(subject, fixed(40), commit_age=old)
    assert [entry[2] for entry in behind] == [40]
    # Without an age source the count alone still decides, as before.
    assert stall.stalled(subject, fixed(40)) != []


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


def collecting_epoch(
    epoch: int,
    baseline: str | None,
    points: list[tuple[str, bool]],
    platform: str = "x86_64-linux",
) -> dict:
    """One platform holding one epoch whose points are (commit, complete).

    Points are stamped a minute apart in the order given, so a fixture can hold
    more of them than a day has hours without the timestamps going out of order.
    """
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
                                "commit": commit,
                                "run": f"run-of-{commit}",
                                "complete": complete,
                                "finished_at": f"2026-08-15T00:{i:02d}:00Z",
                                "index": None,
                            }
                            for i, (commit, complete) in enumerate(points)
                        ],
                    }
                ],
            }
        ]
    }


def test_a_new_epoch_is_given_room_to_pin() -> None:
    # The state is correct while it is young: a baseline is a complete valid run
    # under the epoch, so it cannot be named before collection produces one.
    points = [(chr(97 + i) * 40, True) for i in range(stall.DEFAULT_MAX_UNPINNED_POINTS)]
    assert stall.unpinned(collecting_epoch(6, None, points[:1])) == []
    assert stall.unpinned(collecting_epoch(6, None, points)) == [], (
        "the threshold itself still passes"
    )


def test_the_deadline_is_counted_in_points_not_trunk_commits() -> None:
    # Commits do not arrive one at a time — the RUE-1522 stack put twelve on
    # trunk in a single merge — so a commit-counted deadline can expire in the
    # time one pull request takes to land. A stack costs one point.
    late = stall.unpinned(
        collecting_epoch(6, None, [("a" * 40, True)] * 11), max_points=10
    )
    assert late and late[0][4] == 11


def test_an_epoch_that_never_pins_its_baseline_is_a_failure() -> None:
    # The shape RUE-1533 took: complete runs kept arriving on every platform and
    # the headline index stayed absent, with nothing red anywhere. Fourteen
    # complete runs per platform, which is why the deadline has to be inside
    # that.
    points = [("a" * 40, True)] + [(chr(98 + i) * 40, True) for i in range(13)]
    late = stall.unpinned(collecting_epoch(6, None, points))
    assert late == [("x86_64-linux", 6, "run-of-" + "a" * 40, "a" * 40, 14)]
    assert stall.DEFAULT_MAX_UNPINNED_POINTS < 14


def test_the_run_named_is_the_first_complete_one() -> None:
    # The one the manifest should pin, and the moment the clock started — not
    # whichever point happens to be newest, and not a partial run ahead of it.
    points = [("a" * 40, False)] + [(chr(98 + i) * 40, True) for i in range(11)]
    late = stall.unpinned(collecting_epoch(6, None, points))
    assert [(entry[2], entry[3], entry[4]) for entry in late] == [
        ("run-of-" + "b" * 40, "b" * 40, 11)
    ]


def test_partial_points_never_start_the_deadline() -> None:
    # A partial run is never a baseline, so an epoch that has only ever
    # collected partial runs has nothing to pin. That is the commit-count
    # rule's business, not this one's.
    subject = collecting_epoch(6, None, [("a" * 40, False)] * 50)
    assert stall.unpinned(subject) == []


def test_an_epoch_with_a_baseline_is_not_held_to_pinning_one() -> None:
    subject = collecting_epoch(5, "b" * 40, [("a" * 40, True)] * 50)
    assert stall.unpinned(subject) == []


def test_a_retired_unpinned_epoch_does_not_block_the_repository() -> None:
    # Epoch 4 collected one complete run and was superseded before it was ever
    # pinned. Only the live epoch is the signal anyone reads, and an epoch that
    # will never receive another point cannot be brought current.
    subject = {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {
                        "epoch": 4,
                        "baseline_commit": None,
                        "points": [
                            {
                                "commit": "a" * 40,
                                "run": "run-a",
                                "complete": True,
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
                                "run": "run-d",
                                "complete": True,
                                "finished_at": "2026-08-08T00:00:00Z",
                                "index": {"latency": 1.0},
                            }
                        ],
                    },
                ],
            }
        ]
    }
    assert stall.unpinned(subject) == []


def test_the_unpinned_report_hands_over_the_block_to_paste() -> None:
    # There is no bypass, so the remedy has to be actionable by whoever the
    # gate blocks — including the exact run to pin, which is otherwise only
    # recoverable by re-deriving the data branch by hand.
    text = stall.report_unpinned([("x86_64-linux", 6, "run-a", "a" * 40, 9)], 5)
    assert "not caused by" in text
    assert "performance/manifest.toml" in text
    assert "[epoch.baseline]" in text
    assert 'run = "run-a"' in text
    assert f'commit = "{"a" * 40}"' in text


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
