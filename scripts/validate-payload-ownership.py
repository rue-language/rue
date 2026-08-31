#!/usr/bin/env python3
"""Fail closed when typed IR payload storage escapes its owning module."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import parse_crate_sources


OWNER_PATHS = {
    # These files are the privacy-nested RIR owner layers. Payload encoding,
    # span rewriting, validation, editing, packed decoding, and their moved
    # owner tests intentionally share descendant-private access to the two
    # physical stores. `inst/printer.rs` is deliberately absent: presentation
    # consumes only typed read views.
    "rue-rir": {
        Path("inst.rs"),
        Path("inst/payload.rs"),
        Path("inst/spans.rs"),
        Path("inst/validation.rs"),
        Path("inst/editor.rs"),
        Path("inst/payload_support.rs"),
        Path("inst/packed.rs"),
        Path("inst/tests.rs"),
    },
    "rue-air": {Path("inst.rs"), Path("inst/payload_support.rs")},
    "rue-cfg": {Path("inst.rs"), Path("inst/payload_metrics.rs"), Path("payload.rs")},
    "rue-codegen": set(),
}

DIRECT_STORE_ACCESS = re.compile(
    r"\b(?:rir|air|cfg)\.(?:instructions|extra|values|call_args|switch_cases|projections|places)\s*"
    r"(?:\[|\.(?:push|extend|extend_from_slice|truncate|get_mut|as_mut|clear|reserve)\b)"
)
PUBLIC_PHYSICAL_STORE = re.compile(
    r"^\s*pub\s+(?:\([^)]*\)\s+)?(?P<field>[A-Za-z0-9_]+)\s*:\s*"
    r"Vec\s*<\s*(?:u32|CfgValue|CfgInst|CfgCallArg|AirPlace|AirProjection|Projection|\(\s*i64\s*,\s*BlockId\s*\))\s*>",
    re.MULTILINE,
)
PUBLIC_STORE_ALLOWLIST = {
    ("rue-cfg", Path("inst.rs"), "insts"),  # typed block membership, not a side table
}
RAW_CFG_ARGUMENT = re.compile(
    r"\bcfg\s*:\s*&\s*(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:rue_cfg::)?Cfg\b"
)
MUTABLE_PHYSICAL_STORE_RETURN = re.compile(
    r"->\s*&\s*mut\s*(?:Vec\s*<\s*)?(?:\[\s*)?"
    r"(?:u32|CfgValue|CfgInst|CfgCallArg|AirPlace|AirProjection|Projection|\(\s*i64\s*,\s*BlockId\s*\))"
)
RAW_RANGE_CONSTRUCTION = re.compile(r"\b[A-Za-z0-9_]*Range::from_parts\s*\(")
PAYLOAD_ENUM_CAST = re.compile(
    r"\b(?:PatternKind|AirPattern|AirProjection|CfgInstData|Terminator)::[A-Za-z0-9_]+\s+as\s+(?:u32|usize)\b"
)


def public_function_headers(source: str) -> list[tuple[int, str]]:
    headers: list[tuple[int, str]] = []
    offset = 0
    while True:
        match = re.search(r"^\s*pub\s+(?:const\s+)?fn\s+", source[offset:], re.MULTILINE)
        if match is None:
            return headers
        start = offset + match.start()
        end = start
        depth = 0
        while end < len(source):
            char = source[end]
            if char == "(":
                depth += 1
            elif char == ")":
                depth = max(depth - 1, 0)
            elif char in "{;" and depth == 0:
                end += 1
                break
            end += 1
        headers.append((source.count("\n", 0, start) + 1, source[start:end]))
        offset = end


def source_files(sources: list[tuple[str, Path]]) -> list[tuple[str, Path, Path]]:
    files: list[tuple[str, Path, Path]] = []
    for crate, root in sources:
        for path in sorted(root.rglob("*.rs")):
            files.append((crate, root, path))
    return files


def validate(sources: list[tuple[str, Path]]) -> list[str]:
    errors: list[str] = []
    for crate, root in sources:
        if not root.exists():
            errors.append(f"missing payload inventory source directory: {root}")

    for crate, root, path in source_files(sources):
        source = path.read_text()
        relative = path.relative_to(root)
        if relative.parts and relative.parts[0] == "src":
            relative = Path(*relative.parts[1:])
        display = Path("crates") / crate / "src" / relative
        owner = relative in OWNER_PATHS[crate]

        if owner:
            for match in PUBLIC_PHYSICAL_STORE.finditer(source):
                key = (crate, relative, match.group("field"))
                if key not in PUBLIC_STORE_ALLOWLIST:
                    line = source.count("\n", 0, match.start()) + 1
                    errors.append(
                        f"{display}:{line}: public mutable physical store field: {match.group(0).strip()}"
                    )

        for pattern, reason in [
            (RAW_RANGE_CONSTRUCTION, "raw range construction outside its declaration"),
            (PAYLOAD_ENUM_CAST, "raw payload enum cast"),
        ]:
            for match in pattern.finditer(source):
                # Range construction and payload-tag encoding are implementation
                # details permitted only in the reviewed owner allowlist.
                if owner and pattern in {RAW_RANGE_CONSTRUCTION, PAYLOAD_ENUM_CAST}:
                    continue
                line = source.count("\n", 0, match.start()) + 1
                errors.append(f"{display}:{line}: {reason}: {match.group(0)!r}")

        if not owner and relative.name not in {"tests.rs", "api_inventory.rs"}:
            for match in DIRECT_STORE_ACCESS.finditer(source):
                line = source.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{display}:{line}: direct side-table access outside owner: {match.group(0)!r}"
                )

        for line, header in public_function_headers(source):
            compact = " ".join(header.split())
            raw_shape = any(
                token in compact
                for token in (
                    "-> &[u32]",
                    "-> &mut [u32]",
                    "-> &Vec<u32>",
                    "-> &mut Vec<u32>",
                    "-> (u32, u32)",
                    "start: u32, extent: u32",
                )
            ) or RAW_CFG_ARGUMENT.search(compact) or MUTABLE_PHYSICAL_STORE_RETURN.search(compact)
            allocating_getter = re.search(r"\bfn\s+(?:get|read|decode)_[A-Za-z0-9_]+", compact) and re.search(
                r"->\s+(?:Result<)?Vec<", compact
            )
            if raw_shape or allocating_getter:
                errors.append(f"{display}:{line}: unreviewed public payload API: {compact}")

    return errors


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", action="append", default=[], metavar="CRATE=DIR")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        sources = parse_crate_sources(args.source, OWNER_PATHS)
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    errors = validate(sources)
    if errors:
        print("typed payload ownership inventory failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print("typed payload ownership inventory passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
