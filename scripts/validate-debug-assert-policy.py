#!/usr/bin/env python3
"""Enforce Rue's reviewed production debug-assertion budget.

`debug_assert!` is useful for redundant development checks, but it must never
be the only barrier between malformed compiler state and wrong generated code.
Every surviving production use is therefore an explicit reviewed exception.
Changing, adding, or removing one requires updating this ledger with its
rationale; the lowering, codegen, and linker crates intentionally have none.

The gate runs per crate (RUE-1525): `--crate NAME --sources DIR` scans one
crate's src/ tree and compares it against this ledger's entries for that
crate, so each crate's check is a Buck test target emitted by the
rue_crate/rue_binary macros and re-runs only when that crate changes.
`--ledger-crates rust-project.json` is the structural complement: it fails
when a ledger entry names a crate that no longer exists, which the per-crate
checks cannot see because a deleted crate no longer emits one.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import NamedTuple

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import mask_rust_non_code, run_gate


DEBUG_ASSERT = re.compile(r"\bdebug_assert(?:_eq|_ne)?!\s*\(")


class Allowance(NamedTuple):
    count: int
    rationale: str


# Exact per-file counts make this a ratchet rather than a blanket exemption:
# even deleting an old use requires reviewing and reducing the ledger.
ALLOWANCES = {
    "crates/rue-air/src/inst.rs": Allowance(1, "redundant fixed-width AIR encoding check"),
    "crates/rue-air/src/intern_pool.rs": Allowance(3, "redundant arena and traversal bookkeeping checks"),
    "crates/rue-air/src/sema/analysis/builtin_ops.rs": Allowance(1, "redundant error-recovery state check"),
    "crates/rue-air/src/sema/analysis/calls.rs": Allowance(1, "redundant parameter metadata width check"),
    "crates/rue-air/src/sema/analysis/intrinsics.rs": Allowance(1, "redundant intrinsic signature inventory check"),
    "crates/rue-air/src/sema/analysis/ownership.rs": Allowance(
        2,
        "redundant non-negative arena-index check; redundant cross-check that"
        " the authoritative declared-linear bit agrees with the old"
        " diagnostics-derived classification (RUE-1604)",
    ),
    "crates/rue-air/src/sema/body_identity.rs": Allowance(
        2, "redundant anonymous-identity publication and disjointness checks"
    ),
    "crates/rue-air/src/sema/comptime_eval.rs": Allowance(2, "redundant semantic phase preconditions"),
    "crates/rue-air/src/sema/declaration_index.rs": Allowance(4, "redundant declaration indexing counters"),
    "crates/rue-air/src/sema/ordinary_engine.rs": Allowance(2, "redundant parameter-mode and ABI accounting checks"),
    "crates/rue-compiler/src/artifact_views.rs": Allowance(5, "redundant source/RIR view owner and bounds checks"),
    "crates/rue-compiler/src/body_query.rs": Allowance(
        1, "redundant canonical body-reference ordering check"
    ),
    "crates/rue-compiler/src/diagnostic_attempt_store.rs": Allowance(2, "redundant diagnostic retention accounting"),
    "crates/rue-compiler/src/parsed_modules.rs": Allowance(1, "redundant source ownership check"),
    "crates/rue-compiler/src/revisioned_query_database/parse_import/program_assembly.rs": Allowance(
        1, "redundant source-stamp retention accounting check"
    ),
    "crates/rue-compiler/src/revisioned_query_database/registrations/body/body_reachability.rs": Allowance(
        1, "redundant pending-frontier disjointness check"
    ),
    "crates/rue-compiler/src/revisioned_query_database/registrations/semantic/declaration_semantics_publications.rs": Allowance(
        1, "redundant declaration publication state check"
    ),
    "crates/rue-compiler/src/semantic_query_nucleus.rs": Allowance(1, "redundant semantic query category check"),
    "crates/rue-compiler/src/session.rs": Allowance(3, "redundant rooted-session phase and provenance checks"),
    "crates/rue-compiler/src/source_identity.rs": Allowance(4, "redundant normalized-path representation checks"),
    "crates/rue-compiler/src/source_snapshot.rs": Allowance(1, "redundant source arena identity check"),
    "crates/rue-error/src/lib.rs": Allowance(1, "redundant diagnostic rendering bounds check"),
    "crates/rue-oracle/src/lib.rs": Allowance(
        5,
        "oracle harness ABI, representation-vector, and typed-view bookkeeping"
        " checks, not compiler correctness gates",
    ),
    "crates/rue-parser/src/parser/shared.rs": Allowance(2, "redundant parser entry preconditions"),
    "crates/rue-parser/src/parser/statements.rs": Allowance(1, "redundant parser entry precondition"),
    "crates/rue-rir/src/inst/payload.rs": Allowance(
        1, "redundant RIR variable-width encoding check"
    ),
    "crates/rue/src/emit.rs": Allowance(1, "validated sole-dependency emit mode"),
    "crates/rue/src/source_loader.rs": Allowance(1, "redundant source-plan revision check"),
}


def count_debug_asserts(path: Path) -> int:
    return len(DEBUG_ASSERT.findall(mask_rust_non_code(path.read_text())[0]))


def compare(actual: dict[str, int], allowances: dict[str, Allowance]) -> list[str]:
    """The per-file comparison: exact reviewed counts, nothing unreviewed."""
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


def validate_crate(
    crate: str, sources: Path, allowances: dict[str, Allowance] = ALLOWANCES
) -> list[str]:
    """Scan one crate's src/ tree against its slice of the ledger.

    `sources` is the crate's src/ directory; each file maps to the ledger key
    `crates/<crate>/src/<relative>`, matching how the repository-wide walk
    used to key the same file. The comparison covers every ledger entry under
    `crates/<crate>/`, so an allowance for a file the scan cannot reach
    (deleted, or outside src/) fails as drift instead of going silently
    unchecked.
    """
    if not sources.is_dir():
        return [f"{crate}: sources directory {sources} does not exist"]
    files = sorted(sources.rglob("*.rs"))
    if not files:
        return [
            f"{crate}: no .rs files under {sources}; "
            "the check's wiring is broken, this is never a clean crate"
        ]
    actual = {
        f"crates/{crate}/src/{path.relative_to(sources).as_posix()}": count_debug_asserts(path)
        for path in files
    }
    actual = {path: count for path, count in actual.items() if count}
    prefix = f"crates/{crate}/"
    scoped = {path: a for path, a in allowances.items() if path.startswith(prefix)}
    return compare(actual, scoped)


def first_party_crates(project: dict) -> set[str]:
    """Crate directory names rust-project.json knows as first-party.

    First-party entries are the ones whose root_module lives under crates/;
    third-party and sysroot crates root elsewhere.
    """
    names = set()
    for crate in project.get("crates", []):
        root_module = crate.get("root_module", "")
        if root_module.startswith("crates/"):
            names.add(root_module.split("/")[1])
    return names


def validate_ledger_crates(
    project: dict, allowances: dict[str, Allowance] = ALLOWANCES
) -> list[str]:
    """Fail-closed check that every ledger entry names a crate that exists.

    The per-crate checks only run for crates that emit them, so deleting a
    crate would otherwise leave its ledger entries permanently unchecked —
    an allowance nothing enforces. This mode fails on such orphans.
    """
    crates = first_party_crates(project)
    if not crates:
        return [
            "rust-project.json lists no first-party crates; "
            "a broken project description is never a clean ledger"
        ]
    errors: list[str] = []
    for path in sorted(allowances):
        parts = path.split("/")
        if len(parts) < 3 or parts[0] != "crates":
            errors.append(f"{path}: ledger keys must be crates/<name>/... paths")
        elif parts[1] not in crates:
            errors.append(
                f"{path}: ledger names crate '{parts[1]}', which rust-project.json "
                "does not know; remove the orphaned allowance"
            )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crate", help="crate name to check (with --sources)")
    parser.add_argument(
        "--sources", type=Path, help="the crate's src/ directory (with --crate)"
    )
    parser.add_argument(
        "--ledger-crates",
        type=Path,
        help="rust-project.json; verify every ledger entry names a first-party crate",
    )
    args = parser.parse_args()

    if args.ledger_crates is not None:
        if args.crate is not None or args.sources is not None:
            parser.error("--ledger-crates does not combine with --crate/--sources")
        project = json.loads(args.ledger_crates.read_text())
        crate_names = sorted({path.split("/")[1] for path in ALLOWANCES})
        return run_gate(
            lambda: validate_ledger_crates(project),
            f"debug-assert ledger valid: {len(ALLOWANCES)} entries across "
            f"{len(crate_names)} existing crate(s)",
        )

    if args.crate is None or args.sources is None:
        parser.error("pass --crate NAME with --sources DIR, or --ledger-crates FILE")

    prefix = f"crates/{args.crate}/"
    reviewed = sum(
        allowance.count
        for path, allowance in ALLOWANCES.items()
        if path.startswith(prefix)
    )
    return run_gate(
        lambda: validate_crate(args.crate, args.sources),
        f"debug-assert policy valid for {args.crate}: {reviewed} reviewed assertion(s)",
    )


if __name__ == "__main__":
    raise SystemExit(main())
