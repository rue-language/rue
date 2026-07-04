#!/usr/bin/env python3
"""Check opted-in Rue code fences from the website tutorial.

Tutorial fences are prose first, so the checker is intentionally marker-driven:

* ```rue check        compiles successfully
* ```rue compile-fail must fail compilation
* ```rue skip         ignored with an explicit reason in prose nearby

Unmarked ```rue fences are left alone. This lets chapter-refresh work opt
snippets in as they become complete, self-contained examples.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


FENCE_RE = re.compile(r"^```(?P<info>.*)$")
CHECK_ATTRS = {"check", "compile-fail", "skip"}


@dataclass(frozen=True)
class Snippet:
    path: Path
    line: int
    action: str
    source: str

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
                if len(actions) > 1:
                    raise ValueError(
                        f"{path}:{fence_line}: use only one of {sorted(CHECK_ATTRS)}"
                    )
                if actions:
                    action = next(iter(actions))
                    if action != "skip":
                        snippets.append(
                            Snippet(
                                path=path,
                                line=fence_line,
                                action=action,
                                source="".join(body),
                            )
                        )
                else:
                    unmarked_rue_fences += 1

            # Skip the closing fence if present.
            if i < len(lines):
                i += 1

    return snippets, unmarked_rue_fences


def compile_snippet(rue_binary: str, snippet: Snippet, tempdir: Path) -> subprocess.CompletedProcess[str]:
    source_path = tempdir / f"{snippet.path.stem}-{snippet.line}.rue"
    output_path = tempdir / f"{snippet.path.stem}-{snippet.line}"
    source_path.write_text(snippet.source, encoding="utf-8")

    return subprocess.run(
        [rue_binary, str(source_path), "-o", str(output_path)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


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
    args = parser.parse_args()

    snippets, unmarked = collect_snippets(args.root)
    rue_binary = find_rue_binary()

    failures = 0
    with tempfile.TemporaryDirectory(prefix="rue-tutorial-snippets-") as temp:
        tempdir = Path(temp)
        for snippet in snippets:
            result = compile_snippet(rue_binary, snippet, tempdir)
            compiled = result.returncode == 0
            expected_success = snippet.action == "check"

            if compiled == expected_success:
                if not args.quiet:
                    print(f"ok {snippet.label} ({snippet.action})")
                continue

            failures += 1
            expectation = "compile" if expected_success else "fail compilation"
            print(f"FAIL {snippet.label}: expected snippet to {expectation}", file=sys.stderr)
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
