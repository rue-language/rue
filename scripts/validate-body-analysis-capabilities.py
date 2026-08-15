#!/usr/bin/env python3
"""Guard the provider-native body-analysis capability boundary.

The cutover is complete: production body analysis receives immutable provider
capabilities and request-local state. This guard fails if the provider host or
its public contract regains a whole-program semantic authority, a merged
program, or direct access to declaration-universe tables.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import mask_rust_non_code


INVENTORY_FILES = {
    ("rue-air", Path("sema/provider.rs")),
    ("rue-air", Path("sema/provider_body_host.rs")),
    ("rue-compiler", Path("body_query.rs")),
}

FORBIDDEN_TYPES = (
    "Sema",
    "BoundSema",
    "CanonicalMergedProgram",
    "DeclarationNamespace",
    "BodyOverlay",
)
FORBIDDEN_TABLES = (
    "declarations",
    "enums_by_file_name",
    "functions",
    "functions_by_file_name",
    "methods",
    "module_registry",
    "named_method_declarations",
    "structs_by_file_name",
)

TYPE_ACCESS = re.compile(
    r"\b(?P<name>" + "|".join(FORBIDDEN_TYPES) + r")\b"
)
TABLE_ACCESS = re.compile(
    r"\.\s*(?P<name>" + "|".join(FORBIDDEN_TABLES) + r")\b"
)


@dataclass(frozen=True)
class Hit:
    crate: str
    path: Path
    line: int
    capability: str

    def display(self) -> str:
        return (
            f"crates/{self.crate}/src/{self.path}:{self.line}: "
            f"provider boundary exposes {self.capability}"
        )


def normalize_relative(path: Path, source_root: Path) -> Path:
    relative = path.relative_to(source_root)
    if relative.parts and relative.parts[0] == "src":
        return Path(*relative.parts[1:])
    return relative


def classify(sources: list[tuple[str, Path]]) -> list[Hit]:
    hits: list[Hit] = []
    roots = {crate: root for crate, root in sources}
    for crate, relative in sorted(INVENTORY_FILES, key=lambda item: (item[0], str(item[1]))):
        source_root = roots.get(crate)
        if source_root is None:
            continue
        path = source_root / relative
        if not path.exists():
            # Buck filegroups materialize the crate's `src/` directory beneath
            # the artifact root, while focused tool tests pass that directory
            # itself. Accept both layouts without weakening the required-file
            # inventory.
            path = source_root / "src" / relative
        if not path.exists():
            hits.append(Hit(crate, relative, 0, "missing required provider source"))
            continue
        source = path.read_text()
        masked = mask_rust_non_code(source)[0]
        for pattern, label in (
            (TYPE_ACCESS, "whole-program type"),
            (TABLE_ACCESS, "declaration-universe table"),
        ):
            for match in pattern.finditer(masked):
                line = source.count("\n", 0, match.start()) + 1
                hits.append(
                    Hit(crate, relative, line, f"{label} `{match.group('name')}`")
                )
    return hits


def errors_for(hits: list[Hit]) -> list[str]:
    return [hit.display() for hit in hits]


def parse_sources(items: list[str]) -> list[tuple[str, Path]]:
    sources = []
    for item in items:
        crate, separator, path = item.partition("=")
        if not separator or crate not in {"rue-air", "rue-compiler"}:
            raise ValueError(f"invalid --source {item!r}")
        sources.append((crate, Path(path).resolve()))
    return sources


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", action="append", default=[], metavar="CRATE=PATH")
    args = parser.parse_args()
    try:
        sources = parse_sources(args.source)
    except ValueError as error:
        parser.error(str(error))
    if not sources:
        parser.error("at least one --source is required")

    hits = classify(sources)
    if hits:
        print("body-analysis capability boundary failed:", file=sys.stderr)
        for error in errors_for(hits):
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("body-analysis capability boundary valid: provider-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
