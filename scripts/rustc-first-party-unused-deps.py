#!/usr/bin/env python3
"""Run rustc and reject only unused direct first-party Rue crate dependencies.

Buck's Rust action requests rustc's `unused-externs-silent` notification, but
the upstream switch covers every `--extern`. This wrapper changes that lint to
force-warn so an unused third-party crate cannot fail compilation, then filters
the notification using the owner encoded in each hermetic Buck artifact path.
Only artifacts proved to be owned by `root//crates/...` are promoted to errors;
unknown workspace layouts fail closed for an audited production consumer.

The wrapper is the toolchain compiler, so it sees the exact configured crate
root, cfgs, generated/mapped sources, proc macros, aliases, and extern names.
It preserves rustc stdout and every unrelated stderr record verbatim.
Enforcement happens on each configured production compilation that actually
runs; this is not a proactive all-target or all-configuration audit. The
pre-existing first-party findings are an explicit target/concrete-dependency
baseline with per-edge reasons. It is reviewed debt, not self-pruning evidence.
"""

from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from enum import Enum, auto
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple


UNUSED_FLAGS = {
    "-Wunused-crate-dependencies",
    "-Wunused_crate_dependencies",
}
FORCE_WARN = "--force-warn=unused-crate-dependencies"
ARTIFACT_LAYOUT = re.compile(r"(?:^|/)art/(?P<cell>[^/]+)/(?P<rest>.+)")
FIRST_PARTY_LAYOUT = re.compile(
    r"^(?:[0-9a-f]{16,}/)?(?P<package>crates(?:/[^/]+)+)/__(?P<target>[^/]+)__/"
)
KNOWN_ROOT_NON_FIRST_PARTY = ("third-party/", "toolchains/", "prelude/")


class Ownership(Enum):
    FIRST_PARTY = auto()
    KNOWN_NON_FIRST_PARTY = auto()
    UNKNOWN = auto()


@dataclass(frozen=True)
class BaselineEntry:
    consumer: str
    dependency: str
    reason: str


# Exact target identities after configured alias resolution. Each entry is
# reviewed baseline debt applied in every configuration, not a self-pruning
# exception. Reasons record why the edge remains outside RUE-1853's two-edge
# deletion scope; they do not assert that the dependency is needed in production.
BASELINE_ENTRIES = (
    BaselineEntry(
        "root//crates/rue-codegen:rue-codegen",
        "root//crates/rue-span:rue-span",
        "used by codegen's in-source test modules; production/test dependency separation is follow-up scope",
    ),
    BaselineEntry(
        "root//crates/rue-fuzz:rue-fuzz",
        "root//crates/rue-air:rue-air",
        "the fuzz binary uses rue-air-fuzz-support instead of the base crate directly",
    ),
    BaselineEntry(
        "root//crates/rue-fuzz:rue-fuzz",
        "root//crates/rue-cfg:rue-cfg",
        "the fuzz binary uses rue-cfg-fuzz-support instead of the base crate directly",
    ),
    BaselineEntry(
        "root//crates/rue-fuzz:rue-fuzz",
        "root//crates/rue-rir:rue-rir",
        "the fuzz binary uses rue-rir-fuzz-support instead of the base crate directly",
    ),
    BaselineEntry(
        "root//crates/rue:rue-driver",
        "root//crates/rue-perf-schema:rue-perf-schema",
        "the shared driver dependency list also serves binaries whose timing module uses the schema",
    ),
    BaselineEntry(
        "root//crates/rue:rue-driver",
        "root//crates/rue-target:rue-target",
        "the shared driver dependency list also serves binaries whose output path uses target metadata",
    ),
    BaselineEntry(
        "root//crates/rue-air:rue-air-fuzz-support",
        "root//crates/rue-lexer:rue-lexer",
        "the manual fuzz-support target reuses all AIR sources, where lexer use is test-gated",
    ),
    BaselineEntry(
        "root//crates/rue-air:rue-air-fuzz-support",
        "root//crates/rue-parser:rue-parser",
        "the manual fuzz-support target reuses all AIR sources, where parser use is test-gated",
    ),
)
BASELINE = {(entry.consumer, entry.dependency): entry for entry in BASELINE_ENTRIES}
if len(BASELINE) != len(BASELINE_ENTRIES):
    raise RuntimeError("duplicate first-party unused-dependency baseline edge")


def configured_consumer(args: Sequence[str]) -> Optional[str]:
    values = [
        arg[len("-Cmetadata=") :].split("#", 1)[0]
        for arg in args
        if arg.startswith("-Cmetadata=root//")
    ]
    return values[0] if len(values) == 1 else None


