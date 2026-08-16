#!/usr/bin/env python3
"""Validate the notes index and path references inside docs/**/*.md.

Three failure classes (RUE-1531):

1. A note in ``docs/notes/`` (other than ``README.md``) with no row in the
   notes index table (``docs/notes/README.md``).
2. An index row naming a file that does not exist.
3. A dead path reference anywhere under ``docs/**/*.md``: a relative
   markdown link, or a bare ``docs/…``/``crates/…``/``scripts/…`` path-like
   reference, that resolves to nothing. Bare paths are matched narrowly
   (word-bounded, known extensions only); http(s) URLs, anchor-only links,
   and anything inside a fenced code block are ignored. Two site-flavored
   link forms get their own resolution: a Zola ``@/…`` internal link is
   resolved against the containing content tree (the spec sources use these;
   ``website/build.sh`` rewrites them at site build), and a target ending in
   ``/`` is a rendered-page URL (a slugified section address, not a file) and
   is out of this gate's scope.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import List, Optional, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import run_gate, walk_files

ROOT = Path(__file__).resolve().parent.parent

# `[text](target)` inline links and images; the target is everything up to the
# first whitespace or closing parenthesis, so an optional `"title"` is dropped.
MARKDOWN_LINK = re.compile(r"\[[^\]]*\]\(([^()\s]+)(?:\s+\"[^\"]*\")?\)")

# A bare repository path: rooted at docs/, crates/, or scripts/, with a known
# text extension, and word-bounded so a longer path or identifier around it
# cannot produce a partial match.
BARE_PATH = re.compile(
    r"(?<![\w/.-])"
    r"((?:docs|crates|scripts)/[A-Za-z0-9_./-]+\.(?:md|rs|py|toml))"
    r"(?![\w-])"
)

# A fence opener/closer: three or more backticks or tildes at the start of the
# (possibly indented) line.
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")

# The leading cell of a notes-index table row: either a markdown link or a
# (possibly backticked) bare file name.
INDEX_ROW = re.compile(
    r"^\|\s*(?:\[[^\]]*\]\(([^()\s]+)\)|`?([A-Za-z0-9_.-]+\.md)`?)\s*\|"
)


def prose_lines(text: str) -> List[Tuple[int, str]]:
    """``(line_number, line)`` for every line outside fenced code blocks."""
    kept: List[Tuple[int, str]] = []
    fence: Optional[str] = None
    for number, line in enumerate(text.splitlines(), start=1):
        match = FENCE.match(line)
        if match:
            marker = match.group(1)[0]
            if fence is None:
                fence = marker
            elif marker == fence:
                fence = None
            continue
        if fence is None:
            kept.append((number, line))
    return kept


def link_targets(line: str) -> List[str]:
    """Checkable targets on one prose line, markdown links first."""
    targets: List[str] = []
    spans: List[Tuple[int, int]] = []
    for match in MARKDOWN_LINK.finditer(line):
        targets.append(match.group(1))
        spans.append(match.span())
    for match in BARE_PATH.finditer(line):
        # A bare-path hit inside a markdown link is the same reference; the
        # link's own resolution already covers it.
        if any(start <= match.start() < end for start, end in spans):
            continue
        targets.append(match.group(1))
    return targets


def resolves(target: str, file_dir: Path, root: Path) -> bool:
    """Whether ``target`` names something real, file-relative or root-relative."""
    candidate = target.split("#", 1)[0]
    if not candidate:
        return True
    if candidate.startswith("@/"):
        # Zola internal link: relative to the content root, which is some
        # ancestor of the file (docs/spec/src for the spec tree). Try each
        # ancestor up to the repository root.
        stripped = candidate[2:]
        directory = file_dir
        while True:
            if (directory / stripped).exists():
                return True
            if directory == root:
                return False
            directory = directory.parent
    if (file_dir / candidate).exists():
        return True
    return (root / candidate).exists()


def dead_reference_errors(root: Path) -> List[str]:
    errors: List[str] = []
    docs = root / "docs"
    for path in walk_files(docs, keep=lambda p: p.suffix == ".md"):
        file_dir = path.parent
        for number, line in prose_lines(path.read_text()):
            for target in link_targets(line):
                stripped = target.split("#", 1)[0]
                if not stripped or re.match(r"^[a-z][a-z0-9+.-]*:", target):
                    continue  # anchor-only, or an http(s)/mailto/… URL
                if stripped.endswith("/"):
                    continue  # rendered-page URL (slugified section address)
                if not resolves(target, file_dir, root):
                    errors.append(
                        f"{path.relative_to(root)}:{number}: "
                        f"dead reference {target!r}"
                    )
    return errors


def notes_index_errors(root: Path) -> List[str]:
    notes = root / "docs" / "notes"
    readme = notes / "README.md"
    if not readme.is_file():
        return [f"{readme.relative_to(root)}: notes index is missing"]

    listed: List[str] = []
    for _, line in prose_lines(readme.read_text()):
        match = INDEX_ROW.match(line)
        if not match:
            continue
        name = match.group(1) or match.group(2)
        if name in ("File", "---"):
            continue
        listed.append(name.split("#", 1)[0])

    errors: List[str] = []
    for name in listed:
        if not (notes / name).is_file():
            errors.append(
                f"{readme.relative_to(root)}: index row names "
                f"{name!r}, which does not exist"
            )
    listed_set = set(listed)
    for path in sorted(notes.glob("*.md")):
        if path.name == "README.md":
            continue
        if path.name not in listed_set:
            errors.append(
                f"{path.relative_to(root)}: note has no row in the "
                "notes index (docs/notes/README.md)"
            )
    return errors


def validate(root: Path) -> List[str]:
    return notes_index_errors(root) + dead_reference_errors(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=ROOT,
        help="repository root containing docs/ (default: this checkout)",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    return run_gate(
        lambda: validate(root),
        "doc links valid: notes index complete, no dead references",
    )


if __name__ == "__main__":
    raise SystemExit(main())
