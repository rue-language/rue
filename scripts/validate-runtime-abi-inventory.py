#!/usr/bin/env python3
"""Reject raw runtime-helper spellings in compiler consumers.

The typed manifest owns helper symbols. Compiler phases and auxiliary
consumers carry typed identities; only tests, generated-symbol namespaces, and
the strict archive/version boundary may mention reserved spellings directly.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import mask_rust_non_code


ROOT = Path(__file__).resolve().parent.parent
CONSUMER_CRATES = (
    "rue-builtins",
    "rue-air",
    "rue-cfg",
    "rue-codegen",
    "rue-compiler",
    "rue-linker",
    "rue-oracle",
)
LITERAL = re.compile(r'"(__rue_[A-Za-z0-9_$]*)"')
TEST_MODULE = re.compile(r"#\[cfg\(test\)\]\s*mod\s+[A-Za-z0-9_]+\s*\{", re.MULTILINE)

# These are namespace or generated-symbol boundaries, not runtime-helper
# identities. Keep this list about categories rather than individual helpers.
ALLOWED_PRODUCTION = (
    "__rue_",  # source-name reservation policy
    "__rue_runtime_abi_v",  # strict archive version-marker namespace
    "__rue_fn_",  # compiler-generated Rue functions
    "__rue_drop_",  # compiler-generated destructor glue
    "__rue_drop_array_",  # compiler-generated array destructor glue
    "__rue_for_",  # internal desugaring temporaries
    "__rue_invalid_local_symbol",  # unreachable canonicalization sentinel
)


def test_module_spans(masked_source: str) -> list[tuple[int, int]]:
    spans = []
    for match in TEST_MODULE.finditer(masked_source):
        opening = masked_source.find("{", match.start(), match.end())
        depth = 1
        index = opening + 1
        while index < len(masked_source) and depth:
            if masked_source[index] == "{":
                depth += 1
            elif masked_source[index] == "}":
                depth -= 1
            index += 1
        spans.append((match.start(), index))
    return spans


def contained(offset: int, spans: list[tuple[int, int]]) -> bool:
    return any(start <= offset < end for start, end in spans)


def is_test_path(path: Path) -> bool:
    return path.name in {"tests.rs", "pipeline_tests.rs", "integration_tests.rs"} or "tests" in path.parts


def allowed_production_literal(value: str) -> bool:
    return value in ALLOWED_PRODUCTION


def validate(root: Path, sources: list[tuple[str, Path]] | None = None) -> list[str]:
    errors: list[str] = []
    crate_sources = sources or [
        (crate, root / "crates" / crate / "src") for crate in CONSUMER_CRATES
    ]
    for crate, src in crate_sources:
        if not src.exists():
            errors.append(f"missing compiler-consumer source directory: {src}")
            continue
        for path in sorted(src.rglob("*.rs")):
            source = path.read_text()
            masked, comments = mask_rust_non_code(source)
            tests = test_module_spans(masked)
            for match in LITERAL.finditer(source):
                value = match.group(1)
                if contained(match.start(), comments):
                    continue
                if is_test_path(path) or contained(match.start(), tests):
                    continue
                if allowed_production_literal(value):
                    continue
                line = source.count("\n", 0, match.start()) + 1
                display = (
                    path.relative_to(root)
                    if path.is_relative_to(root)
                    else Path("crates") / crate / "src" / path.relative_to(src)
                )
                errors.append(
                    f"{display}:{line}: raw reserved symbol literal {value!r}; "
                    "carry a typed runtime/helper identity or classify a non-runtime namespace"
                )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument(
        "--source",
        action="append",
        default=[],
        metavar="CRATE=PATH",
        help="scan one Buck-materialized crate source directory",
    )
    args = parser.parse_args()

    root = args.root.resolve()
    sources = []
    for item in args.source:
        crate, separator, path = item.partition("=")
        if not separator or crate not in CONSUMER_CRATES:
            parser.error(f"invalid --source {item!r}")
        sources.append((crate, Path(path).resolve()))
    errors = validate(root, sources or None)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print("runtime ABI inventory valid: no raw helper literals in compiler consumers")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
