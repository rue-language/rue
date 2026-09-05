#!/usr/bin/env python3
"""Check every Rue code fence in the website tutorial.

Each ```rue fence names how it is verified in its info string:

* ```rue check                 compiles successfully
* ```rue run                   compiles, runs, exits 0, and prints exactly the
                               next ```text fence (see below)
* ```rue compile-fail Edddd    must fail with the named diagnostic code(s)
* ```rue file=lib/math.rue     is written to that path next to the chapter's
                               later snippets instead of being compiled itself
* ```rue skip                  is not verified; the prose should say why

A `run` fence may also carry `stdin="..."` (with `\\n` escapes) to feed the
program input and `exit=N` when the program is expected to exit nonzero. The
expected output is the next ```text fence after the program; shell fences in
between (```bash, ```sh, ```console) are skipped, since they only show the
reader how to run the program.

Unmarked ```rue fences are an error: the tutorial's whole point is that what it
shows is what the compiler does today, so every fence must say how it is
checked or why it is not.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import shlex
import signal
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path


FENCE_RE = re.compile(r"^```(?P<info>.*)$")
DIAGNOSTIC_CODE_RE = re.compile(r"^E\d{4}$")
CHECK_ATTRS = {"check", "run", "compile-fail", "skip"}
VALUE_ATTRS = {"file", "stdin", "exit"}
OUTPUT_LANGUAGES = {"text", "output"}
SHELL_LANGUAGES = {"bash", "sh", "shell", "console"}
DEFAULT_TIMEOUT_SECONDS = 10.0
ICE_MARKERS = ("panicked at", "internal compiler error")


@dataclass(frozen=True)
class Snippet:
    path: Path
    line: int
    action: str
    source: str
    expected_codes: frozenset[str] = frozenset()
    stdin: str = ""
    exit_code: int = 0
    expected_output: str | None = None
    # Files declared by earlier `file=` fences in the same chapter, written
    # next to this snippet before it is compiled.
    files: tuple[tuple[str, str], ...] = field(default_factory=tuple)

    @property
    def label(self) -> str:
        return f"{self.path}:{self.line}"


def unescape_attr(value: str) -> str:
    return (
        value.replace("\\\\", "\0")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\0", "\\")
    )


def parse_info_string(info: str) -> tuple[str, set[str], dict[str, str]]:
    """Split a fence info string into (language, flag attrs, key=value attrs)."""
    try:
        tokens = shlex.split(info.strip(), posix=True)
    except ValueError as error:
        raise ValueError(f"malformed fence info string {info!r}: {error}") from error
    if not tokens:
        return "", set(), {}

    language = tokens[0]
    flags: set[str] = set()
    values: dict[str, str] = {}
    for token in tokens[1:]:
        if "=" in token:
            key, _, value = token.partition("=")
            if key in values:
                raise ValueError(f"duplicate fence attribute {key}=")
            values[key] = unescape_attr(value)
            continue
        for part in token.split(","):
            if part:
                flags.add(part)
    return language, flags, values


@dataclass
class Fence:
    path: Path
    line: int
    language: str
    flags: set[str]
    values: dict[str, str]
    body: str


def read_fences(path: Path) -> list[Fence]:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    fences: list[Fence] = []
    i = 0
    while i < len(lines):
        match = FENCE_RE.match(lines[i])
        if not match:
            i += 1
            continue

        fence_line = i + 1
        try:
            language, flags, values = parse_info_string(match.group("info"))
        except ValueError as error:
            raise ValueError(f"{path}:{fence_line}: {error}") from error
        i += 1
        body: list[str] = []
        while i < len(lines) and not lines[i].startswith("```"):
            body.append(lines[i])
            i += 1
        fences.append(Fence(path, fence_line, language, flags, values, "".join(body)))
        # Skip the closing fence if present.
        if i < len(lines):
            i += 1
    return fences


def expected_output_for(fences: list[Fence], index: int) -> str:
    """Find the ```text fence that states a run snippet's expected output."""
    program = fences[index]
    for later in fences[index + 1 :]:
        if later.language in OUTPUT_LANGUAGES:
            return later.body
        if later.language in SHELL_LANGUAGES:
            continue
        break
    raise ValueError(
        f"{program.path}:{program.line}: a `rue run` fence must be followed by a "
        "```text fence holding its expected output"
    )


