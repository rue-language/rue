#!/usr/bin/env python3
"""Ban early-exit pipeline consumers in shell scripts that enable `pipefail`.

`grep -q` (and `-l`, `-m N`, and `head -n`) stop reading as soon as they have
their answer. That closes the pipe, the producer upstream dies of EPIPE, and
under `set -o pipefail` the shell reports *the producer's* status for the whole
pipeline. A test that matched therefore reports "no match", and a gate that
found a violation reports a pass.

The failure is inverted, which is what makes it durable: the broken pipe only
happens when the consumer DID match, so a clean tree exercises the working path
exclusively and cannot distinguish this construct from a correct one. It has
reached trunk three times -- RUE-1011 (scripts/test-wrapper-scripts.sh),
RUE-1155 (the then-required clippy.sh lint gate, plus three more sites) -- so it is
checked mechanically rather than by review.

The fix is to remove the pipe: `grep -q PATTERN <<<"$text"`, a `case`, or
`[[ ]]`. Where a pipeline is genuinely required, annotate the line with
`# pipefail-pipeline-ok: <reason>` and the reason is reviewed here instead.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
# SKIP_DIRECTORIES is re-exported: the tool tests read the prune policy
# through this module as the gate's own surface.
from gatelib import SKIP_DIRECTORIES, run_gate, walk_files


ROOT = Path(__file__).resolve().parent.parent

SHEBANG = re.compile(r"^#!.*\b(?:ba|da|k|z)?sh\b")

# `set -o pipefail`, `set -euo pipefail`, `set -uo pipefail`, ...
PIPEFAIL = re.compile(r"^\s*set\s+(?:-[a-zA-Z]*o|-o)\s+pipefail\b", re.MULTILINE)

# Consumers that stop reading before their input is exhausted. `grep` only
# qualifies with a short-circuiting flag: a plain `grep` drains its input and is
# safe. `-l`/`-L` stop at the first matching file, `-m N` after N matches.
# The flag letter may sit anywhere in a short-option cluster (`-q`, `-qE`,
# `-Fxq`), so match the cluster rather than requiring a word boundary after the
# letter -- `-qE` is precisely what the clippy gate used.
EARLY_EXIT = re.compile(
    r"""^(?:
          (?:\S*/)?(?:[ezf]?grep|rg) \b (?=[^\n]*\s-[a-zA-Z]*[qlLm])
        | (?:\S*/)?head \b
        )""",
    re.VERBOSE,
)

ALLOW = re.compile(r"#\s*pipefail-pipeline-ok:\s*(?P<reason>\S.*?)\s*$")


class Finding(NamedTuple):
    path: str
    line: int
    stage: str

    def __str__(self) -> str:
        return (
            f"{self.path}:{self.line}: early-exit consumer `{self.stage}` reads from a "
            "pipe under `set -o pipefail`; a match kills the producer with EPIPE and the "
            "pipeline reports failure. Use a herestring/case, or annotate with "
            "`# pipefail-pipeline-ok: <reason>`"
        )


def strip_comment(line: str) -> str:
    """Drop a trailing `#` comment, respecting quotes.

    Comments matter here because this policy's own prose names the construct it
    bans, as do the fix notes left at each repaired site.
    """
    quote = ""
    for index, character in enumerate(line):
        if quote:
            if character == quote:
                quote = ""
            elif character == "\\" and quote == '"':
                pass
        elif character in "'\"":
            quote = character
        elif character == "#" and (index == 0 or line[index - 1].isspace()):
            return line[:index]
    return line


def split_pipeline(line: str) -> list[str]:
    """Split on `|`, keeping `||`, `|&`, and quoted pipes intact.

    A `$(...)` body is its own quoting context, so `v="$(cat f | grep -q x)"`
    contains a genuine pipeline even though the `|` sits inside double quotes.
    """
    stages: list[str] = []
    current: list[str] = []
    quote = ""
    enclosing: list[str] = []
    index = 0
    while index < len(line):
        character = line[index]
        if quote == "'":
            if character == "'":
                quote = ""
            current.append(character)
        elif line[index : index + 2] == "$(":
            enclosing.append(quote)
            quote = ""
            current.append(line[index : index + 2])
            index += 2
            continue
        elif character == ")" and enclosing and not quote:
            quote = enclosing.pop()
            current.append(character)
        elif quote:
            if character == quote:
                quote = ""
            current.append(character)
        elif character in "'\"":
            quote = character
            current.append(character)
        elif character == "|":
            if line[index : index + 2] in ("||", "|&"):
                current.append(line[index : index + 2])
                index += 2
                continue
            stages.append("".join(current))
            current = []
        else:
            current.append(character)
        index += 1
    stages.append("".join(current))
    return stages


def shell_scripts(root: Path) -> list[Path]:
    paths: list[Path] = []
    # walk_files prunes while walking rather than filtering afterwards:
    # `buck-out` alone holds enough build output to dominate the gate's
    # runtime.
    for path in walk_files(root):
        if path.is_symlink() or not path.is_file():
            continue
        if path.suffix and path.suffix != ".sh":
            continue
        try:
            with path.open("r", encoding="utf-8", errors="strict") as handle:
                first = handle.readline()
        except (OSError, UnicodeDecodeError):
            continue
        if path.suffix == ".sh" or SHEBANG.match(first):
            paths.append(path)
    return sorted(paths)


def scan(source: str, path: str) -> list[Finding]:
    if not PIPEFAIL.search(source):
        return []

    findings: list[Finding] = []
    for number, raw in enumerate(source.splitlines(), start=1):
        if ALLOW.search(raw):
            continue
        # A pipeline stage may begin after `(`, `$(`, `&&`, `;`, `then`, ... so
        # compare against the stage's first word rather than the line's.
        stages = split_pipeline(strip_comment(raw))
        for stage in stages[1:]:
            word = stage.strip().lstrip("(").strip()
            if EARLY_EXIT.match(word):
                findings.append(Finding(path, number, word.split()[0]))
    return findings


def validate(root: Path) -> list[Finding]:
    findings: list[Finding] = []
    for path in shell_scripts(root):
        relative = path.relative_to(root).as_posix()
        findings.extend(scan(path.read_text(encoding="utf-8"), relative))
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=ROOT)
    args = parser.parse_args()

    scanned = len(shell_scripts(args.root))
    return run_gate(
        lambda: [str(finding) for finding in validate(args.root)],
        f"pipefail pipeline policy valid: {scanned} shell script(s) scanned",
    )


if __name__ == "__main__":
    raise SystemExit(main())
