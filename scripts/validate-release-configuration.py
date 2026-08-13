#!/usr/bin/env python3
"""Verify that Buck analyzes Rue's debug and release Rust flags correctly."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
TARGET = "//crates/rue:rue"
PLATFORMS = {
    "debug": "//platforms:debug",
    "release": "//platforms:release",
}
RELEASE_FLAGS = ("-Copt-level=3", "-Clto=thin", "--cfg=rue_release_build")
SCALING_WORKFLOW = ROOT / ".github" / "workflows" / "performance-scaling.yml"
COLLECTION_WORKFLOW = ROOT / ".github" / "workflows" / "performance-collect.yml"
SCALING_MANIFEST = ROOT / "performance" / "scaling.toml"
SCALING_RELEASE_SNIPPETS = (
    "./buck2 build //crates/rue:rue //crates/rue-bench:rue-bench --target-platforms //platforms:release",
    'RUE="$(scripts/rue-bin --target-platforms //platforms:release)"',
    'BENCH="$(./buck2 build //crates/rue-bench:rue-bench --target-platforms //platforms:release --show-simple-output',
    "--manifest performance/scaling.toml",
)
COLLECTION_BOUNDARY_SNIPPETS = (
    "--target-platforms //platforms:release",
    'RUE="$(scripts/rue-bin --target-platforms //platforms:release)"',
    "--manifest performance/manifest.toml",
    'if [ "$status" -eq 4 ]',
)
SCALING_BOUNDARY_SNIPPETS = (
    "schema_version = 3",
    'boundary = "fresh_source_to_native_v1"',
    'target = "x86-64-linux"',
    'args = []',
    'compiler_build_profile = "release_thin_lto"',
    'workers = ["one", "two", "four", "eight", "automatic"]',
)

def rustc_cfg_command(payload: str, platform: str) -> str:
    try:
        actions = json.loads(payload)
    except json.JSONDecodeError as error:
        raise ValueError(f"{platform} aquery returned invalid JSON: {error}") from error

    matches = [
        attributes.get("cmd", "")
        for key, attributes in actions.items()
        if "prelude//rust/tools:rustc_cfg" in key and f"root//platforms:{platform}#" in key
    ]
    if len(matches) != 1:
        raise ValueError(
            f"{platform} aquery exposed {len(matches)} matching rustc_cfg actions, expected 1"
        )
    return matches[0]


def validate_commands(debug: str, release: str) -> list[str]:
    errors: list[str] = []
    for flag in RELEASE_FLAGS:
        if flag not in release:
            errors.append(f"release rustc_cfg is missing {flag}")
        if flag in debug:
            errors.append(f"debug rustc_cfg unexpectedly contains {flag}")
    return errors


def validate_scaling_workflow(workflow: str) -> list[str]:
    return [
        f"compiler-scaling workflow is missing release command: {snippet}"
        for snippet in SCALING_RELEASE_SNIPPETS
        if snippet not in workflow
    ]


def validate_collection_workflow(workflow: str) -> list[str]:
    return [
        f"performance-collection workflow is missing boundary command: {snippet}"
        for snippet in COLLECTION_BOUNDARY_SNIPPETS
        if snippet not in workflow
    ]


def validate_scaling_manifest(manifest: str) -> list[str]:
    return [
        f"compiler-scaling manifest is missing boundary declaration: {snippet}"
        for snippet in SCALING_BOUNDARY_SNIPPETS
        if snippet not in manifest
    ]


def query(buck2: Path, platform: str) -> str:
    command = [
        str(buck2),
        "aquery",
        f"deps({TARGET})",
        "--target-platforms",
        PLATFORMS[platform],
        "--output-attribute",
        "^cmd$",
        "--json",
    ]
    result = subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return rustc_cfg_command(result.stdout, platform)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--buck2", type=Path, default=ROOT / "buck2")
    args = parser.parse_args()

    try:
        debug = query(args.buck2, "debug")
        release = query(args.buck2, "release")
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: could not inspect Buck release configuration: {error}", file=sys.stderr)
        return 1

    try:
        scaling_workflow = SCALING_WORKFLOW.read_text()
        collection_workflow = COLLECTION_WORKFLOW.read_text()
        scaling_manifest = SCALING_MANIFEST.read_text()
    except OSError as error:
        print(f"error: could not read performance policy input: {error}", file=sys.stderr)
        return 1

    errors = validate_commands(debug, release)
    errors.extend(validate_scaling_workflow(scaling_workflow))
    errors.extend(validate_collection_workflow(collection_workflow))
    errors.extend(validate_scaling_manifest(scaling_manifest))
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        "release configuration valid: release has "
        f"{' '.join(RELEASE_FLAGS)}; debug does not; collection and scaling pin the release boundary"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
