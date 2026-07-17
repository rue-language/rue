#!/usr/bin/env python3
"""Measure ADR-0056 clean and owner-local incremental Buck2 builds."""

import argparse
import hashlib
import json
import re
import statistics
import subprocess
import time
from pathlib import Path


REAL = re.compile(r"^\s*([0-9.]+) real", re.MULTILINE)
RSS = re.compile(r"^\s*(\d+)\s+maximum resident set size", re.MULTILINE)
COMMANDS = re.compile(r"Commands: (\d+)")
OWNERS = {
    "RIR": Path("crates/rue-rir/src/inst.rs"),
    "AIR": Path("crates/rue-air/src/inst.rs"),
    "CFG": Path("crates/rue-cfg/src/inst.rs"),
}


def run(worktree: Path, command: list[str], timed: bool = False) -> dict:
    argv = ["/usr/bin/time", "-l", *command] if timed else command
    completed = subprocess.run(argv, cwd=worktree, check=True, capture_output=True, text=True)
    if not timed:
        return {}
    stderr = completed.stderr
    commands = COMMANDS.findall(stderr)
    return {
        "wall_seconds": float(REAL.search(stderr).group(1)),
        "peak_rss_bytes": int(RSS.search(stderr).group(1)),
        "commands": int(commands[-1]) if commands else 0,
    }


def distribution(samples: list[dict], key: str) -> dict:
    raw = [sample[key] for sample in samples]
    median = statistics.median(raw)
    return {
        "raw": raw,
        "median": median,
        "mad": statistics.median(abs(value - median) for value in raw),
    }


def summarize(samples: list[dict]) -> dict:
    return {key: distribution(samples, key) for key in ("wall_seconds", "peak_rss_bytes", "commands")}


def revision(worktree: Path) -> str:
    return subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=worktree, check=True, capture_output=True, text=True
    ).stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--owners", nargs="+", choices=OWNERS, default=list(OWNERS))
    parser.add_argument("--skip-clean", action="store_true")
    args = parser.parse_args()
    owners = {owner: OWNERS[owner] for owner in args.owners}
    probe_run_id = time.time_ns()
    worktrees = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    samples = {name: {"clean": [], "incremental": {owner: [] for owner in owners}} for name in worktrees}
    order = []
    for sample in range(args.samples):
        pair = ("baseline", "candidate") if sample % 2 == 0 else ("candidate", "baseline")
        order.append(list(pair))
        for name in pair:
            tree = worktrees[name]
            if not args.skip_clean:
                run(tree, ["./buck2", "clean"])
                samples[name]["clean"].append(run(tree, ["./buck2", "build", "//crates/rue:rue"], True))
            else:
                run(tree, ["./buck2", "build", "//crates/rue:rue"])
            for owner, relative in owners.items():
                path = tree / relative
                original = path.read_bytes()
                try:
                    if args.skip_clean:
                        probe = (
                            f'\nconst _: &str = "RUE-843 {name} {owner} rebuild probe '
                            f'{probe_run_id} sample {sample}";\n'
                        )
                    else:
                        probe = f"\n// RUE-843 deterministic {owner} rebuild probe\n"
                    path.write_bytes(original + probe.encode())
                    samples[name]["incremental"][owner].append(
                        run(tree, ["./buck2", "build", "//crates/rue:rue"], True)
                    )
                finally:
                    path.write_bytes(original)
                # Re-establish the unmodified owner digest before the next
                # phase probe; this untimed build is not part of the sample.
                run(tree, ["./buck2", "build", "//crates/rue:rue"])

    result = {
        "schema_version": 1,
        "samples_per_revision": args.samples,
        "skip_clean": args.skip_clean,
        "collection_order": order,
        "identity": {
            name: {"worktree": str(tree), "revision": revision(tree)} for name, tree in worktrees.items()
        },
        "probe_sources": {
            owner: {"path": str(path), "sha256": hashlib.sha256((worktrees["candidate"] / path).read_bytes()).hexdigest()}
            for owner, path in owners.items()
        },
        "summary": {},
    }
    for name in worktrees:
        result["summary"][name] = {
            "clean": summarize(samples[name]["clean"]) if samples[name]["clean"] else None,
            "incremental": {
                owner: summarize(owner_samples)
                for owner, owner_samples in samples[name]["incremental"].items()
            },
        }
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