def extern_artifacts(args: Sequence[str]) -> Dict[str, str]:
    result: Dict[str, str] = {}
    index = 0
    while index < len(args):
        arg = args[index]
        value: Optional[str] = None
        if arg.startswith("--extern="):
            value = arg[len("--extern=") :]
        elif arg == "--extern" and index + 1 < len(args):
            index += 1
            value = args[index]
        if value and "=" in value:
            name, artifact = value.split("=", 1)
            if name in result and result[name] != artifact:
                raise ValueError(f"ambiguous --extern artifact for {name!r}")
            result[name] = artifact
        index += 1
    return result


def inspection_args(args: Sequence[str], depth: int = 0) -> List[str]:
    """Expand rustc response files for policy inspection only."""
    if depth > 8:
        raise ValueError("rustc response-file nesting exceeds 8")
    result: List[str] = []
    for arg in args:
        if arg.startswith("@"):
            path = Path(arg[1:])
            if not path.is_file():
                raise ValueError(f"rustc response file does not exist: {path}")
            result.extend(inspection_args(path.read_text().splitlines(), depth + 1))
        else:
            result.append(arg)
    return result


def _rewrite_response(path: Path, temporary: List[Path], depth: int) -> Path:
    if depth > 8:
        raise ValueError("rustc response-file nesting exceeds 8")
    rewritten: List[str] = []
    for line in path.read_text().splitlines():
        if line in UNUSED_FLAGS:
            rewritten.append(FORCE_WARN)
        elif line.startswith("@"):
            nested = _rewrite_response(Path(line[1:]), temporary, depth + 1)
            rewritten.append("@" + str(nested))
        else:
            rewritten.append(line)
    handle = tempfile.NamedTemporaryFile(
        mode="w", prefix="rue-rustc-args-", suffix=".txt", delete=False
    )
    with handle:
        handle.write("\n".join(rewritten) + "\n")
    result = Path(handle.name)
    temporary.append(result)
    return result


def invocation_args(args: Sequence[str]) -> Tuple[List[str], List[Path]]:
    """Force-warn the lint without flattening Buck's response-file command."""
    result: List[str] = []
    temporary: List[Path] = []
    try:
        for arg in args:
            if arg in UNUSED_FLAGS:
                result.append(FORCE_WARN)
            elif arg.startswith("@"):
                result.append("@" + str(_rewrite_response(Path(arg[1:]), temporary, 0)))
            else:
                result.append(arg)
    except (OSError, UnicodeError, ValueError):
        for path in temporary:
            try:
                path.unlink()
            except OSError:
                pass
        raise
    return result, temporary


def classify_owner(artifact: str) -> Tuple[Ownership, Optional[str]]:
    layout = ARTIFACT_LAYOUT.search(artifact.replace("\\", "/"))
    if layout is None:
        return Ownership.UNKNOWN, None
    cell = layout.group("cell")
    rest = layout.group("rest")
    if cell != "root":
        return Ownership.KNOWN_NON_FIRST_PARTY, None
    match = FIRST_PARTY_LAYOUT.match(rest)
    if match is not None:
        owner = f"root//{match.group('package')}:{match.group('target')}"
        return Ownership.FIRST_PARTY, owner
    unhashed_rest = re.sub(r"^[0-9a-f]{16,}/", "", rest)
    if unhashed_rest.startswith(KNOWN_ROOT_NON_FIRST_PARTY):
        return Ownership.KNOWN_NON_FIRST_PARTY, None
    return Ownership.UNKNOWN, None


def compiler_output_paths(args: Sequence[str]) -> List[Path]:
    """Return the concrete files rustc was asked to create."""
    values: List[str] = []
    index = 0
    while index < len(args):
        arg = args[index]
        if arg.startswith("--emit="):
            values.extend(arg[len("--emit=") :].split(","))
        elif arg == "--emit" and index + 1 < len(args):
            index += 1
            values.extend(args[index].split(","))
        elif arg.startswith("-o="):
            values.append("output=" + arg[len("-o=") :])
        elif arg == "-o" and index + 1 < len(args):
            index += 1
            values.append("output=" + args[index])
        index += 1
    return [Path(value.split("=", 1)[1]) for value in values if "=" in value]


def remove_compiler_outputs(paths: Sequence[Path]) -> List[str]:
    """Remove every explicit output and return deterministic cleanup errors."""
    errors: List[str] = []
    for path in paths:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        except OSError as error:
            errors.append(f"{path}: {error}")
    for path in paths:
        if path.exists() or path.is_symlink():
            message = f"{path}: output still exists after cleanup"
            if not any(error.startswith(f"{path}:") for error in errors):
                errors.append(message)
    return errors