def collect_snippets(root: Path) -> list[Snippet]:
    snippets: list[Snippet] = []

    for path in sorted(root.glob("*.md")):
        fences = read_fences(path)
        chapter_files: list[tuple[str, str]] = []
        for index, fence in enumerate(fences):
            if fence.language != "rue":
                continue
            where = f"{path}:{fence.line}"
            flags = fence.flags
            values = fence.values

            unknown_values = set(values) - VALUE_ATTRS
            if unknown_values:
                raise ValueError(
                    f"{where}: unknown rue fence attribute(s): "
                    f"{', '.join(sorted(unknown_values))}"
                )

            actions = flags & CHECK_ATTRS
            expected_codes = frozenset(
                attr for attr in flags if DIAGNOSTIC_CODE_RE.fullmatch(attr)
            )
            unknown_flags = flags - CHECK_ATTRS - expected_codes
            if unknown_flags:
                raise ValueError(
                    f"{where}: unknown rue fence attribute(s): "
                    f"{', '.join(sorted(unknown_flags))}"
                )
            if len(actions) > 1:
                raise ValueError(f"{where}: use only one of {sorted(CHECK_ATTRS)}")

            if "file" in values:
                if actions or expected_codes or "stdin" in values or "exit" in values:
                    raise ValueError(
                        f"{where}: a file= fence takes no other attributes"
                    )
                relative = values["file"]
                if not relative or Path(relative).is_absolute() or ".." in Path(relative).parts:
                    raise ValueError(
                        f"{where}: file= must be a relative path inside the chapter"
                    )
                chapter_files.append((relative, fence.body))
                continue

            if not actions:
                raise ValueError(
                    f"{where}: unmarked rue fence; mark it with one of "
                    f"{sorted(CHECK_ATTRS)} or file=<path>"
                )
            action = next(iter(actions))

            if action == "compile-fail" and not expected_codes:
                raise ValueError(
                    f"{where}: compile-fail requires at least "
                    "one expected diagnostic code (for example E0203)"
                )
            internal_codes = sorted(code for code in expected_codes if code.startswith("E9"))
            if internal_codes:
                raise ValueError(
                    f"{where}: compile-fail cannot expect internal "
                    f"compiler diagnostic code(s): {', '.join(internal_codes)}"
                )
            if action != "compile-fail" and expected_codes:
                raise ValueError(
                    f"{where}: diagnostic codes are only valid with compile-fail"
                )
            if action != "run" and ("stdin" in values or "exit" in values):
                raise ValueError(f"{where}: stdin= and exit= are only valid with run")

            if action == "skip":
                continue

            exit_code = 0
            if "exit" in values:
                try:
                    exit_code = int(values["exit"])
                except ValueError as error:
                    raise ValueError(f"{where}: exit= must be an integer") from error
                if not 0 <= exit_code <= 255:
                    raise ValueError(f"{where}: exit= must be between 0 and 255")

            expected_output = None
            if action == "run":
                expected_output = expected_output_for(fences, index)

            snippets.append(
                Snippet(
                    path=path,
                    line=fence.line,
                    action=action,
                    source=fence.body,
                    expected_codes=expected_codes,
                    stdin=values.get("stdin", ""),
                    exit_code=exit_code,
                    expected_output=expected_output,
                    files=tuple(chapter_files),
                )
            )

    return snippets


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


