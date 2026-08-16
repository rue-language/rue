#!/usr/bin/env python3
"""Validate CLI hang guards and derive whole-suite executor deadlines.

The packing authority is the CLI harness itself: its
`RUE_CLI_EMIT_SHARD_LOADS` mode performs the real case discovery (tier and
platform filtering, `default_ms` fallback for unmeasured cases) and packs it
with the runtime LPT rule, and Buck materializes the per-platform result as
//:cli-shard-loads-json. This gate applies the declarative policy arithmetic
(multiplier, headroom, minimums) to those reported loads; it does not model
the corpus or the packing itself.

The result is correctness plumbing, not a performance threshold. Shard
deadlines deliberately combine measured expected cost with proportional and
fixed headroom so a loaded worker does not turn a healthy case into a flake.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path

PROFILE_NAMES = ("ordinary", "slow", "stress")
PROFILE_KEYS = {"compile_hang_timeout_ms", "runtime_hang_timeout_ms"}
POLICY_KEYS = {
    "expected_cost_multiplier_percent",
    "fixed_headroom_ms",
    "minimum_shard_timeout_ms",
    "minimum_monolith_timeout_ms",
    "minimum_slow_suite_timeout_ms",
}


def load_policy(path: Path) -> tuple[dict[str, dict[str, int]], dict[str, int]]:
    # The contracts are authored as TOML next to the CLI cases; the build
    # materializes this JSON twin via //crates/rue-toml2json so the gate runs
    # on the repository's Python 3.9 floor without `tomllib` (RUE-1524).
    data = json.loads(path.read_text())
    profiles = data.get("timeout_profile")
    policy = data.get("timeout_policy")
    if not isinstance(profiles, dict) or set(profiles) != set(PROFILE_NAMES):
        raise ValueError(f"{path}: timeout_profile must define exactly {', '.join(PROFILE_NAMES)}")
    for name in PROFILE_NAMES:
        profile = profiles[name]
        if not isinstance(profile, dict) or set(profile) != PROFILE_KEYS:
            raise ValueError(f"{path}: timeout_profile.{name} has ambiguous or missing fields")
        if any(type(profile[key]) is not int or profile[key] <= 0 for key in PROFILE_KEYS):
            raise ValueError(f"{path}: timeout_profile.{name} values must be positive integers")
    for phase in PROFILE_KEYS:
        values = [profiles[name][phase] for name in PROFILE_NAMES]
        if values != sorted(values) or len(set(values)) != len(values):
            raise ValueError(f"{path}: {phase} must increase from ordinary through stress")
    if not isinstance(policy, dict) or set(policy) != POLICY_KEYS:
        raise ValueError(f"{path}: timeout_policy has ambiguous or missing fields")
    if any(type(policy[key]) is not int or policy[key] <= 0 for key in POLICY_KEYS):
        raise ValueError(f"{path}: timeout_policy values must be positive integers")
    if policy["expected_cost_multiplier_percent"] < 100:
        raise ValueError(f"{path}: expected_cost_multiplier_percent must be at least 100")

    contracts = data.get("contract", {})
    for name, contract in contracts.items():
        if set(contract) != {"class", "timeout_profile"}:
            raise ValueError(
                f"{path}: contract.{name} must select class and timeout_profile; "
                "raw deadlines are forbidden"
            )
        if contract["timeout_profile"] not in profiles:
            raise ValueError(f"{path}: contract.{name} selects an unknown timeout profile")
    return profiles, policy


def load_shard_loads(path: Path) -> dict:
    """The harness-reported per-platform shard loads (//:cli-shard-loads-json)."""
    data = json.loads(path.read_text())
    if data.get("version") != 1:
        raise ValueError(f"{path}: unsupported shard-loads version")
    shard_count = data.get("shard_count")
    if type(shard_count) is not int or shard_count <= 0:
        raise ValueError(f"{path}: shard_count must be a positive integer")
    platforms = data.get("platforms")
    if not isinstance(platforms, dict) or not platforms:
        raise ValueError(f"{path}: platforms must be a non-empty object")
    for name, entry in platforms.items():
        loads = entry.get("loads_ms") if isinstance(entry, dict) else None
        if (
            not isinstance(loads, list)
            or len(loads) != shard_count
            or any(type(load) is not int or load < 0 for load in loads)
        ):
            raise ValueError(
                f"{path}: platforms.{name}.loads_ms must list {shard_count} "
                "non-negative integer loads"
            )
    return data


def platform_loads(loads: dict, platform_name: str) -> list[int]:
    platforms = loads["platforms"]
    if platform_name not in platforms:
        raise ValueError(
            f"shard loads do not model platform {platform_name!r} "
            f"(modeled: {', '.join(sorted(platforms))})"
        )
    return platforms[platform_name]["loads_ms"]


def derive_timeout_ms(expected_ms: int, minimum_ms: int, policy: dict[str, int]) -> int:
    scaled = math.ceil(
        expected_ms * policy["expected_cost_multiplier_percent"] / 100
    )
    return max(minimum_ms, scaled + policy["fixed_headroom_ms"])


def timeout_for_target(
    target: str, loads: dict | None, platform_name: str, policy: dict[str, int]
) -> tuple[int, int | None]:
    match = re.fullmatch(r"//:cli-tests-shard-(\d+)", target)
    if match:
        if loads is None:
            raise ValueError(f"{target} requires --shard-loads")
        index = int(match.group(1))
        count = loads["shard_count"]
        if index >= count:
            raise ValueError(f"invalid CLI shard index {index} (shard count {count})")
        expected = platform_loads(loads, platform_name)[index]
        return (
            derive_timeout_ms(expected, policy["minimum_shard_timeout_ms"], policy),
            expected,
        )
    if target == "//:cli-tests":
        if loads is None:
            raise ValueError(f"{target} requires --shard-loads")
        expected = sum(platform_loads(loads, platform_name))
        return (
            derive_timeout_ms(expected, policy["minimum_monolith_timeout_ms"], policy),
            expected,
        )
    if target == "//:cli-tests-slow":
        return policy["minimum_slow_suite_timeout_ms"], None
    raise ValueError(f"no CLI timeout policy for target {target}")


# RUE-1163: the corpus actions' outer bounds, as spelled in the root BUCK file.
# A corpus runs as a build action now, which gets no test-executor timeout, so
# `timeout_seconds` is the only thing that stops a wedged harness — and it must
# not cut inside the declarative correctness deadline this tool derives. Nothing
# connected the two until this check: RUE-1118 gave //:cli-tests a 1800s action
# bound while the policy allowed 3600s, so a healthy run could be killed.
BUCK_TIMEOUT_PATTERNS = {
    "//:cli-tests": re.compile(r"^_CLI_TESTS_TIMEOUT_SECONDS = (\d+)$", re.MULTILINE),
    "//:cli-tests-shard-0": re.compile(
        r"^_CLI_SHARD_TIMEOUT_SECONDS = (\d+)$", re.MULTILINE
    ),
    "//:cli-tests-slow": re.compile(
        r'name = "cli-tests-slow".*?timeout_seconds = (\d+)', re.DOTALL
    ),
}


def buck_timeouts(buck_path: Path) -> dict[str, int]:
    """The action bounds the root BUCK file declares, keyed by target."""
    text = buck_path.read_text()
    found = {}
    for target, pattern in BUCK_TIMEOUT_PATTERNS.items():
        match = pattern.search(text)
        if not match:
            raise ValueError(f"{buck_path}: no action timeout found for {target}")
        found[target] = int(match.group(1))
    return found


def check_buck_timeouts(
    buck_path: Path, loads: dict, policy: dict[str, int]
) -> list[str]:
    """Report action bounds that cut inside the derived correctness deadline.

    Every platform the shard-loads report models is checked, because the BUCK
    value is one static number while the derived deadline is per-platform: a
    bound that is generous on the fastest runner and short on the slowest is
    still a bound that kills healthy runs.
    """
    platforms = sorted(loads["platforms"])
    errors = []
    for target, declared_seconds in buck_timeouts(buck_path).items():
        for platform_name in platforms:
            required_ms, _ = timeout_for_target(target, loads, platform_name, policy)
            required_seconds = math.ceil(required_ms / 1000)
            if declared_seconds < required_seconds:
                errors.append(
                    f"{target}: BUCK bounds the corpus action at {declared_seconds}s, "
                    f"inside the {required_seconds}s correctness deadline the policy "
                    f"derives for {platform_name}. A healthy run would be killed."
                )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--policy",
        type=Path,
        required=True,
        help="JSON twin of execution_contracts.toml; build "
        "//:cli-execution-contracts-json or run this gate as "
        "//:cli-timeout-policy-validation",
    )
    parser.add_argument(
        "--shard-loads",
        type=Path,
        help="harness-reported per-platform shard loads; build "
        "//:cli-shard-loads-json (the harness's RUE_CLI_EMIT_SHARD_LOADS "
        "mode is the packing authority)",
    )
    parser.add_argument(
        "--buck",
        type=Path,
        help="root BUCK file whose corpus action bounds must cover the "
        "derived correctness deadlines",
    )
    args = parser.parse_args()
    try:
        _, policy = load_policy(args.policy)
        loads = (
            load_shard_loads(args.shard_loads)
            if args.shard_loads is not None
            else None
        )
        if args.buck is not None:
            if loads is None:
                raise ValueError("--buck requires --shard-loads")
            errors = check_buck_timeouts(args.buck, loads, policy)
            if errors:
                for error in errors:
                    print(f"error: {error}", file=sys.stderr)
                return 1
            print("CLI timeout policy valid; corpus action bounds cover it")
            return 0
        print("CLI timeout policy valid")
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
