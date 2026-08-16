#!/usr/bin/env python3
"""Guard Rue's canonical live-type representation across every consumer source."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gatelib import parse_crate_sources


ROOT = Path(__file__).resolve().parent.parent
CONSUMER_CRATES = (
    "rue-air",
    "rue-cfg",
    "rue-codegen",
    "rue-compiler",
    "rue-oracle",
)
OPAQUE_IDS = (
    "StructId",
    "EnumId",
    "ArrayTypeId",
    "PtrConstTypeId",
    "PtrMutTypeId",
)
COMPOSITE_TAGS = {
    "Struct": 100,
    "Enum": 101,
    "Array": 102,
    "Module": 103,
    "PtrConst": 104,
    "PtrMut": 105,
}
TAG_ASSIGNMENT = re.compile(
    r"\b(?P<category>" + "|".join(COMPOSITE_TAGS) + r")\s*=\s*"
    r"(?P<value>0x[0-9a-fA-F_]+|[0-9][0-9_]*)"
    r"(?:u(?:8|16|32|64|128|size)|i(?:8|16|32|64|128|size))?\b"
)
PUBLIC_RAW_API = re.compile(
    r"^\s*pub\s+(?:const\s+)?fn\s+"
    r"(?:from_pool_index|pool_index|raw_encoding)\s*\(",
    re.MULTILINE,
)
PUBLIC_UNCHECKED_ENCODING_API = re.compile(
    r"^\s*pub\s+(?:const\s+)?fn\s+(?:from_u32|as_u32)\s*\(",
    re.MULTILINE,
)
OPAQUE_ID_DERIVE = re.compile(
    r"#\[derive\((?P<traits>[^)]*)\)\]\s*"
    r"pub struct (?P<id>" + "|".join(OPAQUE_IDS) + r")\(",
    re.MULTILINE,
)
PUBLIC_TYPE_DECLARATION = re.compile(
    r"^pub\s+(?:struct|enum)\s+(?P<name>[A-Za-z0-9_]*Type[A-Za-z0-9_]*)\b",
    re.MULTILINE,
)
COMPACT_U32_NEWTYPE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+(?P<name>[A-Za-z0-9_]+)\s*"
    r"\(\s*(?:pub(?:\([^)]*\))?\s+)?u32\s*\)",
    re.MULTILINE,
)

# Every public type-shaped declaration in the scanned production graph must be
# classified here. A new module is discovered by the Buck glob and a new
# declaration fails closed until its ownership boundary is explicit.
PUBLIC_TYPE_DECLARATION_ALLOWLIST = {
    ("rue-air", "types.rs", "Type"): "canonical live compiler type handle",
    ("rue-air", "types.rs", "TypeKind"): "checked decoded view of Type",
    ("rue-air", "types.rs", "StructId"): "opaque typed nominal pool identity",
    ("rue-air", "types.rs", "EnumId"): "opaque typed nominal pool identity",
    ("rue-air", "types.rs", "ArrayTypeId"): "opaque typed structural pool identity",
    ("rue-air", "types.rs", "PtrConstTypeId"): "opaque typed structural pool identity",
    ("rue-air", "types.rs", "PtrMutTypeId"): "opaque typed structural pool identity",
    ("rue-air", "types.rs", "ModuleId"): "module identity carried by canonical Type",
    ("rue-air", "intern_pool.rs", "TypeData"): "canonical pool storage schema",
    ("rue-air", "intern_pool.rs", "TypeValidationError"): "checked pool boundary error",
    ("rue-air", "intern_pool.rs", "TypeInternPool"): "mutable semantic type owner",
    ("rue-air", "intern_pool.rs", "FrozenTypeInternPool"): "read-only downstream type owner",
    ("rue-air", "intern_pool.rs", "TypeInternPoolStats"): "pool metrics, not a type identity",
    ("rue-air", "inference/types.rs", "TypeVarId"): "inference-local variable identity",
    ("rue-air", "inference/types.rs", "InferType"): "inference-only term representation",
    ("rue-air", "inference/types.rs", "TypeVarAllocator"): "inference-local allocator",
    ("rue-air", "call_abi.rs", "NativeAbiTypeFacts"): "per-type call-ABI kernel facts, not a type identity",
    ("rue-air", "runtime_call.rs", "RuntimeAirType"): "typed runtime ABI shape classification",
    ("rue-air", "semantic_import.rs", "SemanticImportType"): "stable semantic import schema",
    ("rue-air", "semantic_import.rs", "SemanticImportTypeFold"): "exhaustive stable schema fold view",
    ("rue-air", "semantic_import.rs", "SemanticImportedType"): "validated imported-type result",
    ("rue-air", "semantic_type_resolution.rs", "SemanticTypeFact"): "value-only exact semantic lookup fact",
    ("rue-air", "semantic_type_resolution.rs", "SemanticTypeFactKind"): "semantic lookup fact classification",
    ("rue-air", "semantic_type_resolution.rs", "SemanticTypeConstructorParameter"): "value-only constructor signature metadata",
    ("rue-air", "semantic_type_resolution.rs", "SemanticTypeConstructorHead"): "value-only constructor selection metadata",
    ("rue-air", "semantic_type_resolution.rs", "SemanticTypeSyntaxFailure"): "semantic resolution policy failure schema",
    ("rue-air", "semantic_identity.rs", "TypeInstanceKey"): "canonical cross-boundary type identity",
    ("rue-air", "sema/aggregate_resolution.rs", "ProviderQualifiedType"): "provider-side qualified-type selection result",
    ("rue-air", "sema/binding_manifest.rs", "SemanticExportType"): "stable semantic export schema",
    ("rue-air", "sema/binding_manifest.rs", "SemanticAnonymousMethodType"): "source semantic export method-type schema",
    ("rue-air", "sema/body_identity.rs", "DurableAnonymousMethodType"): "provider-stable anonymous-method type schema",
    ("rue-air", "sema/body_identity.rs", "DurableCallableTypeSyntax"): "provider-stable callable type-syntax fragments",
    ("rue-air", "sema/provider_body_host.rs", "SemanticProducedAnonymousMethodType"): "body-produced semantic method-type schema",
    ("rue-air", "sema/info.rs", "AnonMethodType"): "anonymous method signature metadata",
    ("rue-air", "sema/output.rs", "DeclarationTypeDependencySourceKind"): "dependency metadata",
    ("rue-air", "sema/output.rs", "DeclarationTypeDependencyTargetKind"): "dependency metadata",
    ("rue-air", "sema/output.rs", "DeclarationTypeDependencyKind"): "dependency metadata",
    ("rue-air", "sema/output.rs", "DeclarationTypeDependencyEvent"): "dependency metadata",
    ("rue-air", "sema/output.rs", "DeclarationTypeCallHeadDependencyEvent"): "dependency metadata",
    ("rue-air", "sema/output.rs", "BuiltinTypeCallHead"): "dependency metadata",
    ("rue-air", "sema/output.rs", "DeclarationBuiltinTypeCallHeadDependencyEvent"): "dependency metadata",
    ("rue-compiler", "bound_definitions.rs", "StableNamedTypeKey"): "stable definition lookup key",
    ("rue-compiler", "durable_semantics.rs", "DurableType"): "cross-epoch durable type schema",
    ("rue-compiler", "durable_semantics.rs", "DurableAnonymousMethodType"): "cross-epoch durable anonymous-method type schema",
    ("rue-compiler", "source_identity.rs", "ModuleId"): "stable logical source-module identity",
    ("rue-compiler", "session.rs", "StableDeclarationTypeDependency"): "stable dependency metadata",
    ("rue-compiler", "session.rs", "StableDeclarationTypeCallHeadDependency"): "stable dependency metadata",
    ("rue-compiler", "session.rs", "StableBuiltinTypeCallHeadInput"): "stable dependency metadata",
}
COMPACT_U32_NEWTYPE_ALLOWLIST = {
    ("rue-air", "types.rs", "Type"): "canonical live type handle",
    ("rue-air", "types.rs", "StructId"): "opaque struct pool identity",
    ("rue-air", "types.rs", "EnumId"): "opaque enum pool identity",
    ("rue-air", "types.rs", "ArrayTypeId"): "opaque array pool identity",
    ("rue-air", "types.rs", "PtrConstTypeId"): "opaque const-pointer pool identity",
    ("rue-air", "types.rs", "PtrMutTypeId"): "opaque mut-pointer pool identity",
    ("rue-air", "types.rs", "ModuleId"): "request-local module identity",
    ("rue-air", "inference/types.rs", "TypeVarId"): "inference-local variable identity",
    ("rue-air", "inst.rs", "AirPlaceRef"): "typed AIR place index",
    ("rue-air", "inst.rs", "AirRef"): "typed AIR instruction index",
    ("rue-cfg", "inst.rs", "BlockId"): "typed CFG block index",
    ("rue-cfg", "inst.rs", "CfgValue"): "typed CFG value index",
    ("rue-codegen", "vreg.rs", "VReg"): "typed virtual register index",
    ("rue-codegen", "vreg.rs", "LabelId"): "typed code label index",
    ("rue-codegen", "index_map.rs", "TestHandle"): "test-only index-map fixture",
    ("rue-codegen", "regalloc.rs", "TestReg"): "test-only register fixture",
    ("rue-compiler", "parsed_modules.rs", "ParsedDefinitionOccurrence"): "typed parsed-definition occurrence index",
}


def source_files(root: Path, sources: list[tuple[str, Path]] | None) -> list[tuple[str, Path, Path]]:
    crate_sources = sources or [
        (crate, root / "crates" / crate / "src") for crate in CONSUMER_CRATES
    ]
    files = []
    for crate, source_root in crate_sources:
        for path in sorted(source_root.rglob("*.rs")):
            files.append((crate, source_root, path))
    return files


def validate(root: Path, sources: list[tuple[str, Path]] | None = None) -> list[str]:
    errors: list[str] = []
    crate_sources = sources or [
        (crate, root / "crates" / crate / "src") for crate in CONSUMER_CRATES
    ]
    for crate, source_root in crate_sources:
        if not source_root.exists():
            errors.append(f"missing live-type consumer source directory: {source_root}")

    peer_tokens = (
        "Interned" + "Type",
        "type_to_" + "interned",
        "interned_to_" + "type",
        "mod " + "compatibility",
    )
    retired_updaters = ("update_" + "struct_def", "update_" + "enum_def")
    observed_compact_newtypes: set[tuple[str, str, str]] = set()

    for crate, source_root, path in source_files(root, crate_sources):
        source = path.read_text()
        relative = path.relative_to(source_root)
        if relative.parts and relative.parts[0] == "src":
            relative = Path(*relative.parts[1:])
        display = Path("crates") / crate / "src" / relative

        for token in peer_tokens:
            if token in source:
                errors.append(f"{display}: peer live-type representation token {token!r}")

        if not (
            crate == "rue-air"
            and relative in {Path("type_encoding.rs"), Path("api_inventory.rs")}
        ):
            for match in TAG_ASSIGNMENT.finditer(source):
                value = int(match.group("value").replace("_", ""), 0)
                category = match.group("category")
                if value == COMPOSITE_TAGS[category]:
                    line = source.count("\n", 0, match.start()) + 1
                    errors.append(
                        f"{display}:{line}: duplicate compact type tag assignment "
                        f"{category}={value}"
                    )

        match = PUBLIC_RAW_API.search(source)
        if match:
            line = source.count("\n", 0, match.start()) + 1
            errors.append(f"{display}:{line}: public raw pool/type encoding API")

        if crate == "rue-air" and relative in {
            Path("types.rs"),
            Path("intern_pool.rs"),
            Path("type_encoding.rs"),
            Path("lib.rs"),
        }:
            match = PUBLIC_UNCHECKED_ENCODING_API.search(source)
            if match:
                line = source.count("\n", 0, match.start()) + 1
                errors.append(f"{display}:{line}: public unchecked type encoding API")

        for updater in retired_updaters:
            if updater in source:
                errors.append(f"{display}: whole-definition overwrite API {updater!r}")

        for opaque_id in OPAQUE_IDS:
            if re.search(rf"pub struct {opaque_id}\(pub\s", source):
                errors.append(f"{display}: {opaque_id} exposes its raw storage field")
            if re.search(rf"Display for {opaque_id}\b", source):
                errors.append(f"{display}: {opaque_id} exposes raw numeric Display")

        for match in OPAQUE_ID_DERIVE.finditer(source):
            traits = {trait.strip() for trait in match.group("traits").split(",")}
            if "Ord" in traits or "PartialOrd" in traits:
                errors.append(
                    f"{display}: {match.group('id')} exposes raw storage ordering"
                )

        declarations = [
            match.group("name") for match in PUBLIC_TYPE_DECLARATION.finditer(source)
        ]
        declarations.extend(
            opaque_id
            for opaque_id in ("StructId", "EnumId", "ModuleId")
            if re.search(rf"^pub\s+(?:struct|enum)\s+{opaque_id}\b", source, re.MULTILINE)
        )
        for declaration in declarations:
            key = (crate, relative.as_posix(), declaration)
            if key not in PUBLIC_TYPE_DECLARATION_ALLOWLIST:
                errors.append(
                    f"{display}: unclassified public type representation {declaration!r}"
                )

        for match in COMPACT_U32_NEWTYPE.finditer(source):
            key = (crate, relative.as_posix(), match.group("name"))
            observed_compact_newtypes.add(key)
            if key not in COMPACT_U32_NEWTYPE_ALLOWLIST:
                errors.append(
                    f"{display}: unclassified compact u32 newtype {match.group('name')!r}"
                )

    if {crate for crate, _ in crate_sources} == set(CONSUMER_CRATES):
        for key in sorted(set(COMPACT_U32_NEWTYPE_ALLOWLIST) - observed_compact_newtypes):
            errors.append(
                "stale compact-u32 classification was not observed: "
                f"{key[0]}/src/{key[1]}::{key[2]}"
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
        help="scan one Buck-materialized live-type consumer source directory",
    )
    args = parser.parse_args()

    root = args.root.resolve()
    try:
        parsed = parse_crate_sources(args.source, CONSUMER_CRATES)
    except ValueError as error:
        parser.error(str(error))
    sources = [(crate, path.resolve()) for crate, path in parsed]

    errors = validate(root, sources or None)
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print("type architecture inventory valid: one checked live handle across all consumers")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
