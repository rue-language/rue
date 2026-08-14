#!/usr/bin/env python3
"""Validate CLI hang guards and derive whole-suite executor deadlines.

The result is correctness plumbing, not a performance threshold. Shard
deadlines deliberately combine measured expected cost with proportional and
fixed headroom so a loaded worker does not turn a healthy case into a flake.
"""

from __future__ import annotations

import argparse
import json
import math
import platform
import re
import sys
from pathlib import Path

# RUE-1509: `tomllib` is stdlib only from 3.11, and this target carries
# `rue_test_tier_premerge`, so below the floor the premerge suite died with
# `ModuleNotFoundError: No module named 'tomllib'` out of a timeout validator --
# a message naming neither the version nor the remedy. CI never saw it: its
# runners are 3.12 and newer. The repository floor is 3.11 for this import and
# nothing else; see AGENTS.md, "Repository tooling baseline".
if sys.version_info < (3, 11):
    raise SystemExit(
        "error: scripts/cli-timeout-policy.py requires Python 3.11 or newer "
        f"(this interpreter is {platform.python_version()} at {sys.executable}). "
        "It reads the CLI execution contracts with the stdlib `tomllib` module, "
        "added in 3.11. Install a 3.11+ interpreter and put it earlier on PATH; "
        "see AGENTS.md, \"Repository tooling baseline\"."
    )

import tomllib

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
    data = tomllib.loads(path.read_text())
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


def host_platform() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if system == "darwin":
        return "macos"
    if system == "linux" and machine in ("aarch64", "arm64"):
        return "linux-arm64"
    if system == "linux" and machine in ("x86_64", "amd64"):
        return "linux-x64"
    return "other"


def shard_loads(path: Path, platform_name: str, count: int) -> list[int]:
    data = json.loads(path.read_text())
    if data.get("version") != 1:
        raise ValueError(f"{path}: unsupported weights version")
    common = data.get("common", {})
    platform_weights = data.get("platforms", {}).get(platform_name, {})
    names = set(common) | set(platform_weights)
    weighted = [(name, platform_weights.get(name, common.get(name))) for name in names]
    if not weighted or any(type(weight) is not int or weight <= 0 for _, weight in weighted):
        raise ValueError(f"{path}: weights must be non-empty positive integers")
    weighted.sort(key=lambda item: (-item[1], item[0]))
    loads = [0] * count
    case_counts = [0] * count
    for _, weight in weighted:
        index = min(range(count), key=lambda i: (loads[i], case_counts[i], i))
        loads[index] += weight
        case_counts[index] += 1
    return loads


def derive_timeout_ms(expected_ms: int, minimum_ms: int, policy: dict[str, int]) -> int:
    scaled = math.ceil(
        expected_ms * policy["expected_cost_multiplier_percent"] / 100
    )
    return max(minimum_ms, scaled + policy["fixed_headroom_ms"])


def timeout_for_target(
    target: str, weights_path: Path, platform_name: str, policy: dict[str, int]
) -> tuple[int, int | None]:
    match = re.fullmatch(r"//:cli-tests-shard-(\d+)", target)
    if match:
        index = int(match.group(1))
        count = 4
        if index >= count:
            raise ValueError(f"invalid CLI shard index {index}")
        expected = shard_loads(weights_path, platform_name, count)[index]
        return (
            derive_timeout_ms(expected, policy["minimum_shard_timeout_ms"], policy),
            expected,
        )
    if target == "//:cli-tests":
        expected = sum(shard_loads(weights_path, platform_name, 1))
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
    buck_path: Path, weights_path: Path, policy: dict[str, int]
) -> list[str]:
    """Report action bounds that cut inside the derived correctness deadline.

    Every platform in the weights file is checked, because the BUCK value is one
    static number while the derived deadline is per-platform: a bound that is
    generous on the fastest runner and short on the slowest is still a bound
    that kills healthy runs.
    """
    platforms = sorted(json.loads(weights_path.read_text()).get("platforms", {}))
    errors = []
    for target, declared_seconds in buck_timeouts(buck_path).items():
        for platform_name in platforms:
            required_ms, _ = timeout_for_target(
                target, weights_path, platform_name, policy
            )
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
        default=Path("crates/rue-cli-tests/cases/execution_contracts.toml"),
    )
    parser.add_argument(
        "--weights", type=Path, default=Path("crates/rue-cli-tests/shard-weights.json")
    )
    parser.add_argument("--platform", default=host_platform())
    parser.add_argument("--target")
    parser.add_argument(
        "--buck",
        type=Path,
        help="root BUCK file whose corpus action bounds must cover the "
        "derived correctness deadlines",
    )
    args = parser.parse_args()
    try:
        _, policy = load_policy(args.policy)
        if args.target is None:
            if args.buck is not None:
                errors = check_buck_timeouts(args.buck, args.weights, policy)
                if errors:
                    for error in errors:
                        print(f"error: {error}", file=sys.stderr)
                    return 1
                print("CLI timeout policy valid; corpus action bounds cover it")
                return 0
            print("CLI timeout policy valid")
            return 0
        timeout_ms, expected_ms = timeout_for_target(
            args.target, args.weights, args.platform, policy
        )
        detail = (
            "fixed slow-suite guard"
            if expected_ms is None
            else f"expected={expected_ms}ms plus declarative headroom"
        )
        print(
            f"cli timeout policy: {args.target} {detail}; "
            f"correctness deadline={timeout_ms}ms (not a performance threshold)",
            file=sys.stderr,
        )
        print(math.ceil(timeout_ms / 1000))
        return 0
    except (OSError, ValueError, json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