def filter_unused(
    diagnostic: Dict[str, object],
    consumer: Optional[str],
    externs: Dict[str, str],
    audit_consumer: bool = True,
) -> Tuple[Optional[Dict[str, object]], List[Tuple[str, str]]]:
    raw_names = diagnostic.get("unused_extern_names")
    if not isinstance(raw_names, list):
        return diagnostic, []
    names = sorted({name for name in raw_names if isinstance(name, str)})
    findings: List[Tuple[str, str]] = []
    kept: List[str] = []
    for name in names:
        artifact = externs.get(name)
        if artifact is None:
            raise ValueError(f"unused extern {name!r} has no unique --extern artifact")
        ownership, owner = classify_owner(artifact)
        if not audit_consumer:
            continue
        if ownership == Ownership.KNOWN_NON_FIRST_PARTY:
            continue
        if ownership == Ownership.UNKNOWN:
            raise ValueError(f"unused extern {name!r} has unknown Buck artifact ownership: {artifact!r}")
        assert owner is not None
        if consumer is None:
            raise ValueError("first-party unused extern has no unique configured consumer metadata")
        if (consumer, owner) in BASELINE:
            continue
        kept.append(name)
        findings.append((consumer, owner))
    if not kept:
        return None, []
    diagnostic["unused_extern_names"] = kept
    diagnostic["lint_level"] = "deny"
    return diagnostic, findings


def policy_diagnostic(consumer: str, dependency: str) -> Dict[str, object]:
    message = f"unused direct first-party Rust dependency: {consumer} -> {dependency}"
    return {
        "$message_type": "diagnostic",
        "message": message,
        "code": {"code": "unused_crate_dependencies", "explanation": None},
        "level": "error",
        "spans": [],
        "children": [],
        "rendered": "error: " + message + "\n",
    }


def run(argv: Sequence[str]) -> int:
    if not argv:
        print("rustc-first-party-unused-deps: missing compiler", file=sys.stderr)
        return 2
    compiler, raw_args = argv[0], list(argv[1:])
    temporary: List[Path] = []
    try:
        inspected = inspection_args(raw_args)
        consumer = configured_consumer(inspected)
        audit_consumer = (
            "--test" not in inspected
            and (consumer is None or consumer.startswith("root//crates/"))
        )
        externs = extern_artifacts(inspected)
        outputs = compiler_output_paths(inspected)
        args, temporary = invocation_args(raw_args)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"rustc-first-party-unused-deps: {error}", file=sys.stderr)
        return 2

    try:
        process = subprocess.Popen(
            [compiler] + args,
            stdout=None,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as error:
        print(f"rustc-first-party-unused-deps: {error}", file=sys.stderr)
        for path in temporary:
            try:
                path.unlink()
            except OSError:
                pass
        return 2
    assert process.stderr is not None
    forwarded_signals: List[int] = []

    def forward_signal(signum: int, _frame: object) -> None:
        if not forwarded_signals:
            forwarded_signals.append(signum)
        try:
            os.killpg(process.pid, signum)
        except ProcessLookupError:
            pass

    handled_signals = (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)
    previous_handlers = {
        signum: signal.signal(signum, forward_signal) for signum in handled_signals
    }
    policy_failed = False
    wrapper_failed = False
    try:
        for line in process.stderr:
            try:
                diagnostic = json.loads(line)
            except (UnicodeDecodeError, json.JSONDecodeError):
                sys.stderr.buffer.write(line)
                continue
            if not isinstance(diagnostic, dict):
                sys.stderr.buffer.write(line)
                continue
            try:
                filtered, findings = filter_unused(
                    diagnostic, consumer, externs, audit_consumer=audit_consumer
                )
            except ValueError as error:
                print(f"rustc-first-party-unused-deps: {error}", file=sys.stderr)
                wrapper_failed = True
                continue
            policy_failed = policy_failed or bool(findings)
            if filtered is not None:
                sys.stderr.write(json.dumps(filtered, separators=(",", ":")) + "\n")
            for finding in findings:
                sys.stderr.write(
                    json.dumps(policy_diagnostic(*finding), separators=(",", ":")) + "\n"
                )
        returncode = process.wait()
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    for path in temporary:
        try:
            path.unlink()
        except OSError:
            pass
    if forwarded_signals:
        received_signal = forwarded_signals[0]
        signal.signal(received_signal, signal.SIG_DFL)
        os.kill(os.getpid(), received_signal)
        return 128 + received_signal
    if returncode != 0:
        if returncode < 0:
            received_signal = -returncode
            signal.signal(received_signal, signal.SIG_DFL)
            os.kill(os.getpid(), received_signal)
            return 128 + received_signal
        return returncode
    if policy_failed or wrapper_failed:
        cleanup_errors = remove_compiler_outputs(outputs)
        if cleanup_errors:
            for error in cleanup_errors:
                print(
                    f"rustc-first-party-unused-deps: cannot remove compiler output: {error}",
                    file=sys.stderr,
                )
            return 2
    if wrapper_failed:
        return 2
    return 1 if policy_failed else 0


def main() -> int:
    return run(sys.argv[1:])


if __name__ == "__main__":
    raise SystemExit(main())
