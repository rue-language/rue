#!/usr/bin/env python3
"""Enforce Rue's reviewed production debug-assertion budget.

`debug_assert!` is useful for redundant development checks, but it must never
be the only barrier between malformed compiler state and wrong generated code.
Every surviving production use is therefore an explicit reviewed exception.
Changing, adding, or removing one requires updating this ledger with its
rationale; the lowering, codegen, and linker crates intentionally have none.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import NamedTuple


ROOT = Path(__file__).resolve().parent.parent
DEBUG_ASSERT = re.compile(r"\bdebug_assert(?:_eq|_ne)?!\s*\(")
RAW_STRING_START = re.compile(r"(?:br|r)(?P<hashes>#{0,255})\"")
CHARACTER_LITERAL_START = re.compile(r"'(?:\\.|[^'\\\n])+'")


class Allowance(NamedTuple):
    count: int
    rationale: str


# Exact per-file counts make this a ratchet rather than a blanket exemption:
# even deleting an old use requires reviewing and reducing the ledger.
ALLOWANCES = {
    "crates/rue-air/src/inst.rs": Allowance(1, "redundant fixed-width AIR encoding check"),
    "crates/rue-air/src/intern_pool.rs": Allowance(3, "redundant arena and traversal bookkeeping checks"),
    "crates/rue-air/src/sema/analysis.rs": Allowance(20, "redundant semantic index and phase-state coherence checks"),
    "crates/rue-air/src/sema/analysis/builtin_ops.rs": Allowance(1, "redundant error-recovery state check"),
    "crates/rue-air/src/sema/analysis/calls.rs": Allowance(1, "redundant parameter metadata width check"),
    "crates/rue-air/src/sema/analysis/intrinsics.rs": Allowance(1, "redundant intrinsic signature inventory check"),
    "crates/rue-air/src/sema/analysis/ownership.rs": Allowance(1, "redundant non-negative arena-index check"),
    "crates/rue-air/src/sema/binding_manifest.rs": Allowance(2, "redundant binding-manifest construction checks"),
    "crates/rue-air/src/sema/comptime_eval.rs": Allowance(2, "redundant semantic phase preconditions"),
    "crates/rue-air/src/sema/declaration_index.rs": Allowance(4, "redundant declaration indexing counters"),
    "crates/rue-air/src/sema/declarations.rs": Allowance(7, "redundant declaration-resolution lifecycle checks"),
    "crates/rue-air/src/sema/ordinary_engine.rs": Allowance(2, "redundant parameter-mode and ABI accounting checks"),
    "crates/rue-air/src/sema/typeck.rs": Allowance(1, "redundant parameter flag cardinality check"),
    "crates/rue-compiler/src/artifact_views.rs": Allowance(7, "redundant typed-view owner and bounds checks"),
    "crates/rue-compiler/src/body_query.rs": Allowance(
        1, "redundant canonical body-reference ordering check"
    ),
    "crates/rue-compiler/src/definition_snapshot.rs": Allowance(1, "redundant definition arena identity check"),
    "crates/rue-compiler/src/diagnostic_attempt_store.rs": Allowance(2, "redundant diagnostic retention accounting"),
    "crates/rue-compiler/src/parsed_modules.rs": Allowance(1, "redundant source ownership check"),
    "crates/rue-compiler/src/revisioned_query_database.rs": Allowance(
        1, "redundant memo-retention accounting"
    ),
    "crates/rue-compiler/src/semantic_query_nucleus.rs": Allowance(1, "redundant semantic query category check"),
    "crates/rue-compiler/src/session.rs": Allowance(6, "redundant canonical-session phase and provenance checks"),
    "crates/rue-compiler/src/source_identity.rs": Allowance(4, "redundant normalized-path representation checks"),
    "crates/rue-compiler/src/source_snapshot.rs": Allowance(1, "redundant source arena identity check"),
    "crates/rue-error/src/lib.rs": Allowance(1, "redundant diagnostic rendering bounds check"),
    "crates/rue-oracle/src/lib.rs": Allowance(2, "oracle harness bookkeeping checks, not compiler correctness gates"),
    "crates/rue-parser/src/parser/shared.rs": Allowance(2, "redundant parser entry preconditions"),
    "crates/rue-parser/src/parser/statements.rs": Allowance(1, "redundant parser entry precondition"),
    "crates/rue-rir/src/inst.rs": Allowance(1, "redundant RIR variable-width encoding check"),
    "crates/rue-span/src/lib.rs": Allowance(1, "redundant span construction bounds check"),
    "crates/rue/src/emit.rs": Allowance(1, "validated sole-dependency emit mode"),
    "crates/rue/src/source_loader.rs": Allowance(1, "redundant source-plan revision check"),
}


def strip_non_code(source: str) -> str:
    """Replace Rust comments and string/character contents with whitespace."""

    result = list(source)
    index = 0
    block_depth = 0
    state = "code"
    raw_hashes = 0

    def blank(position: int) -> None:
        if result[position] != "\n":
            result[position] = " "

    while index < len(source):
        if state == "line_comment":
            if source[index] == "\n":
                state = "code"
            else:
                blank(index)
            index += 1
            continue

        if state == "block_comment":
            if source.startswith("/*", index):
                blank(index)
                blank(index + 1)
                block_depth += 1
                index += 2
            elif source.startswith("*/", index):
                blank(index)
                blank(index + 1)
                block_depth -= 1
                index += 2
                if block_depth == 0:
                    state = "code"
            else:
                blank(index)
                index += 1
            continue

        if state == "quoted":
            if source[index] == "\\":
                blank(index)
                if index + 1 < len(source):
                    blank(index + 1)
                index += 2
            else:
                character = source[index]
                blank(index)
                index += 1
                if character == '"':
                    state = "code"
            continue

        if state == "character":
            if source[index] == "\\":
                blank(index)
                if index + 1 < len(source):
                    blank(index + 1)
                index += 2
            else:
                character = source[index]
                blank(index)
                index += 1
                if character == "'":
                    state = "code"
            continue

        if state == "raw":
            terminator = '"' + ("#" * raw_hashes)
            if source.startswith(terminator, index):
                for offset in range(len(terminator)):
                    blank(index + offset)
                index += len(terminator)
                state = "code"
            else:
                blank(index)
                index += 1
            continue

        if source.startswith("//", index):
            blank(index)
            blank(index + 1)
            index += 2
            state = "line_comment"
            continue
        if source.startswith("/*", index):
            blank(index)
            blank(index + 1)
            index += 2
            block_depth = 1
            state = "block_comment"
            continue

        raw_match = RAW_STRING_START.match(source, index)
        if raw_match:
            token = raw_match.group(0)
            raw_hashes = len(raw_match.group("hashes"))
            for offset in range(len(token)):
                blank(index + offset)
            index += len(token)
            state = "raw"
            continue

        if source.startswith('b"', index):
            blank(index)
            blank(index + 1)
            index += 2
            state = "quoted"
            continue
        if source[index] == '"':
            blank(index)
            index += 1
            state = "quoted"
            continue

        # Treat a quote as a character literal only when it has a closing quote
        # on this line; this avoids mistaking Rust lifetimes for literals.
        if source[index] == "'" and CHARACTER_LITERAL_START.match(source, index):
            blank(index)
            index += 1
            state = "character"
            continue

        index += 1

    return "".join(result)


def count_debug_asserts(path: Path) -> int:
    return len(DEBUG_ASSERT.findall(strip_non_code(path.read_text())))


def validate(root: Path, allowances: dict[str, Allowance] = ALLOWANCES) -> list[str]:
    actual = {
        path.relative_to(root).as_posix(): count_debug_asserts(path)
        for path in sorted((root / "crates").glob("**/*.rs"))
    }
    actual = {path: count for path, count in actual.items() if count}

    errors: list[str] = []
    for path in sorted(set(actual) | set(allowances)):
        observed = actual.get(path, 0)
        allowance = allowances.get(path)
        if allowance is None:
            errors.append(
                f"{path}: found {observed} unreviewed debug assertion(s); "
                "use an always-on assertion/diagnostic, or add a justified allowance"
            )
        elif observed != allowance.count:
            errors.append(
                f"{path}: debug-assertion ledger expects {allowance.count}, found {observed}; "
                "review the change and update or remove its allowance"
            )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=ROOT)
    args = parser.parse_args()

    errors = validate(args.root)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    total = sum(allowance.count for allowance in ALLOWANCES.values())
    print(
        "debug-assertion policy valid: "
        f"{total} reviewed debug-only assertion(s), none in CFG/codegen/linker"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
