#!/usr/bin/env python3
"""Check opted-in Rue code fences from the website tutorial.

Tutorial fences are prose first, so the checker is intentionally marker-driven:

* ```rue check        compiles successfully
* ```rue compile-fail Edddd must fail with the named diagnostic code
* ```rue skip         ignored with an explicit reason in prose nearby

Unmarked ```rue fences are left alone. This lets chapter-refresh work opt
snippets in as they become complete, self-contained examples.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import signal
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


FENCE_RE = re.compile(r"^```(?P<info>.*)$")
DIAGNOSTIC_CODE_RE = re.compile(r"^E\d{4}$")
CHECK_ATTRS = {"check", "compile-fail", "skip"}
DEFAULT_TIMEOUT_SECONDS = 10.0
ICE_MARKERS = ("panicked at", "internal compiler error")


@dataclass(frozen=True)
class Snippet:
    path: Path
    line: int
    action: str
    source: str
    expected_codes: frozenset[str] = frozenset()

    @property
    def label(self) -> str:
        return f"{self.path}:{self.line}"


def parse_info_string(info: str) -> tuple[str, set[str]]:
    parts = re.split(r"[\s,]+", info.strip())
    parts = [part for part in parts if part]
    if not parts:
        return "", set()

    language = parts[0]
    attrs = set(parts[1:])
    return language, attrs


def collect_snippets(root: Path) -> tuple[list[Snippet], int]:
    snippets: list[Snippet] = []
    unmarked_rue_fences = 0

    for path in sorted(root.glob("*.md")):
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        i = 0
        while i < len(lines):
            match = FENCE_RE.match(lines[i])
            if not match:
                i += 1
                continue

            language, attrs = parse_info_string(match.group("info"))
            fence_line = i + 1
            i += 1
            body: list[str] = []
            while i < len(lines) and not lines[i].startswith("```"):
                body.append(lines[i])
                i += 1

            if language == "rue":
                actions = attrs & CHECK_ATTRS
                expected_codes = frozenset(
                    attr for attr in attrs if DIAGNOSTIC_CODE_RE.fullmatch(attr)
                )
                unknown_attrs = attrs - CHECK_ATTRS - expected_codes
                if unknown_attrs:
                    raise ValueError(
                        f"{path}:{fence_line}: unknown rue fence attribute(s): "
                        f"{', '.join(sorted(unknown_attrs))}"
                    )
                if len(actions) > 1:
                    raise ValueError(
                        f"{path}:{fence_line}: use only one of {sorted(CHECK_ATTRS)}"
                    )
                if actions:
                    action = next(iter(actions))
                    if action == "compile-fail" and not expected_codes:
                        raise ValueError(
                            f"{path}:{fence_line}: compile-fail requires at least "
                            "one expected diagnostic code (for example E0203)"
                        )
                    internal_codes = sorted(
                        code for code in expected_codes if code.startswith("E9")
                    )
                    if internal_codes:
                        raise ValueError(
                            f"{path}:{fence_line}: compile-fail cannot expect internal "
                            f"compiler diagnostic code(s): {', '.join(internal_codes)}"
                        )
                    if action != "compile-fail" and expected_codes:
                        raise ValueError(
                            f"{path}:{fence_line}: diagnostic codes are only valid "
                            "with compile-fail"
                        )
                    if action != "skip":
                        snippets.append(
                            Snippet(
                                path=path,
                                line=fence_line,
                                action=action,
                                source="".join(body),
                                expected_codes=expected_codes,
                            )
                        )
                else:
                    if attrs:
                        raise ValueError(
                            f"{path}:{fence_line}: rue fence attributes require one of "
                            f"{sorted(CHECK_ATTRS)}"
                        )
                    unmarked_rue_fences += 1

            # Skip the closing fence if present.
            if i < len(lines):
                i += 1

    return snippets, unmarked_rue_fences


def snippet_env() -> dict[str, str]:
    env = os.environ.copy()
    # Structured diagnostics must remain a single JSON document. Inherited
    # tracing output would prefix the document and make valid failures look
    # like malformed infrastructure output.
    env.pop("RUST_LOG", None)
    std_path = Path("std")
    if std_path.is_dir():
        env.setdefault("RUE_STD_PATH", str(std_path.resolve()))
    return env


def compile_snippet(
    rue_binary: str,
    snippet: Snippet,
    tempdir: Path,
    env: dict[str, str],
    timeout_seconds: float,
) -> subprocess.CompletedProcess[str]:
    source_path = tempdir / f"{snippet.path.stem}-{snippet.line}.rue"
    output_path = tempdir / f"{snippet.path.stem}-{snippet.line}"
    source_path.write_text(snippet.source, encoding="utf-8")

    command = [
        rue_binary,
        "--error-format",
        "json",
        str(source_path),
        "-o",
        str(output_path),
    ]
    process = subprocess.Popen(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired as error:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        stdout, stderr = process.communicate()
        raise subprocess.TimeoutExpired(
            command,
            timeout_seconds,
            output=stdout,
            stderr=stderr,
        ) from error

    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def diagnostic_codes(stderr: str) -> frozenset[str]:
    """Parse error codes from Rue's structured diagnostic output."""
    try:
        diagnostics = json.loads(stderr)
    except json.JSONDecodeError as error:
        raise ValueError(f"compiler stderr was not valid JSON: {error.msg}") from error

    if not isinstance(diagnostics, list):
        raise ValueError("compiler diagnostics must be a JSON array")
    if not diagnostics:
        raise ValueError("compiler emitted an empty diagnostic array")

    codes = set()
    for diagnostic in diagnostics:
        if not isinstance(diagnostic, dict):
            raise ValueError("compiler diagnostic entries must be JSON objects")
        code = diagnostic.get("code")
        if diagnostic.get("severity") == "error" and isinstance(code, str):
            codes.add(code)
    if not codes:
        raise ValueError("compiler emitted no error diagnostics")
    return frozenset(codes)


