#!/usr/bin/env python3
"""Reproducible parser-only timing and allocation survey (RUE-906)."""

import argparse
import json
import math
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "//crates/rue-parser-profile:rue-parser-profile"


def resolve_driver(release: bool) -> str:
    command = [str(ROOT / "buck2"), "build", TARGET, "--show-full-output"]
    if release:
        command += ["--target-platforms", "//platforms:release"]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise RuntimeError("failed to build parser profile driver")
    for line in result.stdout.splitlines():
        if line.startswith((TARGET, "root" + TARGET)) and " " in line:
            return line.split(" ", 1)[1]
    raise RuntimeError("Buck2 did not report the parser profile driver path")


def write_fixtures(workdir: Path):
    multi = workdir / "multi"
    multi.mkdir()
    (multi / "main.rue").write_text('const a = @import("a.rue");\nfn main() -> i32 { a.square(4) }\n')
    (multi / "a.rue").write_text("pub fn square(x: i32) -> i32 { x * x }\n")
    fixtures = [
        ("hello", "small", None, [ROOT / "examples/hello.rue"]),
        ("multi_file", "multi", None, [multi / "main.rue", multi / "a.rue"]),
        ("control_flow", "stress", None, [ROOT / "benchmarks/stress/control_flow.rue"]),
        ("deep_nesting", "stress", None, [ROOT / "benchmarks/stress/deep_nesting.rue"]),
    ]
    for functions in (8, 32, 128, 512):
        malformed = workdir / f"malformed_{functions}.rue"
        malformed.write_text(
            "".join(
                f"fn broken_{index}() -> i32 {{ let x: = 0; x }}\n"
                for index in range(functions)
            )
        )
        fixtures.append(
            (f"malformed_{functions}", "malformed", functions, [malformed])
        )
    # Repeat each depth enough times that the smallest member is measurable;
    # depth remains the only changing structural variable in this family.
    for depth in (12, 24, 48, 96, 192):
        adversarial = workdir / f"adversarial_nesting_{depth}.rue"
        body = "{ " * depth + "42" + " }" * depth
        adversarial.write_text(
            "".join(f"fn nested_{index}() -> i32 {{ {body} }}\n" for index in range(64))
        )
        fixtures.append(
            (f"adversarial_nesting_{depth}", "adversarial", depth, [adversarial])
        )
    return fixtures


def run(driver: str, files, iterations: int, warmup: int, timeout: float):
    command = [driver, *map(str, files)]
    samples = []
    for sample in range(warmup + iterations):
        try:
            result = subprocess.run(
                command,
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired as error:
            raise RuntimeError(f"parser profile exceeded {timeout:g}s timeout") from error
        if result.returncode != 0:
            raise RuntimeError(result.stderr.strip() or "parser profile driver failed")
        parsed = json.loads(result.stdout)
        if sample >= warmup:
            samples.append(parsed)
    first = samples[0]
    stable = ("files", "source_bytes", "tokens", "outcome")
    if any(any(sample[key] != first[key] for key in stable) for sample in samples[1:]):
        raise RuntimeError("non-timing parser profile metrics changed across fresh processes")
    times = [sample["parser_ns"] / 1_000_000 for sample in samples]
    median = statistics.median(times)
    mad = statistics.median(abs(value - median) for value in times)
    allocation_samples = [sample["allocations"] for sample in samples]
    byte_samples = [sample["allocated_bytes"] for sample in samples]
    allocation_median = statistics.median(allocation_samples)
    allocated_byte_median = statistics.median(byte_samples)
    return {
        **{key: first[key] for key in stable},
        "parser_ms": {"median": median, "mad": mad},
        "allocations": {
            "median": allocation_median,
            "mad": statistics.median(abs(value - allocation_median) for value in allocation_samples),
        },
        "allocated_bytes": {
            "median": allocated_byte_median,
            "mad": statistics.median(abs(value - allocated_byte_median) for value in byte_samples),
        },
        "allocations_per_ktoken": allocation_median / first["tokens"] * 1000,
        "allocated_bytes_per_token": allocated_byte_median / first["tokens"],
    }


def scaling_summary(results, kind: str):
    family = [result for result in results if result["class"] == kind]
    first, last = family[0], family[-1]
    token_growth = last["tokens"] / first["tokens"]
    time_growth = last["parser_ms"]["median"] / first["parser_ms"]["median"]
    allocation_growth = last["allocations"]["median"] / first["allocations"]["median"]
    return {
        "members": len(family),
        "first_scale": first["scale"],
        "last_scale": last["scale"],
        "token_growth": token_growth,
        "time_growth": time_growth,
        "allocation_growth": allocation_growth,
        "time_growth_per_token_growth": time_growth / token_growth,
        "allocation_growth_per_token_growth": allocation_growth / token_growth,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=7)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--release", action="store_true")
    parser.add_argument(
        "--timeout", type=float, default=10.0, help="seconds allowed per fresh process"
    )
    args = parser.parse_args()
    if (
        args.iterations < 1
        or args.warmup < 0
        or not math.isfinite(args.timeout)
        or args.timeout <= 0
    ):
        parser.error("iterations and timeout must be positive and warmup non-negative")
    try:
        driver = resolve_driver(args.release)
        with tempfile.TemporaryDirectory(prefix="rue-parser-profile-") as directory:
            results = [
                {
                    "name": name,
                    "class": kind,
                    "scale": scale,
                    **run(driver, files, args.iterations, args.warmup, args.timeout),
                }
                for name, kind, scale, files in write_fixtures(Path(directory))
            ]
    except (RuntimeError, OSError, json.JSONDecodeError) as error:
        sys.exit(f"parser-profile: {error}")
    print(
        json.dumps(
            {
                "schema_version": 1,
                "profile": "release" if args.release else "default",
                "iterations": args.iterations,
                "warmup": args.warmup,
                "timeout_seconds": args.timeout,
                "scaling": {
                    kind: scaling_summary(results, kind)
                    for kind in ("malformed", "adversarial")
                },
                "results": results,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
