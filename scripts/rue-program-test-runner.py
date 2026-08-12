#!/usr/bin/env python3
"""Run one rue_program_test scenario against a prebuilt executable (RUE-1404).

The test half of ADR-0070's program/scenario split: the compile happened in the
`rue_program` build action, so all this runner does is stage the scenario's
runtime fixtures into a WRITABLE working directory, run the program, and check
expectations. Two properties are deliberate (ADR-0070, "Two rules and one
provider"): fixtures are frequently inline and deliberately malformed rather
than checked-in files, hence the spec-driven `files`; and several programs
write output through relative paths, hence the fresh temp cwd rather than a
read-only runfiles tree.

The spec is a JSON file generated at analysis time by `rue_program_test`:
{
  "program_args":    [...],
  "program_env":     {...},
  "files":           [{"path": ..., "source": ...}, ...],
  "stdin":           str | null,
  "exit_code":       int,
  "stdout":          str | null,        # exact match
  "stdout_contains": [...],
  "stderr_contains": [...]
}
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: rue-program-test-runner SPEC PROGRAM", file=sys.stderr)
        return 2

    _, spec_path, program = sys.argv
    with open(spec_path, encoding="utf-8") as handle:
        spec = json.load(handle)

    program = os.path.abspath(program)
    workdir = tempfile.mkdtemp(prefix="rue-program-test-")
    try:
        for entry in spec.get("files", []):
            path = os.path.join(workdir, entry["path"])
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(entry["source"])

        env = dict(os.environ)
        env.update(spec.get("program_env", {}))

        result = subprocess.run(
            [program] + spec.get("program_args", []),
            cwd=workdir,
            env=env,
            input=spec.get("stdin"),
            capture_output=True,
            text=True,
            check=False,
        )

        failures = []
        expected_exit = spec.get("exit_code", 0)
        if result.returncode != expected_exit:
            failures.append(
                f"exit code: expected {expected_exit}, got {result.returncode}"
            )
        exact_stdout = spec.get("stdout")
        if exact_stdout is not None and result.stdout != exact_stdout:
            failures.append(
                f"stdout: expected {exact_stdout!r}, got {result.stdout!r}"
            )
        for needle in spec.get("stdout_contains", []):
            if needle not in result.stdout:
                failures.append(f"stdout missing {needle!r}")
        for needle in spec.get("stderr_contains", []):
            if needle not in result.stderr:
                failures.append(f"stderr missing {needle!r}")

        if failures:
            for failure in failures:
                print(f"FAIL: {failure}", file=sys.stderr)
            sys.stderr.write(result.stdout)
            sys.stderr.write(result.stderr)
            return 1
        return 0
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