def run_with_timeout(
    command: list[str],
    env: dict[str, str],
    timeout_seconds: float,
    stdin: str = "",
) -> subprocess.CompletedProcess[str]:
    process = subprocess.Popen(
        command,
        text=True,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(input=stdin, timeout=timeout_seconds)
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


def snippet_paths(snippet: Snippet, tempdir: Path) -> tuple[Path, Path]:
    chapter_dir = tempdir / snippet.path.stem
    source_path = chapter_dir / f"{snippet.path.stem}-{snippet.line}.rue"
    output_path = chapter_dir / f"{snippet.path.stem}-{snippet.line}"
    return source_path, output_path


def compile_snippet(
    rue_binary: str,
    snippet: Snippet,
    tempdir: Path,
    env: dict[str, str],
    timeout_seconds: float,
) -> subprocess.CompletedProcess[str]:
    source_path, output_path = snippet_paths(snippet, tempdir)
    source_path.parent.mkdir(parents=True, exist_ok=True)
    for relative, content in snippet.files:
        target = source_path.parent / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
    source_path.write_text(snippet.source, encoding="utf-8")

    command = [
        rue_binary,
        "--error-format",
        "json",
        str(source_path),
        "-o",
        str(output_path),
    ]
    return run_with_timeout(command, env, timeout_seconds)


def run_snippet(
    snippet: Snippet,
    tempdir: Path,
    env: dict[str, str],
    timeout_seconds: float,
) -> subprocess.CompletedProcess[str]:
    _, output_path = snippet_paths(snippet, tempdir)
    return run_with_timeout([str(output_path)], env, timeout_seconds, snippet.stdin)


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

    if snippet.action in {"check", "run"}:
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


def run_failure(
    snippet: Snippet, result: subprocess.CompletedProcess[str]
) -> str | None:
    """Return a failure reason for a run snippet, or None when it behaved."""
    if result.returncode < 0:
        return f"program terminated by signal {-result.returncode}"
    if result.returncode != snippet.exit_code:
        return (
            f"expected the program to exit {snippet.exit_code}, but it exited "
            f"{result.returncode}"
        )
    expected = (snippet.expected_output or "").rstrip("\n")
    actual = (result.stdout or "").rstrip("\n")
    if expected != actual:
        return (
            "program output did not match the ```text fence\n"
            f"--- expected ---\n{expected}\n--- actual ---\n{actual}"
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
        help=f"per-snippet compiler and program timeout in seconds (default: {DEFAULT_TIMEOUT_SECONDS:g})",
    )
    args = parser.parse_args()

    try:
        snippets = collect_snippets(args.root)
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
    counts = {"check": 0, "run": 0, "compile-fail": 0}
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
                print(
                    f"error: could not start compiler {rue_binary}: {error}",
                    file=sys.stderr,
                )
                return 1

            failure = snippet_failure(snippet, result)
            if failure is None and snippet.action == "run":
                try:
                    outcome = run_snippet(snippet, tempdir, env, args.timeout)
                except subprocess.TimeoutExpired:
                    failure = f"program timed out after {args.timeout:g}s"
                except OSError as error:
                    failure = f"could not start the compiled program: {error}"
                else:
                    failure = run_failure(snippet, outcome)
                    if failure is not None and outcome.stderr:
                        failure += f"\n--- program stderr ---\n{outcome.stderr}"

            if failure is None:
                counts[snippet.action] += 1
                if not args.quiet:
                    print(f"ok {snippet.label} ({snippet.action})")
                continue

            failures += 1
            print(f"FAIL {snippet.label}: {failure}", file=sys.stderr)
            if result.stdout:
                print(result.stdout, file=sys.stderr)
            if result.stderr:
                print(result.stderr, file=sys.stderr)

    summary = (
        f"checked {len(snippets)} tutorial snippet(s): "
        f"{counts['run']} run, {counts['check']} compiled, "
        f"{counts['compile-fail']} failed as expected"
    )
    if failures:
        print(f"{summary}; {failures} failure(s)", file=sys.stderr)
        return 1
    if not args.quiet:
        print(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
