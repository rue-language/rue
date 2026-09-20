#!/usr/bin/env python3
"""Run the hosted runtime smoke test against the native archive."""

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
    return 0


if __name__ == "__main__":
    try:
        sys.exit(run())
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"hosted runtime smoke test failed: {error}", file=sys.stderr)
        sys.exit(1)
