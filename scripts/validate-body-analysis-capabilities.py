#!/usr/bin/env python3
"""Inventory epoch-table capabilities visible to exact-one-body analysis.

This is intentionally a static source inventory. It observes and attributes
epoch reads without adding a production hook to the analyzer. The flip changes
``PROVIDER_ONLY_MODE`` to ``True``; until then, reads owned by an epoch-backed
facts implementation or the legacy ``impl Sema`` epoch authority are reported
as allowed rather than rejected.

Whitelist carry-forwards from ``rue-1091-analyzer-rewire-plan.md``:

* ``outer_named_callable_symbols`` — r5 replaces the driver enumeration with
  demand-population; it currently interns named callable symbols deterministically.
* ``outer_unused_function_warnings`` — r5 supplies the warning pass's universe
  successor; this scan is driver-level rather than one-body resolution.
* ``outer_anonymous_destructor_enqueue`` — r5 supplies the demand-populated
  destructor frontier; drop glue currently makes this a driver-level root scan.
* ``universe_cardinality_counters`` — the flip removes the remaining epoch
  cardinality probes after their structural measurements have served their purpose.
* ``one_body_interner_reads`` — decision pending; the plan provisionally permits
  ``one_body.rs`` to translate stable names and local symbols through the interner.
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path


PROVIDER_ONLY_MODE = False

EPOCH_FIELDS = (
    "anonymous_callable_methods_by_symbol",
    "anonymous_methods",
    "anon_enum_identities",
    "anon_struct_identities",
    "body_owner_tokens",
    "bound_const_candidates",
    "builtin_enums",
    "builtin_structs",
    "canonical_imports",
    "const_candidate_endpoints",
    "const_candidate_tokens",
    "const_resolutions",
    "declarations",
    "declaration_index",
    "enums_by_file_name",
    "file_paths",
    "function_source_names",
    "functions",
    "functions_by_file_name",
    "generated_enums",
    "generated_structs",
    "interner",
    "methods",
    "module_registry",
    "named_callable_methods_by_symbol",
    "named_method_declarations",
    "reusable_ordinary_bodies",
    "reusable_specialized_bodies",
    "stable_definition_endpoints",
    "stable_definition_tokens",
    "stable_module_endpoints",
    "stable_module_tokens",
    "structs_by_file_name",
    "symbol_paths",
    "trusted_standard_library_files",
    "well_known_option_by_payload",
)

# Every field on Sema and its dereferenced DeclarationNamespace is classified.
# This explicit complement is primarily body-local state, outputs, scalar
# configuration, or the query-owned RIR input. A new field fails the inventory
# until it is placed here or in EPOCH_FIELDS.
NON_EPOCH_SEMA_FIELDS = (
    "active_anonymous_producer",
    "analyzed_body_owners",
    "anon_struct_captured_values",
    "anon_struct_method_sigs",
    "anon_struct_type_subst",
    "anonymous_digest_owners",
    "anonymous_enum_ids",
    "anonymous_struct_ids",
    "body_analysis_work",
    "body_callable_dependencies",
    "body_dependency_observer",
    "body_named_dependencies",
    "body_specialization_dependencies",
    "builtin_arch_id",
    "builtin_data_model_id",
    "builtin_os_id",
    # The plan's read inventory classifies canonical_anonymous_types as a
    # body-local canonicalization accumulator. Its one_body.rs reads describe
    # anonymous identities produced/materialized by this attempt; they do not
    # consult the declaration epoch or whole-program namespace.
    "canonical_anonymous_types",
    "comptime_type_call_depth",
    "const_resolution_in_progress",
    "ctor_type_displays",
    "declaration_binding_active",
    "declaration_builtin_type_call_head_dependencies",
    "declaration_type_call_head_dependencies",
    "declaration_type_dependencies",
    "declaration_type_observer",
    "deferred_ownership_gates",
    "destructor_spans",
    "fn_signatures_in_flight",
    "forced_anonymous_digests",
    "infectious_linear",
    "known",
    "named_const_dependencies",
    "named_const_dependency_source",
    "named_destructor_dependencies",
    "named_method_dependencies",
    "non_generic_named_method_dependencies_complete",
    "one_body_error_recovery",
    "one_body_inference_failure_incomplete",
    "one_body_initial_anonymous_identities",
    "one_body_recovered_errors",
    "one_body_requested_producer",
    "ordinary_body_exports",
    "ordinary_free_function_dependencies",
    "param_arena",
    "preview_features",
    "rir",
    "root_file_id",
    "specialized_body_exports",
    "specialized_free_function_dependencies",
    "specialized_free_function_origins",
    "synthetic_declaration_discovery",
    "target",
    "type_pool",
    "well_known_option_identities",
)
EPOCH_ACCESS = re.compile(
    r"\b(?P<receiver>self(?:\s*\.\s*sema)?|sema)\s*\.\s*(?P<field>"
    + "|".join(sorted(EPOCH_FIELDS, key=len, reverse=True))
    + r")\b"
)

# Exact-body resolution is rooted in one_body.rs and its epoch-fact adapters.
# analysis.rs contributes only the explicitly carried outer-driver scans below;
# its other entry points compose already-produced bodies or run the legacy
# whole-program test driver, not an exact-one-body execution.
WHOLE_FILE_INVENTORY = {
    ("rue-air", Path("sema/aggregate_resolution.rs")),
    ("rue-air", Path("sema/body_endpoint.rs")),
    ("rue-air", Path("sema/call_resolution.rs")),
    ("rue-air", Path("sema/one_body.rs")),
    ("rue-air", Path("sema/typeck.rs")),
    ("rue-compiler", Path("canonical_semantic.rs")),
}
OUTER_DRIVER_FILE = ("rue-air", Path("sema/analysis.rs"))

# The Sema type-syntax adapters are the pre-r2 names of the type-syntax
# EpochFacts implementations described by the plan. Their impls have the same
# capability ownership: epoch reads stay inside the adapter and no borrowed
# table escapes.
EPOCH_FACT_IMPL_TYPES = (
    "DeferredSemaTypeSyntaxProvider",
    "EpochFacts",
    "SemaTypeSyntaxProvider",
)


@dataclass(frozen=True)
class WhitelistEntry:
    name: str
    path: Path
    function: str | None
    field: str | None
    justification: str


WHITELIST = (
    WhitelistEntry(
        "outer_named_callable_symbols",
        Path("sema/analysis.rs"),
        "intern_named_callable_symbols",
        None,
        "plan Review carry-forwards #2; r5: deterministic callable-universe enumeration",
    ),
    WhitelistEntry(
        "outer_unused_function_warnings",
        Path("sema/analysis.rs"),
        "add_unused_function_warnings",
        None,
        "plan Review carry-forwards #2; r5: outer-driver warning-universe scan",
    ),
    WhitelistEntry(
        "outer_anonymous_destructor_enqueue",
        Path("sema/analysis.rs"),
        "enqueue_anonymous_destructors",
        None,
        "plan Review carry-forwards #2; r5: implicit-root universe enumeration",
    ),
    WhitelistEntry(
        "universe_cardinality_counters",
        Path("sema/analysis.rs"),
        "analyze_all_function_bodies_mut_for_test",
        "functions_by_file_name",
        "plan Review carry-forwards #2; flip: both test-only cardinality reads",
    ),
    WhitelistEntry(
        "one_body_interner_reads",
        Path("sema/one_body.rs"),
        None,
        "interner",
        "plan Review carry-forwards #1; decision pending for name/symbol translation",
    ),
)


@dataclass(frozen=True)
class Hit:
    category: str
    crate: str
    path: Path
    line: int
    field: str
    attribution: str

    def display(self) -> str:
        return (
            f"{self.category}: crates/{self.crate}/src/{self.path}:{self.line}: "
            f"{self.field} ({self.attribution})"
        )


def mask_rust_non_code(source: str) -> str:
    """Mask comments and literals while preserving offsets and newlines."""
    masked = list(source)

    def hide(start: int, end: int) -> None:
        for index in range(start, end):
            if masked[index] != "\n":
                masked[index] = " "

    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end < 0 else end
            hide(index, end)
            index = end
            continue
        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            hide(index, end)
            index = end
            continue
        raw = re.match(r'(?:br|r)(?P<hashes>#{0,255})"', source[index:])
        if raw:
            terminator = '"' + raw.group("hashes")
            end = source.find(terminator, index + raw.end())
            end = len(source) if end < 0 else end + len(terminator)
            hide(index, end)
            index = end
            continue
        quote = index + 1 if source.startswith('b"', index) else index
        if quote < len(source) and source[quote] == '"':
            end = quote + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            hide(index, min(end, len(source)))
            index = end
            continue
        if source[index] == "'":
            end = index + 2
            if end < len(source) and source[index + 1] == "\\":
                end += 1
            if end < len(source) and source[end] == "'":
                hide(index, end + 1)
                index = end + 1
                continue
        index += 1
    return "".join(masked)


def braced_span(masked: str, opening: int) -> tuple[int, int]:
    depth = 1
    index = opening + 1
    while index < len(masked) and depth:
        if masked[index] == "{":
            depth += 1
        elif masked[index] == "}":
            depth -= 1
        index += 1
    return opening, index


def named_function_spans(masked: str) -> dict[str, tuple[int, int]]:
    spans: dict[str, tuple[int, int]] = {}
    for match in re.finditer(
        r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>{}]*>)?\s*\(", masked
    ):
        opening = masked.find("{", match.end())
        semicolon = masked.find(";", match.end(), opening if opening >= 0 else None)
        if opening < 0 or semicolon >= 0:
            continue
        spans[match.group(1)] = braced_span(masked, opening)
    return spans


def epoch_impl_spans(masked: str) -> list[tuple[int, int]]:
    spans = []
    for match in re.finditer(r"\bimpl\b", masked):
        opening = masked.find("{", match.end())
        if opening < 0:
            continue
        header = " ".join(masked[match.start():opening].split())
        if any(re.search(rf"\b{name}\b", header) for name in EPOCH_FACT_IMPL_TYPES):
            spans.append((match.start(), braced_span(masked, opening)[1]))
    return spans


def sema_impl_spans(masked: str) -> list[tuple[int, int]]:
    """Find legacy inherent Sema impls whose epoch reads strict mode removes."""
    spans = []
    for match in re.finditer(r"\bimpl\b", masked):
        opening = masked.find("{", match.end())
        if opening < 0:
            continue
        header = " ".join(masked[match.start():opening].split())
        if re.search(r"\bSema\s*<", header):
            spans.append((match.start(), braced_span(masked, opening)[1]))
    return spans


def declared_sema_fields(source: str) -> set[str]:
    """Read the capability universe from Sema and DeclarationNamespace."""
    fields: set[str] = set()
    sema_start = source.find("pub struct Sema<")
    if sema_start >= 0:
        opening = source.find("{", sema_start)
        _, end = braced_span(mask_rust_non_code(source), opening)
        body = source[opening:end]
        fields.update(
            re.findall(
                r"^\s*(?:pub\(crate\)\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:(?!:)",
                body,
                re.MULTILINE,
            )
        )
    namespace_start = source.find("pub struct DeclarationNamespace")
    if namespace_start >= 0:
        opening = source.find("{", namespace_start)
        _, end = braced_span(mask_rust_non_code(source), opening)
        body = source[opening:end]
        fields.update(
            re.findall(
                r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:",
                body,
                re.MULTILINE,
            )
        )
    return fields


def unclassified_sema_fields(source: str) -> set[str]:
    classified = set(EPOCH_FIELDS) | set(NON_EPOCH_SEMA_FIELDS)
    return declared_sema_fields(source) - classified


def contains(offset: int, span: tuple[int, int]) -> bool:
    return span[0] <= offset < span[1]


def whitelist_for(
    path: Path,
    field: str,
    offset: int,
    functions: dict[str, tuple[int, int]],
) -> WhitelistEntry | None:
    for entry in WHITELIST:
        if entry.path != path or entry.field not in (None, field):
            continue
        if entry.function is None or (
            entry.function in functions and contains(offset, functions[entry.function])
        ):
            return entry
    return None


def normalize_relative(path: Path, source_root: Path) -> Path:
    relative = path.relative_to(source_root)
    if relative.parts and relative.parts[0] == "src":
        return Path(*relative.parts[1:])
    return relative


def classify(sources: list[tuple[str, Path]]) -> list[Hit]:
    hits: list[Hit] = []
    for crate, source_root in sources:
        if not source_root.exists():
            hits.append(
                Hit(
                    "unaccounted",
                    crate,
                    Path("."),
                    0,
                    "<missing>",
                    "missing source root",
                )
            )
            continue
        if crate == "rue-air":
            sema_definition = source_root / "sema" / "mod.rs"
            if sema_definition.exists():
                for field in sorted(unclassified_sema_fields(sema_definition.read_text())):
                    hits.append(
                        Hit(
                            "unaccounted",
                            crate,
                            Path("sema/mod.rs"),
                            0,
                            field,
                            "Sema field has no epoch/body-local capability classification",
                        )
                    )
        for path in sorted(source_root.rglob("*.rs")):
            relative = normalize_relative(path, source_root)
            key = (crate, relative)
            if key not in WHOLE_FILE_INVENTORY and key != OUTER_DRIVER_FILE:
                continue
            source = path.read_text()
            masked = mask_rust_non_code(source)
            functions = named_function_spans(masked)
            impls = epoch_impl_spans(masked)
            legacy_sema_impls = sema_impl_spans(masked)
            for match in EPOCH_ACCESS.finditer(masked):
                field = match.group("field")
                offset = match.start()
                receiver = re.sub(r"\s+", "", match.group("receiver"))
                in_epoch_impl = any(contains(offset, span) for span in impls)
                in_legacy_sema_impl = any(
                    contains(offset, span) for span in legacy_sema_impls
                )
                if receiver == "self" and not (in_epoch_impl or in_legacy_sema_impl):
                    continue
                entry = whitelist_for(relative, field, offset, functions)
                if key == OUTER_DRIVER_FILE and entry is None:
                    continue
                if entry is not None:
                    category = "whitelist"
                    attribution = f"{entry.name}: {entry.justification}"
                elif in_epoch_impl:
                    category = "allowed"
                    attribution = "epoch-backed facts impl"
                elif receiver == "self" and in_legacy_sema_impl:
                    category = "allowed"
                    attribution = (
                        "legacy Sema epoch authority; provider-only mode rejects this span"
                    )
                else:
                    category = "unaccounted"
                    attribution = (
                        "outside an epoch-backed facts impl and the recorded whitelist"
                    )
                hits.append(
                    Hit(
                        category,
                        crate,
                        relative,
                        source.count("\n", 0, offset) + 1,
                        field,
                        attribution,
                    )
                )
    return hits


def errors_for(
    hits: list[Hit], provider_only: bool = PROVIDER_ONLY_MODE
) -> list[str]:
    errors = [hit.display() for hit in hits if hit.category == "unaccounted"]
    if provider_only:
        errors.extend(
            f"provider-only violation: {hit.display()}"
            for hit in hits
            if hit.category == "allowed"
        )
    return errors


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
    for hit in hits:
        print(hit.display())
    errors = errors_for(hits)
    if errors:
        print("body-analysis capability inventory failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    counts = {
        category: sum(hit.category == category for hit in hits)
        for category in ("allowed", "whitelist", "unaccounted")
    }
    print(
        "body-analysis capability inventory valid: "
        f"{counts['allowed']} epoch-authority, {counts['whitelist']} whitelisted, "
        f"{counts['unaccounted']} unaccounted"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
