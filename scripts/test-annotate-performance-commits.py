#!/usr/bin/env python3
"""Tests for the dashboard's commit annotations (RUE-1194)."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "annotate_performance_commits",
    Path(__file__).resolve().parent / "annotate-performance-commits.py",
)
annotate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(annotate)


def data(*commits: str) -> dict:
    """Derived data carrying one point per commit."""
    return {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {"epoch": 2, "points": [{"commit": commit} for commit in commits]}
                ],
            }
        ]
    }


def missing(_commit):
    return None


def test_subjects_and_ordinals_are_attached() -> None:
    subject = data("aaa", "bbb")
    subjects_found, ordinals_found, total = annotate.annotate(
        subject,
        {"aaa": "first", "bbb": "second"}.get,
        {"aaa": 10, "bbb": 13}.get,
    )
    assert (subjects_found, ordinals_found, total) == (2, 2, 2)
    assert subject["commit_subjects"] == {"aaa": "first", "bbb": "second"}
    assert subject["commit_ordinals"] == {"aaa": 10, "bbb": 13}


def test_an_unresolvable_commit_is_omitted_rather_than_invented() -> None:
    # A shallow clone or rewritten history. The page falls back to the hash and
    # drops the skipped-commit line; it must not publish a guess for either.
    subject = data("aaa", "bbb")
    subjects_found, ordinals_found, total = annotate.annotate(
        subject, {"aaa": "first"}.get, {"bbb": 4}.get
    )
    assert (subjects_found, ordinals_found, total) == (1, 1, 2)
    assert subject["commit_subjects"] == {"aaa": "first"}
    assert subject["commit_ordinals"] == {"bbb": 4}


def test_both_keys_are_always_written() -> None:
    # The page reads `data.commit_ordinals` unconditionally. Emitting the key
    # only when something resolved would make an empty repository a different
    # shape from a shallow one, for no gain.
    subject = data("aaa")
    annotate.annotate(subject, missing, missing)
    assert subject["commit_subjects"] == {}
    assert subject["commit_ordinals"] == {}


def test_an_empty_dashboard_annotates_to_nothing() -> None:
    subject = {"platforms": []}
    assert annotate.annotate(subject, missing, missing) == (0, 0, 0)
    assert subject["commit_ordinals"] == {}


def test_commits_are_collected_across_platforms_and_epochs() -> None:
    subject = {
        "platforms": [
            {
                "platform": "x86_64-linux",
                "epochs": [
                    {"epoch": 1, "points": [{"commit": "aaa"}]},
                    {"epoch": 2, "points": [{"commit": "bbb"}]},
                ],
            },
            {
                "platform": "aarch64-macos",
                # The same commit measured on two platforms is one commit.
                "epochs": [{"epoch": 2, "points": [{"commit": "bbb"}, {"commit": "ccc"}]}],
            },
        ]
    }
    assert annotate.measured_commits(subject) == {"aaa", "bbb", "ccc"}


def test_commits_are_collected_from_the_runtime_series_too() -> None:
    # ADR-0072's runtime records are a sibling section rather than another
    # platform, so a collector that walked only the top level would leave every
    # runtime tooltip without a subject.
    subject = {
        "platforms": [
            {"platform": "x86_64-linux", "epochs": [{"epoch": 2, "points": [{"commit": "aaa"}]}]}
        ],
        "runtime": {
            "platforms": [
                {
                    "platform": "x86_64-linux",
                    # The same commit in both series is still one commit.
                    "epochs": [{"epoch": 1, "points": [{"commit": "aaa"}, {"commit": "ddd"}]}],
                }
            ]
        },
    }
    assert annotate.measured_commits(subject) == {"aaa", "ddd"}


def test_an_underived_runtime_section_is_not_an_error() -> None:
    # `rue-bench derive` omits the section entirely unless a runtime manifest is
    # named, which is how the page tells "not asked for" from "nothing yet".
    assert annotate.measured_commits({"platforms": []}) == set()
    assert annotate.measured_commits({"platforms": [], "runtime": None}) == set()


def test_commit_links_are_built_from_recognized_github_remotes() -> None:
    want = "https://github.com/rue-language/rue/commit/"
    for url in [
        "https://github.com/rue-language/rue.git",
        "https://github.com/rue-language/rue",
        "https://github.com/rue-language/rue/",
        "git@github.com:rue-language/rue.git",
        "git@github.com:rue-language/rue",
        "ssh://git@github.com/rue-language/rue.git",
        "  https://github.com/rue-language/rue.git\n",
    ]:
        assert annotate.commit_url_prefix(url) == want, url


def test_a_credential_in_the_remote_never_reaches_the_published_prefix() -> None:
    # GitHub Actions writes a token into the remote URL during checkout, and
    # this value is published to a world-readable JSON file. The prefix is
    # rebuilt from the owner and name, so nothing else can survive.
    for url in [
        "https://x-access-token:ghp_SECRETVALUE@github.com/rue-language/rue.git",
        "https://someone:hunter2@github.com/rue-language/rue",
        "ssh://deploy-key@github.com/rue-language/rue.git",
    ]:
        prefix = annotate.commit_url_prefix(url)
        assert prefix == "https://github.com/rue-language/rue/commit/", url
        assert "@" not in prefix
        assert "SECRET" not in prefix and "hunter2" not in prefix


def test_an_unrecognized_remote_yields_no_prefix() -> None:
    # Better no link than a wrong one: the page falls back to plain text.
    for url in [
        None,
        "",
        "git@gitlab.com:rue-language/rue.git",
        "https://example.com/rue-language/rue.git",
        # Host must be github.com itself, not merely end with it.
        "https://notgithub.com/rue-language/rue",
        "https://github.com.evil.test/rue-language/rue",
        "/srv/git/rue.git",
    ]:
        assert annotate.commit_url_prefix(url) is None, url


def test_the_prefix_is_written_even_when_absent() -> None:
    subject = data("aaa")
    annotate.annotate(subject, missing, missing, None)
    assert subject["commit_url_prefix"] is None
    annotate.annotate(subject, missing, missing, "https://github.com/o/r/commit/")
    assert subject["commit_url_prefix"] == "https://github.com/o/r/commit/"


def run(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def test_ordinals_count_trunk_positions_not_ancestors() -> None:
    """A merged side branch must not inflate the gap between two measurements.

    This is the whole reason the ordinal follows first parents. Counting
    ancestors instead would report a five-commit topic branch as five skipped
    trunk commits, and the tooltip would claim measurement gaps that never
    happened.
    """
    with tempfile.TemporaryDirectory() as directory:
        repo = Path(directory)
        run(repo, "init", "-q", "-b", "trunk")
        run(repo, "config", "user.email", "test@example.com")
        run(repo, "config", "user.name", "Test")

        def commit(message: str) -> str:
            run(repo, "commit", "-q", "--allow-empty", "-m", message)
            return run(repo, "rev-parse", "HEAD")

        base = commit("base")
        run(repo, "checkout", "-q", "-b", "topic")
        for index in range(3):
            commit(f"topic {index}")
        run(repo, "checkout", "-q", "trunk")
        next_trunk = commit("trunk 1")
        run(repo, "merge", "-q", "--no-ff", "-m", "merge topic", "topic")
        merge = run(repo, "rev-parse", "HEAD")

        ordinals = annotate.first_parent_ordinals(repo, ["trunk"])

        # base -> trunk 1 -> merge is the whole first-parent line.
        assert ordinals[base] == 1
        assert ordinals[next_trunk] == 2
        assert ordinals[merge] == 3
        # Three topic commits are reachable from the merge but sit off the
        # first-parent line, so they are absent rather than numbered.
        assert len(ordinals) == 3
        # The gap the tooltip would report across the merge: one trunk commit.
        assert ordinals[merge] - ordinals[next_trunk] == 1


def test_an_unreadable_repository_yields_no_ordinals() -> None:
    with tempfile.TemporaryDirectory() as directory:
        assert annotate.first_parent_ordinals(Path(directory), ["trunk"]) == {}


def test_refs_are_probed_in_order() -> None:
    with tempfile.TemporaryDirectory() as directory:
        repo = Path(directory)
        run(repo, "init", "-q", "-b", "trunk")
        run(repo, "config", "user.email", "test@example.com")
        run(repo, "config", "user.name", "Test")
        run(repo, "commit", "-q", "--allow-empty", "-m", "base")
        # The first ref does not resolve; probing must continue rather than
        # concluding the repository has no history.
        assert annotate.first_parent_ordinals(repo, ["origin/trunk", "trunk"]) != {}


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
    print(f"ok: {len(tests)} test(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