def snippet_failure(
    snippet: Snippet, result: subprocess.CompletedProcess[str]
) -> str | None:
    """Return a failure reason, or None when the compiler outcome is expected."""
    stderr = result.stderr or ""
    if result.returncode < 0:
        return f"compiler terminated by signal {-result.returncode}"
    if result.returncode == 101 or any(marker in stderr for marker in ICE_MARKERS):
        return "compiler reported an internal compiler error"

    actual_codes = None
    diagnostic_error = None
    if result.returncode == 1:
        try:
            actual_codes = diagnostic_codes(stderr)
        except ValueError as error:
            diagnostic_error = error
        else:
            internal_codes = sorted(
                code for code in actual_codes if code.startswith("E9")
            )
            if internal_codes:
                return (
                    "compiler reported internal compiler diagnostic code(s): "
                    + ", ".join(internal_codes)
                )

    if snippet.action == "check":
        if result.returncode == 0:
            return None
        return f"expected snippet to compile, but compiler exited {result.returncode}"

    if result.returncode == 0:
        return "expected snippet to fail compilation, but it compiled successfully"
    if result.returncode != 1:
        return f"compiler returned unexpected failure status {result.returncode}"

    if diagnostic_error is not None:
        return str(diagnostic_error)
    assert actual_codes is not None

    missing = sorted(snippet.expected_codes - actual_codes)
    if missing:
        actual = ", ".join(sorted(actual_codes)) or "none"
        return (
            f"missing expected diagnostic code(s) {', '.join(missing)}; "
            f"compiler emitted: {actual}"
        )
    return None


def find_rue_binary() -> str:
    env_binary = os.environ.get("RUE_BINARY")
    if env_binary:
        return env_binary

    helper = Path("scripts/rue-bin")
    if helper.exists():
        result = subprocess.run(
            [str(helper)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if result.returncode == 0:
            return result.stdout.strip()
        raise SystemExit(result.stderr)

    raise SystemExit("RUE_BINARY is not set and scripts/rue-bin was not found")


def positive_seconds(value: str) -> float:
    seconds = float(value)
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("timeout must be a finite value greater than zero")
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root",
        nargs="?",
        default="website/content/tutorial",
        type=Path,
        help="tutorial markdown directory to scan",
    )
    parser.add_argument("--quiet", action="store_true", help="only print failures")
    parser.add_argument(
        "--timeout",
        type=positive_seconds,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"per-snippet compiler timeout in seconds (default: {DEFAULT_TIMEOUT_SECONDS:g})",
    )
    args = parser.parse_args()

    try:
        snippets, unmarked = collect_snippets(args.root)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if not snippets:
        print(
            f"error: no checked tutorial snippets found in {args.root}",
            file=sys.stderr,
        )
        return 1

    rue_binary = find_rue_binary()

    failures = 0
    env = snippet_env()
    with tempfile.TemporaryDirectory(prefix="rue-tutorial-snippets-") as temp:
        tempdir = Path(temp)
        for snippet in snippets:
            try:
                result = compile_snippet(
                    rue_binary, snippet, tempdir, env, args.timeout
                )
            except subprocess.TimeoutExpired as error:
                failures += 1
                print(
                    f"FAIL {snippet.label}: compiler timed out after {args.timeout:g}s",
                    file=sys.stderr,
                )
                if error.stdout:
                    print(error.stdout, file=sys.stderr)
                if error.stderr:
                    print(error.stderr, file=sys.stderr)
                continue
            except OSError as error:
                failures += 1
                print(
                    f"FAIL {snippet.label}: could not start compiler: {error}",
                    file=sys.stderr,
                )
                continue

            failure = snippet_failure(snippet, result)
            if failure is None:
                if not args.quiet:
                    print(f"ok {snippet.label} ({snippet.action})")
                continue

            failures += 1
            print(f"FAIL {snippet.label}: {failure}", file=sys.stderr)
            if result.stdout:
                print(result.stdout, file=sys.stderr)
            if result.stderr:
                print(result.stderr, file=sys.stderr)

    if failures:
        return 1

    if not args.quiet:
        print(
            f"checked {len(snippets)} tutorial snippet(s); "
            f"{unmarked} unmarked rue fence(s) left prose-only"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
