#!/usr/bin/env python3
"""Generate deterministic CLI shard weights from machine-readable case timings."""

from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = ROOT / "crates" / "rue-cli-tests" / "shard-weights.json"
SUPPORTED_PLATFORMS = {"common", "linux-x64", "linux-arm64", "macos"}
VERSION = 1


def canonical_text(document: dict[str, Any]) -> str:
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def read_document(path: Path) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    if not isinstance(document, dict):
        raise ValueError(f"{path}: top-level value must be an object")
    if document.get("version") != VERSION:
        raise ValueError(f"{path}: version must be {VERSION}")
    default_ms = document.get("default_ms")
    if not isinstance(default_ms, int) or isinstance(default_ms, bool) or default_ms <= 0:
        raise ValueError(f"{path}: default_ms must be a positive integer")
    for field in ("common", "platforms"):
        if not isinstance(document.get(field), dict):
            raise ValueError(f"{path}: {field} must be an object")
    unknown_platforms = set(document["platforms"]) - (SUPPORTED_PLATFORMS - {"common"})
    if unknown_platforms:
        raise ValueError(f"{path}: unsupported platforms: {sorted(unknown_platforms)}")
    for platform, weights in [("common", document["common"]), *document["platforms"].items()]:
        if not isinstance(weights, dict):
            raise ValueError(f"{path}: weights for {platform} must be an object")
        for name, weight in weights.items():
            if not isinstance(name, str) or not name:
                raise ValueError(f"{path}: case names must be non-empty strings")
            if not isinstance(weight, int) or isinstance(weight, bool) or weight <= 0:
                raise ValueError(
                    f"{path}: weight for {platform}/{name} must be a positive integer"
                )
    return document


def timing_weights(paths: list[Path]) -> dict[str, int]:
    samples: dict[str, list[float]] = defaultdict(list)
    for path in paths:
        starts: dict[str, float] = {}
        try:
            lines = path.read_text().splitlines()
        except OSError as error:
            raise ValueError(f"cannot read {path}: {error}") from error
        for line_number, line in enumerate(lines, 1):
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError as error:
                raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
            event_kind = event.get("event")
            if event_kind not in {
                "rue_cli_case_timing",
                "case_start",
                "case_complete",
            }:
                continue
            name = event.get("name")
            elapsed_s = event.get("elapsed_s")
            if isinstance(elapsed_s, str):
                try:
                    elapsed_s = float(elapsed_s)
                except ValueError:
                    pass
            if not isinstance(name, str) or not name:
                raise ValueError(f"{path}:{line_number}: invalid or missing case name")
            if (
                not isinstance(elapsed_s, (int, float))
                or isinstance(elapsed_s, bool)
                or not math.isfinite(elapsed_s)
                or elapsed_s < 0
            ):
                raise ValueError(f"{path}:{line_number}: invalid elapsed_s for {name!r}")
            elapsed_s = float(elapsed_s)
            if event_kind == "case_start":
                starts[name] = elapsed_s
            elif event_kind == "case_complete":
                if name not in starts:
                    raise ValueError(
                        f"{path}:{line_number}: case_complete for {name!r} has no case_start"
                    )
                samples[name].append(elapsed_s - starts.pop(name))
            else:
                samples[name].append(elapsed_s)
    if not samples:
        raise ValueError("timing inputs contained no rue_cli_case_timing events")
    return {
        name: max(1, round(statistics.median(values) * 1000))
        for name, values in sorted(samples.items())
    }


def parse_input(value: str) -> tuple[str, Path]:
    try:
        platform, raw_path = value.split("=", 1)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected PLATFORM=PATH") from error
    if platform not in SUPPORTED_PLATFORMS:
        raise argparse.ArgumentTypeError(
            f"unsupported platform {platform!r}; expected one of "
            f"{', '.join(sorted(SUPPORTED_PLATFORMS))}"
        )
    if not raw_path:
        raise argparse.ArgumentTypeError("timing path must not be empty")
    return platform, Path(raw_path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--timings",
        action="append",
        default=[],
        metavar="PLATFORM=PATH",
        type=parse_input,
        help="JSONL timing input; repeat to combine samples or update platforms",
    )
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate that the checked file is canonical instead of updating it",
    )
    args = parser.parse_args()

    try:
        document = read_document(args.output)
        if args.check:
            if args.timings:
                parser.error("--check cannot be combined with --timings")
            actual = args.output.read_text()
            expected = canonical_text(document)
            if actual != expected:
                print(
                    f"error: {args.output} is not canonical; regenerate it with this tool",
                    file=sys.stderr,
                )
                return 1
            print(f"CLI shard weights valid: {args.output}")
            return 0
        if not args.timings:
            parser.error("at least one --timings PLATFORM=PATH is required")

        grouped: dict[str, list[Path]] = defaultdict(list)
        for platform, path in args.timings:
            grouped[platform].append(path)
        for platform, paths in sorted(grouped.items()):
            weights = timing_weights(paths)
            if platform == "common":
                document["common"] = weights
                document["default_ms"] = max(1, round(statistics.median(weights.values())))
            else:
                document["platforms"][platform] = weights
            print(f"{platform}: recorded {len(weights)} case weights")
        args.output.write_text(canonical_text(document))
        print(f"wrote {args.output}")
        return 0
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
