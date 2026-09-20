#!/usr/bin/env python3
"""Run the hosted runtime smoke test against the native archive."""

import json
import os
import platform
import subprocess
import sys
import tempfile
from pathlib import Path


def native_configuration():
    system = platform.system()
    machine = platform.machine().lower()
    if system == "Linux" and machine in ("x86_64", "amd64"):
        return "RUNTIME_HOSTED_NATIVE", "_start", ["-no-pie", "-lc", "-lpthread"]
    if system == "Linux" and machine in ("aarch64", "arm64"):
        return "RUNTIME_HOSTED_NATIVE", "_start", ["-no-pie", "-lc", "-lpthread"]
    if system == "Darwin" and machine in ("aarch64", "arm64"):
        # Rust's source `_main` is the Mach-O `__main` linker symbol. This
        # keeps dyld's LC_MAIN path on the runtime callback rather than
        # jumping directly to the C fixture's `main`.
        return "RUNTIME_HOSTED_NATIVE", "__main", ["-lpthread"]
    return None


def run():
    configuration = native_configuration()
    if configuration is None:
        print("hosted runtime smoke test skipped on unsupported native host")
        return 0

    archive_variable, entry, libraries = configuration
    archive = os.environ.get(archive_variable)
    if not archive:
        raise RuntimeError(f"missing {archive_variable} test input")

    test_dir = Path(__file__).resolve().parent
    with tempfile.TemporaryDirectory(prefix="rue-hosted-runtime-") as directory:
        executable = Path(directory) / "hosted-runtime"
        compile_command = [
            os.environ.get("CC", "cc"),
            "-std=c11",
            "-pthread",
            "-nostartfiles",
            f"-Wl,-e,{entry}",
            "-o",
            str(executable),
            str(test_dir / "hosted-main.c"),
            str(test_dir / "hosted-pthread.c"),
            str(test_dir / "hosted-reporting.c"),
            archive,
            *libraries,
        ]
        subprocess.run(compile_command, check=True, timeout=30)
        child_environment = os.environ.copy()
        child_environment["RUE_HOSTED_TEST"] = "preserved"
        completed = subprocess.run(
            [str(executable), "hosted-argument"],
            check=False,
            env=child_environment,
            timeout=10,
        )
        if completed.returncode != 37:
            raise RuntimeError(
                f"hosted runtime exited {completed.returncode}, expected 37"
            )
        report_path = Path(directory) / "report.jsonl"
        child_environment["RUE_HOSTED_REPORT_FILE"] = str(report_path)
        unarmed = subprocess.run(
            [str(executable), "panic-unarmed"], capture_output=True,
            env=child_environment, timeout=10,
        )
        if unarmed.returncode != 101 or unarmed.stderr != b"panic\n":
            raise RuntimeError(
                f"hosted startup did not initialize panic reporting: {unarmed}"
            )
        if report_path.read_bytes() != b"caller-owned descriptor\n":
            raise RuntimeError("unarmed reporting overwrote the caller's descriptor 3")

        left_message = b'left:"\\\n' + b"L" * (5000 - 8)
        right_message = b'right:\n\\"' + b"R" * (4100 - 9)
        expected = {
            'left/"thread\none.rue': (111, 7, left_message),
            'a-different/right\\worker.rue': (999, 123, right_message),
        }
        for mode in ["assert-race", "complete-race"]:
            for iteration in range(12):
                completed = subprocess.run(
                    [str(executable), mode], capture_output=True,
                    env=child_environment, timeout=10,
                )
                if completed.returncode != 101 or completed.stdout:
                    raise RuntimeError(f"{mode} iteration {iteration} failed: {completed}")
                records = [json.loads(line) for line in report_path.read_bytes().splitlines()]
                if mode == "complete-race" and len(records) == 2:
                    if records.pop(0) != {"record": "complete", "schema": "1.0"}:
                        raise RuntimeError("completion and failure frames interleaved")
                if len(records) != 1:
                    raise RuntimeError(f"{mode} must end in exactly one whole failure: {records}")
                record = records[0]
                location = record["location"]
                if mode == "complete-race" and location["file"] != 'a-different/right\\worker.rue':
                    raise RuntimeError("completion race reported an impossible caller")
                line, column, message = expected[location["file"]]
                if location != {"file": location["file"], "line": line, "column": column}:
                    raise RuntimeError(f"concurrent source sites were mixed: {location}")
                framed = message[:4096].decode("utf-8") + " …[truncated]"
                if record != {
                    "record": "failure", "schema": "1.0", "kind": "assert",
                    "message": framed, "location": location,
                }:
                    raise RuntimeError(f"{mode} emitted an incoherent or unbounded report")
                if completed.stderr != b"panic: " + message + b"\n":
                    raise RuntimeError(f"{mode} terminated before the winning stderr completed")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(run())
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"hosted runtime smoke test failed: {error}", file=sys.stderr)
        sys.exit(1)
