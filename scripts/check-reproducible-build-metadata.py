#!/usr/bin/env python3
"""Compare graph-reachable Rust build outputs from two relocated clean trees.

This is an opt-in diagnostic, not the required final-compiler reproducibility
gate.  It intentionally inventories only output contracts in the configured
dependency closure of //crates/rue:rue; it never walks buck-out globally.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


TARGET = "//crates/rue:rue"
GRAPH_ATTRIBUTES = "^(buck.type|name|proc_macro|crate)$"
BUILD_SCRIPT_RULE = "_cargo_buildscript_rule"
RUST_OUTPUT_SUFFIXES = (".rlib", ".rmeta")
PROC_MACRO_SUFFIXES = (".so", ".dylib", ".dll")
CREDENTIAL_ENV = ("BUILDBUDDY_API_KEY",)
SOURCE_DATE_EPOCH = "1900000000"


class DiagnosticError(RuntimeError):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def normalize_bytes(data: bytes, replacements: Sequence[Tuple[bytes, bytes]]) -> bytes:
    for old, new in sorted(replacements, key=lambda pair: len(pair[0]), reverse=True):
        data = data.replace(old, new)
    return data


def embedded_paths(data: bytes, replacements: Sequence[Tuple[bytes, bytes]]) -> List[str]:
    """Classify the most specific embedded path instead of double counting it."""
    remaining = data
    found = []
    for old, token in sorted(replacements, key=lambda pair: len(pair[0]), reverse=True):
        if old in remaining:
            found.append(token.decode("ascii"))
            remaining = remaining.replace(old, b"")
    return found


def path_replacements(root: Path, isolation: str) -> Tuple[Tuple[bytes, bytes], ...]:
    """Cover lexical and canonical macOS spellings such as /var and /private/var."""
    lexical_source = Path(os.path.abspath(str(root)))
    canonical_source = lexical_source.resolve()
    pairs = []
    for source in (lexical_source, canonical_source):
        pairs.append((str(source / "buck-out" / isolation).encode(), b"<BUILD_ROOT>"))
        pairs.append((str(source).encode(), b"<SOURCE_ROOT>"))
    unique = {}
    for old, token in pairs:
        unique[old] = token
    return tuple(unique.items())


def parse_ar_details(data: bytes) -> Dict[str, Any]:
    """Parse SysV/BSD ar while retaining every byte outside member payloads."""
    if not data.startswith(b"!<arch>\n"):
        raise DiagnosticError("file with .rlib suffix is not an ar archive")
    offset = 8
    members: List[Dict[str, Any]] = []
    gnu_names = b""
    occurrences: Dict[str, int] = {}
    while offset < len(data):
        if offset + 60 > len(data):
            break
        header = data[offset : offset + 60]
        if header[58:60] != b"`\n":
            raise DiagnosticError("invalid ar member header")
        raw_name = header[0:16].decode("utf-8", "surrogateescape").rstrip()
        try:
            member_size = int(header[48:58].decode("ascii").strip() or "0")
        except ValueError as error:
            raise DiagnosticError("invalid ar member size") from error
        payload_start = offset + 60
        payload_end = payload_start + member_size
        if payload_end > len(data):
            raise DiagnosticError("truncated ar member payload")
        payload = data[payload_start:payload_end]
        name_encoding = header[0:16]
        name = raw_name.rstrip("/")
        name_format = "short"
        name_storage_size = 0
        if raw_name.startswith("#1/"):
            name_size = int(raw_name[3:])
            name_format = "bsd-long"
            name_storage_size = name_size
            name_encoding = payload[:name_size]
            name = name_encoding.decode("utf-8", "surrogateescape").rstrip("\0")
            payload = payload[name_size:]
        elif raw_name == "//":
            name_format = "gnu-string-table"
            name = "//"
            gnu_names = payload
        elif raw_name.startswith("/") and raw_name[1:].isdigit() and gnu_names:
            name_format = "gnu-long-reference"
            name_offset = int(raw_name[1:])
            end = gnu_names.find(b"/\n", name_offset)
            if end < 0:
                end = len(gnu_names)
            name = gnu_names[name_offset:end].decode("utf-8", "surrogateescape")
        occurrence = occurrences.get(name, 0)
        occurrences[name] = occurrence + 1
        members.append(
            {
                "name": name,
                "occurrence": occurrence,
                "name_format": name_format,
                "member_size": member_size,
                "name_storage_size": name_storage_size,
                "canonical_bsd_name_field": name_format == "bsd-long"
                and header[0:16]
                == "#1/{}".format(name_storage_size).encode("ascii").ljust(16),
                "canonical_size_field": header[48:58]
                == str(member_size).encode("ascii").ljust(10),
                "name_encoding": name_encoding,
                "name_field": header[0:16],
                "mtime": header[16:28].decode("ascii", "replace").strip(),
                "mtime_field": header[16:28],
                "uid": header[28:34].decode("ascii", "replace").strip(),
                "uid_field": header[28:34],
                "gid": header[34:40].decode("ascii", "replace").strip(),
                "gid_field": header[34:40],
                "mode": header[40:48].decode("ascii", "replace").strip(),
                "mode_field": header[40:48],
                "size_field": header[48:58],
                "trailer_field": header[58:60],
                "payload": payload,
                "padding": data[payload_end : payload_end + (member_size & 1)],
            }
        )
        offset = payload_end + (member_size & 1)
    return {"members": members, "trailing": data[offset:]}


def parse_ar(data: bytes) -> List[Dict[str, Any]]:
    return parse_ar_details(data)["members"]


def bytes_record(data: bytes, replacements: Sequence[Tuple[bytes, bytes]]) -> Dict[str, Any]:
    normalized = normalize_bytes(data, replacements)
    return {
        "size": len(data),
        "normalized_size": len(normalized),
        "sha256": sha256(data),
        "normalized_sha256": sha256(normalized),
        "embedded_paths": embedded_paths(data, replacements),
    }


def configured_label_parts(label: str) -> Optional[Tuple[str, str]]:
    match = re.match(r"^root//(?P<package>[^:]*)[:](?P<name>[^ ]+) ", label)
    if not match:
        return None
    return match.group("package"), match.group("name")


def graph_contracts(graph: Mapping[str, Mapping[str, Any]]) -> List[Dict[str, Any]]:
    """Return the exact output classes this diagnostic promises to inspect."""
    contracts: Dict[Tuple[str, str, str], Dict[str, Any]] = {}
    for configured_label, attributes in graph.items():
        rule_type = attributes.get("buck.type")
        category = ""
        if rule_type == "rust_library" and attributes.get("proc_macro") is True:
            category = "proc-macro"
        elif rule_type == "rust_library":
            category = "rust-library"
        elif rule_type == "rust_binary" and attributes.get("crate") == "build_script_build":
            category = "build-script-executable"
        elif rule_type == BUILD_SCRIPT_RULE:
            category = "build-script-output"
        elif rule_type == "genrule":
            category = "generated-output"
        if category:
            parts = configured_label_parts(configured_label)
            if parts is None:
                if configured_label.startswith("root//"):
                    raise DiagnosticError(
                        "cannot parse eligible root-cell configured label: {}".format(
                            configured_label
                        )
                    )
                continue
            package, name = parts
            hashes = re.findall(r"#([0-9a-f]+)\)", configured_label)
            if not hashes:
                raise DiagnosticError(
                    "eligible configured label has no configuration hash: {}".format(
                        configured_label
                    )
                )
            key = (package, name, category)
            if key not in contracts:
                contracts[key] = {
                    "label": "root//{}:{}".format(package, name),
                    "package": package,
                    "name": name,
                    "category": category,
                    "configured_labels": [],
                }
            contracts[key]["configured_labels"].append(
                {"label": configured_label, "hash": hashes[-1]}
            )
    for contract in contracts.values():
        contract["configured_labels"].sort(key=lambda item: item["label"])
    return [contracts[key] for key in sorted(contracts)]


def metadata_check_targets(contracts: Sequence[Mapping[str, Any]]) -> List[str]:
    """Return only graph-reachable Rust library metadata subtargets."""
    return sorted(
        {
            "{}[check]".format(contract["label"])
            for contract in contracts
            if contract["category"] == "rust-library"
        }
    )


def provider_artifacts(
    provider_text: str, contracts: Sequence[Mapping[str, Any]]
) -> Dict[str, List[str]]:
    """Extract artifacts Buck binds to each exact configured target variant."""
    result: Dict[str, List[str]] = {}
    for contract in contracts:
        for configured in contract["configured_labels"]:
            label = configured["label"]
            pattern = re.compile(
                r"<build artifact ([^>\r\n]+?) bound to {}>".format(
                    re.escape(label)
                )
            )
            paths = sorted(set(pattern.findall(provider_text)))
            for path in paths:
                candidate = Path(path)
                if candidate.is_absolute() or ".." in candidate.parts:
                    raise DiagnosticError(
                        "Buck provider emitted unsafe output path for {}".format(label)
                    )
            result[label] = paths
    return result


def files_below(path: Path) -> Iterable[Tuple[str, Path]]:
    """Walk a declared output path, following its top-level Buck symlink."""
    if path.is_file():
        yield path.name, path
        return
    if not path.is_dir():
        return
    for directory, dirnames, filenames in os.walk(str(path), followlinks=True):
        dirnames.sort()
        filenames.sort()
        directory_path = Path(directory)
        for filename in filenames:
            item = directory_path / filename
            yield item.relative_to(path).as_posix(), item


def eligible_provider_output(category: str, path: str) -> bool:
    parts = Path(path).parts
    if any("depslink" in part or "depsfull" in part for part in parts):
        # Rust providers bind their dependency staging trees to the configured
        # target too. They are command inputs, not products to inventory.
        return False
    if category == "rust-library":
        return path.endswith(RUST_OUTPUT_SUFFIXES)
    if category == "proc-macro":
        return path.endswith(PROC_MACRO_SUFFIXES)
    if category == "build-script-executable":
        return Path(path).name == "build_script_build"
    if category == "build-script-output":
        return "OUT_DIR" in parts or "rustc_flags" in parts
    return category == "generated-output"


def contract_outputs(
    root: Path,
    isolation: str,
    contract: Mapping[str, Any],
    expected_outputs: Mapping[str, Sequence[str]],
) -> List[Tuple[str, Path]]:
    """Materialize only artifacts Buck providers bind to this configured node."""
    found: Dict[str, Path] = {}
    prefix = Path("buck-out") / isolation
    for configured in contract["configured_labels"]:
        label = configured["label"]
        scope = "configured-{}".format(configured["hash"])
        materialized = 0
        declared = [
            path
            for path in expected_outputs.get(label, ())
            if eligible_provider_output(contract["category"], path)
        ]
        if not declared:
            raise DiagnosticError(
                "eligible configured variant has no provider-declared outputs: {}".format(
                    label
                )
            )
        for raw_path in declared:
            relative = Path(raw_path)
            if relative.parts[: len(prefix.parts)] == prefix.parts:
                output = root / relative
                logical_declared = relative.relative_to(prefix).as_posix()
            else:
                package = Path(contract["package"]) if contract["package"] else Path()
                target_dir = (
                    root
                    / prefix
                    / "art"
                    / "root"
                    / configured["hash"]
                    / package
                    / "__{}__".format(contract["name"])
                )
                output = target_dir / relative
                logical_declared = relative.as_posix()
            if not output.exists():
                # Providers expose optional subtarget and linkage artifacts as
                # well as products materialized by this build invocation.
                continue
            expanded = list(files_below(output))
            if not expanded and output.is_dir():
                found["{}/{}".format(scope, logical_declared)] = output
                materialized += 1
            for child, item in expanded:
                logical = logical_declared
                if output.is_dir():
                    logical = "{}/{}".format(logical, child)
                found["{}/{}".format(scope, logical)] = item
                materialized += 1
        if materialized == 0:
            raise DiagnosticError(
                "eligible configured variant has no materialized provider-declared outputs: {}".format(
                    label
                )
            )
    return sorted(found.items())


def file_record(
    logical_key: str,
    category: str,
    label: str,
    path: Path,
    replacements: Sequence[Tuple[bytes, bytes]],
) -> Dict[str, Any]:
    file_stat = path.stat()
    if path.is_dir():
        return {
            "key": logical_key,
            "category": category,
            "label": label,
            "kind": "directory",
            "mode": stat.S_IMODE(file_stat.st_mode),
            "observed_mtime_ns": file_stat.st_mtime_ns,
        }
    data = path.read_bytes()
    normalized = normalize_bytes(data, replacements)
    leaks = embedded_paths(data, replacements)
    record: Dict[str, Any] = {
        "key": logical_key,
        "category": category,
        "label": label,
        "kind": "archive" if path.suffix == ".rlib" else "file",
        "size": len(data),
        "normalized_size": len(normalized),
        "sha256": sha256(data),
        "normalized_sha256": sha256(normalized),
        "mode": stat.S_IMODE(file_stat.st_mode),
        "observed_mtime_ns": file_stat.st_mtime_ns,
        "embedded_paths": leaks,
    }
    if path.suffix == ".rlib":
        archive_members = []
        details = parse_ar_details(data)
        for member in details["members"]:
            payload = member.pop("payload")
            raw_name = member.pop("name").encode("utf-8", "surrogateescape")
            member["name"] = normalize_bytes(raw_name, replacements).decode(
                "utf-8", "surrogateescape"
            )
            member["name_digest"] = bytes_record(raw_name, replacements)
            if member["name_format"] == "gnu-string-table":
                member["name_table"] = bytes_record(payload, replacements)
                member["payload"] = bytes_record(b"", replacements)
            else:
                member["payload"] = bytes_record(payload, replacements)
            for field in (
                "name_encoding",
                "name_field",
                "mtime_field",
                "uid_field",
                "gid_field",
                "mode_field",
                "size_field",
                "trailer_field",
                "padding",
            ):
                member[field] = bytes_record(member[field], replacements)
            archive_members.append(member)
        record["members"] = archive_members
        record["trailing"] = bytes_record(details["trailing"], replacements)
    return record


def inventory(
    root: Path,
    isolation: str,
    graph: Mapping[str, Mapping[str, Any]],
    expected_outputs: Mapping[str, Sequence[str]],
    owner_lookup: Optional[Any] = None,
    partial_path: Optional[Path] = None,
) -> List[Dict[str, Any]]:
    replacements = path_replacements(root, isolation)
    records: List[Dict[str, Any]] = []
    contracts = graph_contracts(graph)
    if not contracts:
        if partial_path is not None:
            dump_json(partial_path, records)
        raise DiagnosticError("configured graph contains no eligible output contracts")
    for contract in contracts:
        outputs = contract_outputs(root, isolation, contract, expected_outputs)
        if not outputs:
            if partial_path is not None:
                dump_json(partial_path, records)
            raise DiagnosticError(
                "eligible graph contract has no owned outputs: {} ({})".format(
                    contract["label"], contract["category"]
                )
            )
        for logical, path in outputs:
            key = "{}/{}/{}".format(contract["category"], contract["label"], logical)
            record = file_record(
                key, contract["category"], contract["label"], path, replacements
            )
            if owner_lookup is not None:
                owner = owner_lookup(path)
                if owner != contract["label"]:
                    if partial_path is not None:
                        dump_json(partial_path, records + [record])
                    raise DiagnosticError(
                        "Buck reports {} owns {}, expected {}".format(
                            owner, path.relative_to(root), contract["label"]
                        )
                    )
                record["audited_owner"] = owner
            records.append(record)
            if partial_path is not None:
                dump_json(partial_path, records)
    records.sort(key=lambda record: record["key"])
    return records


def normalized_value(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: normalized_value(item)
            for key, item in value.items()
            if key
            not in (
                "observed_mtime_ns",
                "sha256",
                "size",
                "member_size",
                "name_storage_size",
            )
        }
    if isinstance(value, list):
        return [normalized_value(item) for item in value]
    return value


def normalized_manifest(records: Sequence[Mapping[str, Any]], revision: str) -> Dict[str, Any]:
    normalized_records = [normalized_value(record) for record in records]
    return {
        "schema": 1,
        "target": TARGET,
        "revision": revision,
        "execution": {
            "local_only": True,
            "remote_cache": False,
            "source_date_epoch": SOURCE_DATE_EPOCH,
        },
        "artifacts": normalized_records,
    }


def compare_records(first: Sequence[Mapping[str, Any]], second: Sequence[Mapping[str, Any]]) -> Dict[str, Any]:
    left = {record["key"]: record for record in first}
    right = {record["key"]: record for record in second}
    report: Dict[str, Any] = {
        "only_in_first": sorted(left.keys() - right.keys()),
        "only_in_second": sorted(right.keys() - left.keys()),
        "filesystem_mtime_differences": [],
        "filesystem_mode_differences": [],
        "embedded_path_leaks": [],
        "archive_metadata_differences": [],
        "archive_format_differences": [],
        "path_only_archive_name_differences": [],
        "path_only_payload_differences": [],
        "payload_differences": [],
    }
    for key in sorted(left.keys() & right.keys()):
        a, b = left[key], right[key]
        if a.get("observed_mtime_ns") != b.get("observed_mtime_ns"):
            report["filesystem_mtime_differences"].append(key)
        if a.get("mode") != b.get("mode"):
            report["filesystem_mode_differences"].append(
                {"key": key, "first": a.get("mode"), "second": b.get("mode")}
            )
        if a.get("embedded_paths") or b.get("embedded_paths"):
            report["embedded_path_leaks"].append(
                {"key": key, "first": a.get("embedded_paths", []), "second": b.get("embedded_paths", [])}
            )
        if a.get("kind") != b.get("kind"):
            report["payload_differences"].append({"key": key, "reason": "kind"})
            continue
        if a.get("kind") == "archive":
            classified_before = sum(
                len(report[name])
                for name in (
                    "archive_metadata_differences",
                    "archive_format_differences",
                    "path_only_archive_name_differences",
                    "path_only_payload_differences",
                    "payload_differences",
                )
            )
            a_ids = [(m["name"], m["occurrence"]) for m in a["members"]]
            b_ids = [(m["name"], m["occurrence"]) for m in b["members"]]
            if len(a_ids) != len(b_ids):
                report["archive_format_differences"].append(
                    {"key": key, "reason": "archive member count"}
                )
            for index, (ma, mb) in enumerate(zip(a["members"], b["members"])):
                member_key = {"index": index, "first": a_ids[index], "second": b_ids[index]}
                metadata_fields = ("mtime_field", "uid_field", "gid_field", "mode_field")
                changed_metadata = [
                    field.removesuffix("_field")
                    for field in metadata_fields
                    if ma[field]["sha256"] != mb[field]["sha256"]
                ]
                if changed_metadata:
                    report["archive_metadata_differences"].append(
                        {"key": key, "member": member_key, "fields": changed_metadata}
                    )
                member_paths = sorted(
                    set(
                        ma["payload"]["embedded_paths"]
                        + mb["payload"]["embedded_paths"]
                        + ma["name_digest"]["embedded_paths"]
                        + mb["name_digest"]["embedded_paths"]
                        + ma["name_encoding"]["embedded_paths"]
                        + mb["name_encoding"]["embedded_paths"]
                        + ma.get("name_table", {}).get("embedded_paths", [])
                        + mb.get("name_table", {}).get("embedded_paths", [])
                    )
                )
                if member_paths:
                    report["embedded_path_leaks"].append(
                        {"key": key, "member": member_key, "paths": member_paths}
                    )
                changed_name_regions = []
                path_only_name = False
                for field in ("name_digest", "name_encoding", "name_table"):
                    if field not in ma and field not in mb:
                        continue
                    if field not in ma or field not in mb:
                        changed_name_regions.append(field)
                        continue
                    if ma[field]["sha256"] != mb[field]["sha256"]:
                        changed_name_regions.append(field)
                        path_only_name = path_only_name or (
                            ma[field]["normalized_sha256"]
                            == mb[field]["normalized_sha256"]
                        )
                if changed_name_regions:
                    all_path_only = all(
                        field in ma
                        and field in mb
                        and ma[field]["normalized_sha256"]
                        == mb[field]["normalized_sha256"]
                        for field in changed_name_regions
                    )
                    path_only_name = all_path_only
                    destination = (
                        "path_only_archive_name_differences"
                        if path_only_name
                        else "archive_format_differences"
                    )
                    report[destination].append(
                        {"key": key, "member": member_key, "fields": changed_name_regions}
                    )
                bsd_length_only = (
                    path_only_name
                    and ma["name_format"] == "bsd-long"
                    and mb["name_format"] == "bsd-long"
                    and ma["member_size"]
                    == ma["name_storage_size"] + ma["payload"]["size"]
                    and mb["member_size"]
                    == mb["name_storage_size"] + mb["payload"]["size"]
                    and ma["payload"]["size"] == mb["payload"]["size"]
                    and ma["name_storage_size"] != mb["name_storage_size"]
                    and ma["canonical_bsd_name_field"]
                    and mb["canonical_bsd_name_field"]
                    and ma["canonical_size_field"]
                    and mb["canonical_size_field"]
                )
                format_fields = ("name_field", "size_field", "trailer_field", "padding")
                changed_format = []
                for field in format_fields:
                    if ma[field]["sha256"] == mb[field]["sha256"]:
                        continue
                    consequent = bsd_length_only and field in ("name_field", "size_field")
                    if field == "padding" and bsd_length_only:
                        # A parity change adds/removes the single ar alignment byte.
                        # If both archives have padding, its content is independent.
                        consequent = not (
                            ma[field]["size"] > 0 and mb[field]["size"] > 0
                        )
                    if not consequent:
                        changed_format.append(field)
                if changed_format:
                    report["archive_format_differences"].append(
                        {"key": key, "member": member_key, "fields": changed_format}
                    )
                if ma["payload"]["sha256"] != mb["payload"]["sha256"]:
                    destination = (
                        "path_only_payload_differences"
                        if ma["payload"]["normalized_sha256"]
                        == mb["payload"]["normalized_sha256"]
                        else "payload_differences"
                    )
                    report[destination].append({"key": key, "member": member_key})
            if a["trailing"]["sha256"] != b["trailing"]["sha256"]:
                report["archive_format_differences"].append(
                    {"key": key, "reason": "trailing bytes"}
                )
            classified_after = sum(
                len(report[name])
                for name in (
                    "archive_metadata_differences",
                    "archive_format_differences",
                    "path_only_archive_name_differences",
                    "path_only_payload_differences",
                    "payload_differences",
                )
            )
            if a["sha256"] != b["sha256"] and classified_after == classified_before:
                destination = (
                    "path_only_payload_differences"
                    if a["normalized_sha256"] == b["normalized_sha256"]
                    else "archive_format_differences"
                )
                report[destination].append(
                    {"key": key, "reason": "whole-archive fallback"}
                )
        elif a.get("sha256") != b.get("sha256"):
            destination = (
                "path_only_payload_differences"
                if a.get("normalized_sha256") == b.get("normalized_sha256")
                else "payload_differences"
            )
            report[destination].append({"key": key})
    report["reproducible"] = not any(
        report[name]
        for name in (
            "only_in_first",
            "only_in_second",
            "filesystem_mode_differences",
            "embedded_path_leaks",
            "archive_metadata_differences",
            "archive_format_differences",
            "path_only_archive_name_differences",
            "path_only_payload_differences",
            "payload_differences",
        )
    )
    return report


def run(command: Sequence[str], cwd: Path, **kwargs: Any) -> subprocess.CompletedProcess:
    print("+ {}".format(" ".join(command)), flush=True)
    kwargs.setdefault("check", True)
    return subprocess.run(command, cwd=str(cwd), **kwargs)


def sentinel_state(root: Path) -> Tuple[bool, int, str, int, int]:
    sentinel = root / ".buckconfig.local"
    if sentinel.is_symlink() or not sentinel.is_file():
        raise DiagnosticError("cache-off sentinel is not an ordinary file: {}".format(sentinel))
    sentinel_stat = sentinel.stat()
    return (
        sentinel.is_symlink(),
        stat.S_IMODE(sentinel_stat.st_mode),
        sha256(sentinel.read_bytes()),
        sentinel_stat.st_mtime_ns,
        sentinel_stat.st_ino,
    )


def hardened_buck_env(root: Path, timezone: str) -> Dict[str, str]:
    env = os.environ.copy()
    for name in CREDENTIAL_ENV:
        env.pop(name, None)
    absent_config = root / ".diagnostic-no-buildbuddy-config"
    if absent_config.exists() or absent_config.is_symlink():
        raise DiagnosticError(
            "nonexistent BuildBuddy config sentinel unexpectedly exists: {}".format(absent_config)
        )
    env.update(
        {
            "LC_ALL": "C",
            "RUE_BUILDBUDDY_CONFIG": str(absent_config),
            "SOURCE_DATE_EPOCH": SOURCE_DATE_EPOCH,
            "TMPDIR": str(root / ".diagnostic-tmp"),
            "TZ": timezone,
        }
    )
    return env


def run_buck(
    args: Sequence[str],
    root: Path,
    env: Mapping[str, str],
    expected_sentinel: Tuple[bool, int, str, int, int],
    **kwargs: Any,
) -> subprocess.CompletedProcess:
    if sentinel_state(root) != expected_sentinel:
        raise DiagnosticError("cache-off sentinel changed before Buck invocation")
    try:
        result = run(["./buck2"] + list(args), root, env=dict(env), **kwargs)
    finally:
        if sentinel_state(root) != expected_sentinel:
            raise DiagnosticError("cache-off sentinel changed during Buck invocation")
        absent_config = Path(env["RUE_BUILDBUDDY_CONFIG"])
        if absent_config.exists() or absent_config.is_symlink():
            raise DiagnosticError("Buck created the forbidden central cache config sentinel")
    return result


def archive_revision(repo_root: Path, destination: Path) -> None:
    archive = subprocess.Popen(
        ["git", "-C", str(repo_root), "archive", "--format=tar", "HEAD"],
        stdout=subprocess.PIPE,
    )
    assert archive.stdout is not None
    try:
        with tarfile.open(fileobj=archive.stdout, mode="r|") as source:
            source.extractall(destination)
    finally:
        archive.stdout.close()
    if archive.wait() != 0:
        raise DiagnosticError("git archive failed")


def build_one(
    root: Path,
    isolation: str,
    threads: int,
    timezone: str,
    graph_path: Path,
    partial_path: Path,
) -> Tuple[Mapping[str, Mapping[str, Any]], List[Dict[str, Any]]]:
    sentinel = root / ".buckconfig.local"
    sentinel.write_bytes(b"")
    sentinel.chmod(0o600)
    expected_sentinel = sentinel_state(root)
    env = hardened_buck_env(root, timezone)
    Path(env["TMPDIR"]).mkdir()
    run_buck(
        [
            "--isolation-dir",
            isolation,
            "build",
            TARGET,
            "--local-only",
            "--no-remote-cache",
            "--num-threads",
            str(threads),
        ],
        root,
        env=env,
        expected_sentinel=expected_sentinel,
    )
    query = run_buck(
        [
            "--isolation-dir",
            isolation,
            "cquery",
            "deps({})".format(TARGET),
            "--output-attribute={}".format(GRAPH_ATTRIBUTES),
            "--json",
        ],
        root,
        env=env,
        expected_sentinel=expected_sentinel,
        stdout=subprocess.PIPE,
        text=True,
    )
    graph = json.loads(query.stdout)
    dump_json(graph_path, graph)
    if any("root//platforms:remote_cache" in label for label in graph):
        raise DiagnosticError("configured graph unexpectedly uses the remote-cache platform")
    contracts = graph_contracts(graph)
    check_targets = metadata_check_targets(contracts)
    if not check_targets:
        raise DiagnosticError("configured graph contains no Rust metadata subtargets")
    run_buck(
        [
            "--isolation-dir",
            isolation,
            "build",
        ]
        + check_targets
        + [
            "--materializations",
            "all",
            "--local-only",
            "--no-remote-cache",
            "--num-threads",
            str(threads),
        ],
        root,
        env=env,
        expected_sentinel=expected_sentinel,
    )
    providers = run_buck(
        [
            "--isolation-dir",
            isolation,
            "cquery",
            "deps({})".format(TARGET),
            "--show-providers",
        ],
        root,
        env=env,
        expected_sentinel=expected_sentinel,
        stdout=subprocess.PIPE,
        text=True,
    )
    expected_outputs = provider_artifacts(providers.stdout, contracts)

    def owner_lookup(path: Path) -> str:
        relative = path.relative_to(root).as_posix()
        audited = run_buck(
            ["--isolation-dir", isolation, "audit", "output", relative],
            root,
            env,
            expected_sentinel,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        labels = re.findall(r"root//[^\s`]+:[^\s`(]+", audited.stdout + audited.stderr)
        if not labels:
            raise DiagnosticError("Buck did not report an owner for {}".format(relative))
        return labels[-1]

    records = inventory(
        root,
        isolation,
        graph,
        expected_outputs,
        owner_lookup=owner_lookup,
        partial_path=partial_path,
    )
    if not any(
        record["category"] == "rust-library" and record["key"].endswith(".rmeta")
        for record in records
    ):
        raise DiagnosticError(
            "explicit metadata materialization produced no inventoried .rmeta outputs"
        )
    return graph, records


def dump_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, help="directory for manifests and comparison report")
    parser.add_argument("--keep-build-roots", action="store_true", help="retain the large relocated build trees")
    args = parser.parse_args(argv)

    repo_root = Path(__file__).resolve().parent.parent
    revision_result = run(
        ["git", "rev-parse", "--verify", "HEAD"],
        repo_root,
        stdout=subprocess.PIPE,
        text=True,
    )
    revision = revision_result.stdout.strip()
    status = run(
        ["git", "status", "--porcelain", "--untracked-files=normal"],
        repo_root,
        stdout=subprocess.PIPE,
        text=True,
    )
    if status.stdout:
        raise DiagnosticError(
            "working tree must be clean: both builds archive recorded revision {}".format(revision)
        )

    temp_base = Path(
        os.path.abspath(os.environ.get("RUNNER_TEMP", os.environ.get("TMPDIR", "/tmp")))
    ).resolve()
    scratch = Path(
        tempfile.mkdtemp(prefix="rue-repro-metadata-builds.", dir=str(temp_base))
    ).resolve()
    roots: List[Path] = []
    isolation = "repro-metadata"
    try:
        output_dir = args.output_dir or Path(
            tempfile.mkdtemp(prefix="rue-repro-metadata-report.", dir=str(temp_base))
        )
        output_dir.mkdir(parents=True, exist_ok=True)
        first_root = (scratch / "a").resolve()
        second_root = (
            scratch / "second-deliberately-relocated-and-longer-source-root"
        ).resolve()
        roots.extend((first_root, second_root))
        first_root.mkdir()
        second_root.mkdir()
        archive_revision(repo_root, first_root)
        archive_revision(repo_root, second_root)
        # Both trees contain only immutable tracked bytes. Deliberately distinct
        # mtimes prove that the manifest treats them as observations, not payload.
        for index, root in enumerate((first_root, second_root)):
            stamp = 1_900_000_000 + index * 10_000
            for directory, _, filenames in os.walk(str(root)):
                for filename in filenames:
                    os.utime(str(Path(directory) / filename), (stamp, stamp))

        _, first = build_one(
            first_root,
            isolation,
            1,
            "UTC",
            output_dir / "first-graph.json",
            output_dir / "first-partial-observations.json",
        )
        dump_json(output_dir / "first-observations.json", first)
        dump_json(
            output_dir / "first-manifest.json", normalized_manifest(first, revision)
        )
        (output_dir / "first-partial-observations.json").unlink()
        _, second = build_one(
            second_root,
            isolation,
            4,
            "Pacific/Honolulu",
            output_dir / "second-graph.json",
            output_dir / "second-partial-observations.json",
        )
        dump_json(output_dir / "second-observations.json", second)
        dump_json(output_dir / "second-manifest.json", normalized_manifest(second, revision))
        (output_dir / "second-partial-observations.json").unlink()
        report = compare_records(first, second)
        dump_json(output_dir / "comparison.json", report)
        print("Compared {} and {} graph-scoped outputs".format(len(first), len(second)))
        print("Filesystem mtime differences (informational): {}".format(len(report["filesystem_mtime_differences"])))
        print("Report: {}".format(output_dir))
        if report["reproducible"]:
            print("Graph-reachable compiler build metadata and payloads are reproducible")
            return 0
        print("FAIL: graph-reachable compiler build outputs differ", file=sys.stderr)
        for name in (
            "only_in_first",
            "only_in_second",
            "filesystem_mode_differences",
            "embedded_path_leaks",
            "archive_metadata_differences",
            "archive_format_differences",
            "path_only_archive_name_differences",
            "path_only_payload_differences",
            "payload_differences",
        ):
            if report[name]:
                print("  {}: {}".format(name, len(report[name])), file=sys.stderr)
        return 1
    finally:
        for root in roots:
            if (root / "buck2").exists() and (root / ".buckconfig.local").is_file():
                env = hardened_buck_env(root, "UTC")
                run_buck(
                    ["--isolation-dir", isolation, "kill"],
                    root,
                    env,
                    sentinel_state(root),
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
        if args.keep_build_roots:
            print("Build roots: {}".format(scratch))
        else:
            shutil.rmtree(scratch)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (DiagnosticError, OSError, subprocess.CalledProcessError) as error:
        print("error: {}".format(error), file=sys.stderr)
        sys.exit(2)
